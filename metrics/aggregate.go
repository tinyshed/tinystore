package metrics

import (
	"context"
	"fmt"
	"math"
	"math/big"
	"time"
)

type bucketAccumulator struct {
	from, to                             int64
	count, resets                        int
	minimum, maximum, previousValue      float64
	previous, exact, current, difference big.Int
}

func finiteUnits(value float64, into *big.Int) error {
	bits := math.Float64bits(value)
	exponent := int((bits >> 52) & 0x7ff)
	if exponent == 0x7ff {
		return ErrNonFinite
	}
	mantissa := bits & ((1 << 52) - 1)
	if exponent != 0 {
		mantissa |= 1 << 52
	}
	into.SetUint64(mantissa)
	if exponent != 0 {
		into.Lsh(into, uint(exponent-1))
	}
	if bits>>63 != 0 {
		into.Neg(into)
	}
	return nil
}

func roundedExact(total *big.Int) (float64, bool) {
	if total.Sign() == 0 {
		return 0, false
	}
	value := new(big.Float).SetPrec(53).SetMode(big.ToNearestEven).SetInt(total)
	value.SetMantExp(value, -1074)
	rounded, _ := value.Float64()
	return rounded, math.IsInf(rounded, 0)
}

func (b *bucketAccumulator) add(value float64, op AggregateOp, kind Kind) error {
	if kind == Counter && (math.IsNaN(value) || math.IsInf(value, 0) || value < 0) {
		return ErrCounterValue
	}
	if err := finiteUnits(value, &b.current); err != nil {
		return err
	}
	if b.count == 0 {
		b.minimum, b.maximum = value, value
	} else {
		if value < b.minimum || value == 0 && b.minimum == 0 && math.Signbit(value) {
			b.minimum = value
		}
		if value > b.maximum || value == 0 && b.maximum == 0 && !math.Signbit(value) {
			b.maximum = value
		}
	}
	switch op {
	case AggregateSum:
		b.exact.Add(&b.exact, &b.current)
	case AggregateIncrease:
		if b.count > 0 {
			if value < b.previousValue {
				b.exact.Add(&b.exact, &b.current)
				b.resets++
			} else {
				b.difference.Sub(&b.current, &b.previous)
				b.exact.Add(&b.exact, &b.difference)
			}
		}
		b.previous.Set(&b.current)
		b.previousValue = value
	}
	b.count++
	return nil
}

func (b *bucketAccumulator) result(op AggregateOp) AggregateBucket {
	result := AggregateBucket{From: b.from, To: b.to, Count: b.count}
	switch op {
	case AggregateCount:
		result.Value = float64(b.count)
	case AggregateMin:
		result.Value = b.minimum
	case AggregateMax:
		result.Value = b.maximum
	case AggregateSum, AggregateIncrease:
		result.Value, result.Overflow = roundedExact(&b.exact)
		result.Resets = b.resets
	}
	return result
}

// bucketEdges is the bucket that holds at. Buckets are width long from from,
// except the last, which ends at to:
//
//	from 0, to 25, width 10    at 7 → [0, 10)    at 23 → [20, 25)
func bucketEdges(from, to, at, width int64) (int64, int64) {
	step := uint64(width) //nolint:gosec // width is a checked positive duration
	startOffset := distance(from, at) / step * step
	start := advance(from, startOffset)
	if step > distance(from, to)-startOffset {
		return start, to
	}
	return start, advance(start, step)
}

// Aggregate rounds exact finite arithmetic once and never uses legacy float64 block sums.
func (s *Store) Aggregate(ctx context.Context, request AggregateRequest) ([]AggregateResult, error) {
	release, err := s.admit(ctx, s.readSlots)
	if err != nil {
		return nil, err
	}
	defer release()

	query, err := s.checkAggregate(request)
	if err != nil {
		return nil, err
	}
	if query.from >= query.to {
		return []AggregateResult{}, nil
	}
	query.aggregate = &aggregateSelection{origin: request.Range.From, width: request.Width.Milliseconds()}

	unreserve, err := s.reserve(ctx, func() (int64, error) { return aggregateReservation(query.limits) })
	if err != nil {
		return nil, err
	}
	defer unreserve()

	reads, err := s.fetchSnapshot(ctx, query)
	if err != nil {
		return nil, err
	}

	aggregation := aggregation{
		store: s, op: request.Op, origin: request.Range.From, width: request.Width.Milliseconds(),
		from: query.from, to: query.to, limit: query.limits.OutputSamples,
	}
	results, err := aggregation.fold(ctx, reads)
	if err != nil {
		return nil, err
	}
	s.queried.Add(1)
	return results, nil
}

func (s *Store) checkAggregate(request AggregateRequest) (rangeQuery, error) {
	width := request.Width
	if request.Range.To < request.Range.From || width < time.Millisecond || width%time.Millisecond != 0 {
		return rangeQuery{}, fmt.Errorf("%w: aggregate range or bucket width", ErrInvalid)
	}
	switch request.Op {
	case AggregateCount, AggregateSum, AggregateMin, AggregateMax, AggregateIncrease:
	default:
		return rangeQuery{}, fmt.Errorf("%w: aggregate operation", ErrInvalid)
	}
	return s.checkRange(request.Range)
}

// aggregateReservation is a read's, plus the exact accumulator of each output bucket.
func aggregateReservation(limits Limits) (int64, error) {
	weight, err := readReservation(limits)
	if err != nil {
		return 0, err
	}
	extra, err := reservedMultiple(limits.OutputSamples, 64)
	if err != nil {
		return 0, err
	}
	accumulators, err := reservedMultiple(limits.Series, 2048)
	if err != nil {
		return 0, err
	}
	return reservation(weight, extra, accumulators)
}

// aggregation is one Aggregate call. Its buckets start at the requested From,
// while retention may have moved the start of the samples that count:
//
//	origin 0, width 481, retention from 200    samples 200…480 fill the bucket [0, 481), partial
type aggregation struct {
	store         *Store
	op            AggregateOp
	origin, width int64
	from, to      int64
	limit, output int
}

func (a *aggregation) fold(ctx context.Context, reads []seriesRead) ([]AggregateResult, error) {
	results := make([]AggregateResult, 0, len(reads))
	for _, read := range reads {
		if a.op == AggregateIncrease && read.series.kind != Counter {
			return nil, fmt.Errorf("%w: increase requires a counter series", ErrInvalid)
		}
		series := seriesBuckets{aggregation: a, kind: read.series.kind}
		if err := a.foldRead(ctx, read, &series); err != nil {
			return nil, err
		}
		if err := series.flush(); err != nil {
			return nil, err
		}
		if len(series.buckets) > 0 {
			labelled := Series{Labels: read.series.labels, Kind: read.series.kind}
			results = append(results, AggregateResult{Series: labelled, Buckets: series.buckets})
		}
	}
	return results, nil
}

// seriesBuckets folds one series' samples, which arrive in time order, into
// its buckets; the output limit counts buckets across every series.
type seriesBuckets struct {
	*aggregation
	kind        Kind
	current     bucketAccumulator
	previousAt  int64
	hasPrevious bool
	buckets     []AggregateBucket
}

func (b *seriesBuckets) add(point Sample) error {
	if point.At < b.from || point.At >= b.to {
		return nil
	}
	if b.hasPrevious && point.At <= b.previousAt {
		return fmt.Errorf("%w: overlapping samples", ErrCorrupt)
	}
	b.previousAt, b.hasPrevious = point.At, true

	start, end := bucketEdges(b.origin, b.to, point.At, b.width)
	if b.current.count > 0 && start != b.current.from {
		if err := b.flush(); err != nil {
			return err
		}
		b.current = bucketAccumulator{}
	}
	b.current.from, b.current.to = start, end
	if err := b.current.add(point.Value, b.op, b.kind); err != nil {
		return fmt.Errorf("aggregate sample at %d: %w", point.At, err)
	}
	return nil
}

func (b *seriesBuckets) flush() error {
	if b.current.count == 0 {
		return nil
	}
	if b.op == AggregateCount && uint64(b.current.count) > 1<<53 { //nolint:gosec // positive after the zero check
		return fmt.Errorf("%w: exact count representation", ErrLimit)
	}
	if b.output == b.limit {
		return fmt.Errorf("%w: aggregate output buckets", ErrLimit)
	}
	bucket := b.current.result(b.op)
	bucket.Partial = bucket.From < b.from
	b.buckets = append(b.buckets, bucket)
	b.output++
	return nil
}
