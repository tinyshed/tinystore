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

func bucketEdges(from, to, at, width int64) (int64, int64) {
	span := uint64(to) - uint64(from)                     //nolint:gosec // modular distance includes both signed timestamp extremes
	offset := uint64(at) - uint64(from)                   //nolint:gosec // modular distance stays nonnegative inside the range
	startOffset := offset / uint64(width) * uint64(width) //nolint:gosec // width is a checked positive duration
	start := int64(uint64(from) + startOffset)            //nolint:gosec // bucket start stays inside the signed range
	if uint64(width) > span-startOffset {                 //nolint:gosec // width is a checked positive duration
		return start, to
	}
	return start, int64(uint64(start) + uint64(width)) //nolint:gosec // bucket end stays below the requested bound
}

// Aggregate rounds exact finite arithmetic once and never uses legacy float64 block sums.
func (s *Store) Aggregate(ctx context.Context, request AggregateRequest) ([]AggregateResult, error) {
	if err := s.enter(ctx); err != nil {
		return nil, err
	}
	defer s.leave()
	select {
	case s.readSlots <- struct{}{}:
	case <-ctx.Done():
		return nil, ctx.Err()
	}
	defer func() { <-s.readSlots }()
	if request.Range.To < request.Range.From || request.Width < time.Millisecond || request.Width%time.Millisecond != 0 {
		return nil, fmt.Errorf("%w: aggregate range or bucket width", ErrInvalid)
	}
	switch request.Op {
	case AggregateCount, AggregateSum, AggregateMin, AggregateMax, AggregateIncrease:
	default:
		return nil, fmt.Errorf("%w: aggregate operation", ErrInvalid)
	}
	matchers, err := orderedLabels(request.Range.Matchers, false)
	if err != nil {
		return nil, err
	}
	limits, err := narrowLimits(request.Range.Limits, s.opts.Limits)
	if err != nil {
		return nil, err
	}
	from := max(request.Range.From, s.cutoff())
	if from >= request.Range.To {
		return []AggregateResult{}, nil
	}
	if s.opts.SharedBudget != nil {
		weight, reserveErr := readReservation(limits)
		if reserveErr != nil {
			return nil, reserveErr
		}
		extra, reserveErr := reservedMultiple(limits.OutputSamples, 48)
		if reserveErr != nil {
			return nil, reserveErr
		}
		weight, reserveErr = reservation(weight, extra)
		if reserveErr != nil {
			return nil, reserveErr
		}
		if reserveErr := s.opts.SharedBudget.acquire(ctx, weight); reserveErr != nil {
			return nil, reserveErr
		}
		defer s.opts.SharedBudget.release(weight)
	}
	reads, err := s.fetchSnapshot(ctx, matchers, from, request.Range.To, limits)
	if err != nil {
		return nil, err
	}
	results := make([]AggregateResult, 0, len(reads))
	outputCount := 0
	width := request.Width.Milliseconds()
	for _, read := range reads {
		if request.Op == AggregateIncrease && read.series.kind != Counter {
			return nil, fmt.Errorf("%w: increase requires a counter series", ErrInvalid)
		}
		result := AggregateResult{Series: Series{Labels: read.series.labels, Kind: read.series.kind}}
		var current bucketAccumulator
		var previousAt int64
		hasPrevious := false
		flush := func() error {
			if current.count == 0 {
				return nil
			}
			if request.Op == AggregateCount && uint64(current.count) > 1<<53 { //nolint:gosec // count is positive after the zero check
				return fmt.Errorf("%w: exact count representation", ErrLimit)
			}
			if outputCount == limits.OutputSamples {
				return fmt.Errorf("%w: aggregate output buckets", ErrLimit)
			}
			result.Buckets = append(result.Buckets, current.result(request.Op))
			outputCount++
			return nil
		}
		consume := func(point Sample) error {
			if point.At < from || point.At >= request.Range.To {
				return nil
			}
			if hasPrevious && point.At <= previousAt {
				return fmt.Errorf("%w: overlapping samples", ErrCorrupt)
			}
			previousAt, hasPrevious = point.At, true
			start, end := bucketEdges(request.Range.From, request.Range.To, point.At, width)
			if current.count > 0 && start != current.from {
				if err := flush(); err != nil {
					return err
				}
				current = bucketAccumulator{}
			}
			current.from, current.to = start, end
			if err := current.add(point.Value, request.Op, read.series.kind); err != nil {
				return fmt.Errorf("aggregate sample at %d: %w", point.At, err)
			}
			return nil
		}
		for _, block := range read.blocks {
			if err := ctx.Err(); err != nil {
				return nil, err
			}
			points, decodeErr := s.decodeBlock(block)
			if decodeErr != nil {
				return nil, fmt.Errorf("%w: decode values: %w", ErrCorrupt, decodeErr)
			}
			for _, point := range points {
				if err := consume(point); err != nil {
					return nil, err
				}
			}
		}
		head, decodeErr := s.decodeSelectedHead(ctx, read.head)
		if decodeErr != nil {
			return nil, decodeErr
		}
		for index, point := range head {
			if index%blockSamples == 0 {
				if err := ctx.Err(); err != nil {
					return nil, err
				}
			}
			if err := consume(point); err != nil {
				return nil, err
			}
		}
		if err := flush(); err != nil {
			return nil, err
		}
		if len(result.Buckets) > 0 {
			results = append(results, result)
		}
	}
	s.queried.Add(1)
	return results, nil
}
