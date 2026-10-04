package metrics

import (
	"cmp"
	"context"
	"fmt"
	"maps"
	"math"
	"math/big"
	"slices"
	"strings"
	"time"
	"unicode/utf8"
)

// bucketAccumulator folds one bucket's samples exactly, in units of 2^-1074:
// exact is the sum, the increase, or once closed a delta's last less its
// first, which first and previous hold until then. A bucket that starts from
// the sample before it, stepFrom, counts the step into its first sample, so
// that the increases and deltas of adjacent buckets add up to the range's.
//
// First and last keep the sample itself in value, which a group of more than
// one series, joined, replaces with the exact sum of theirs.
type bucketAccumulator struct {
	from, to                                    int64
	count, resets, joined                       int
	minimum, maximum, previousValue             float64
	firstValue, value                           float64
	first, previous, exact, current, difference big.Int
	stepFrom, lookback, partial                 bool
}

// startFrom begins a bucket at the sample before its first, which a step
// into the bucket ends at: a delta is its last sample less that one
func (b *bucketAccumulator) startFrom(units *big.Int, value float64, beforeRange bool) {
	b.stepFrom, b.lookback = true, beforeRange
	b.previous.Set(units)
	b.first.Set(units)
	b.previousValue = value
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

// roundedQuotient is numerator over denominator, the numerator in units as
// roundedExact takes them, rounded once: an average, or a rate a second.
func roundedQuotient(numerator, denominator *big.Int) (float64, bool) {
	if numerator.Sign() == 0 {
		return 0, false
	}
	quotient := new(big.Float).SetPrec(53).SetMode(big.ToNearestEven)
	quotient.Quo(new(big.Float).SetInt(numerator), new(big.Float).SetInt(denominator))
	quotient.SetMantExp(quotient, -1074)
	rounded, _ := quotient.Float64()
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
	case AggregateSum, AggregateAvg:
		b.exact.Add(&b.exact, &b.current)
	case AggregateDelta:
		if b.count == 0 && !b.stepFrom {
			b.first.Set(&b.current)
		}
		b.previous.Set(&b.current)
	case AggregateIncrease, AggregateRate:
		if b.count > 0 || b.stepFrom {
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
	case AggregateFirst:
		if b.count == 0 {
			b.first.Set(&b.current)
			b.firstValue = value
		}
	case AggregateLast:
		b.previous.Set(&b.current)
		b.previousValue = value
	}
	b.count++
	return nil
}

// close ends a series' bucket: a delta is its last sample less its first.
func (b *bucketAccumulator) close(op AggregateOp) {
	switch op {
	case AggregateDelta:
		b.exact.Sub(&b.previous, &b.first)
	case AggregateFirst:
		b.exact.Set(&b.first)
		b.value = b.firstValue
	case AggregateLast:
		b.exact.Set(&b.previous)
		b.value = b.previousValue
	}
}

// join adds a closed bucket of another series of the group, exactly.
func (b *bucketAccumulator) join(other *bucketAccumulator) error {
	if other.count > math.MaxInt-b.count {
		return fmt.Errorf("%w: aggregate group count", ErrLimit)
	}
	if b.count == 0 {
		b.minimum, b.maximum = other.minimum, other.maximum
	} else {
		b.minimum, b.maximum = math.Min(b.minimum, other.minimum), math.Max(b.maximum, other.maximum)
	}
	if b.joined == 0 {
		b.value = other.value
	}
	b.joined++
	b.count += other.count
	b.resets += other.resets
	b.exact.Add(&b.exact, &other.exact)
	b.lookback = b.lookback || other.lookback
	b.partial = b.partial || other.partial
	return nil
}

func (b *bucketAccumulator) result(op AggregateOp) AggregateBucket {
	result := AggregateBucket{From: b.from, To: b.to, Count: b.count, Lookback: b.lookback, Partial: b.partial}
	switch op {
	case AggregateCount:
		result.Value = float64(b.count)
	case AggregateMin:
		result.Value = b.minimum
	case AggregateMax:
		result.Value = b.maximum
	case AggregateSum, AggregateIncrease, AggregateDelta:
		result.Value, result.Overflow = roundedExact(&b.exact)
		result.Resets = b.resets
	case AggregateAvg:
		result.Value, result.Overflow = roundedQuotient(&b.exact, big.NewInt(int64(b.count)))
	case AggregateRate:
		perSecond := new(big.Int).Mul(&b.exact, big.NewInt(1000))
		result.Value, result.Overflow = roundedQuotient(perSecond, big.NewInt(b.to-b.from))
		result.Resets = b.resets
	case AggregateFirst, AggregateLast:
		result.Value = b.value
		if b.joined > 1 {
			result.Value, result.Overflow = roundedExact(&b.exact)
		}
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
	from := query.from // withLookback moves the read's start before it
	steps := withLookback(request, &query)

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
		store: s, op: request.Op, origin: query.origin, width: request.Width.Milliseconds(),
		from: from, to: query.to, limit: query.limits.OutputSamples, by: request.By, without: request.Without,
		steps: steps,
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
	if width < time.Millisecond || width%time.Millisecond != 0 {
		return rangeQuery{}, fmt.Errorf("%w: aggregate bucket width", ErrInvalid)
	}
	if request.Lookback < 0 || request.Lookback%time.Millisecond != 0 {
		return rangeQuery{}, fmt.Errorf("%w: aggregate lookback", ErrInvalid)
	}
	switch request.Op {
	case AggregateCount, AggregateSum, AggregateMin, AggregateMax, AggregateAvg, AggregateIncrease, AggregateRate,
		AggregateDelta, AggregateFirst, AggregateLast:
	default:
		return rangeQuery{}, fmt.Errorf("%w: aggregate operation", ErrInvalid)
	}
	if err := checkGrouping(request.By, request.Without); err != nil {
		return rangeQuery{}, err
	}
	return s.checkRange(request.Range)
}

// withLookback selects what an aggregate reads, and widens the read to the
// newest sample a lookback before the range when the operation counts steps
// between samples: a bucket counts the step that ends in it, and the range's
// first bucket the step from the last sample before it. A lookback that
// retention cut leaves that step uncounted, and the bucket partial.
func withLookback(request AggregateRequest, query *rangeQuery) (steps *lookback) {
	query.aggregate = &aggregateSelection{origin: query.origin, width: request.Width.Milliseconds(), from: query.from}
	switch request.Op {
	case AggregateIncrease, AggregateRate, AggregateDelta:
	default:
		return nil
	}
	steps = &lookback{from: query.from}
	if query.from > query.origin { // retention cut the range itself
		return steps
	}
	back := cmp.Or(request.Lookback, request.Width).Milliseconds()
	start := query.origin - back
	if start > query.origin { // past the smallest time
		start = math.MinInt64
	}
	steps.from, steps.clipped = max(start, query.cutoff), start < query.cutoff
	query.from = steps.from
	query.aggregate.steps = true
	return steps
}

// lookback is where an aggregate's read begins before its range, and whether
// retention cut it
type lookback struct {
	from    int64
	clipped bool
}

// checkGrouping refuses By beside Without, and a label name a series cannot
// hold; the name is never a label to group by, since a group keeps one.
func checkGrouping(by, without []string) error {
	if by != nil && without != nil {
		return fmt.Errorf("%w: an aggregate groups by labels or without them, not both", ErrInvalid)
	}
	for _, name := range slices.Concat(by, without) {
		if name == "" || strings.HasPrefix(name, "__") || len(name) > maxLabelNameBytes || !utf8.ValidString(name) {
			return fmt.Errorf("%w: group by label %q", ErrInvalid, name)
		}
	}
	if len(by) > maxLabels || len(without) > maxLabels {
		return fmt.Errorf("%w: a grouping of more than %d labels", ErrInvalid, maxLabels)
	}
	return nil
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
	by, without   []string
	steps         *lookback // nil unless the operation counts steps between samples
}

func (a *aggregation) fold(ctx context.Context, reads []seriesRead) ([]AggregateResult, error) {
	grouped := a.by != nil || a.without != nil
	groups := map[string]*bucketGroup{}
	results := make([]AggregateResult, 0, len(reads))
	for _, read := range reads {
		if err := a.checkKind(read.series.kind); err != nil {
			return nil, err
		}
		series := seriesBuckets{aggregation: a, kind: read.series.kind}
		if grouped {
			series.group = a.groupOf(groups, publicSeries(read.series.labels, read.series.kind))
		}
		if err := a.foldRead(ctx, read, &series); err != nil {
			return nil, err
		}
		if err := series.flush(); err != nil {
			return nil, err
		}
		if len(series.buckets) > 0 {
			results = append(results, AggregateResult{
				Series: publicSeries(read.series.labels, read.series.kind), Buckets: series.buckets,
			})
		}
	}
	if grouped {
		return a.groupResults(groups)
	}
	return results, nil
}

// checkKind refuses an operation the series' kind has no meaning for.
func (a *aggregation) checkKind(kind Kind) error {
	switch {
	case (a.op == AggregateIncrease || a.op == AggregateRate) && kind != Counter:
		return fmt.Errorf("%w: %s requires a counter series", ErrInvalid, a.op)
	case a.op == AggregateDelta && kind != Gauge:
		return fmt.Errorf("%w: delta requires a gauge series; a counter's is its increase", ErrInvalid)
	}
	return nil
}

// bucketGroup is one result of a grouped aggregate: the series it stands for,
// and its buckets by their start, each joined from every series in it.
type bucketGroup struct {
	series  Series
	buckets map[int64]*bucketAccumulator
}

// groupOf is the group a series falls in: its name and kind, and the labels
// By keeps or Without leaves
//
//	By route   http_requests_total{host="a",route="/x"}  →  http_requests_total{route="/x"}
func (a *aggregation) groupOf(groups map[string]*bucketGroup, series Series) *bucketGroup {
	labels := Labels{}
	for name, value := range series.Labels {
		if a.by != nil && slices.Contains(a.by, name) || a.without != nil && !slices.Contains(a.without, name) {
			labels[name] = value
		}
	}
	member := Series{Name: series.Name, Kind: series.Kind, Labels: labels}
	key := string(member.Kind) + " " + member.String()
	if groups[key] == nil {
		groups[key] = &bucketGroup{series: member, buckets: map[int64]*bucketAccumulator{}}
	}
	return groups[key]
}

// join adds a series' closed bucket to the group's; a bucket new to the group
// counts against the output limit, as a series' bucket does ungrouped
func (g *bucketGroup) join(bucket *bucketAccumulator, a *aggregation) error {
	into, found := g.buckets[bucket.from]
	if !found {
		if a.output == a.limit {
			return limit(LimitOutputBuckets, a.output+1, a.limit)
		}
		a.output++
		into = &bucketAccumulator{from: bucket.from, to: bucket.to}
		g.buckets[bucket.from] = into
	}
	return into.join(bucket)
}

// groupResults rounds each group's buckets once, the groups in the order of
// their series' text, and each group's buckets in time order.
func (a *aggregation) groupResults(groups map[string]*bucketGroup) ([]AggregateResult, error) {
	results := make([]AggregateResult, 0, len(groups))
	for _, key := range slices.Sorted(maps.Keys(groups)) {
		group := groups[key]
		buckets := make([]AggregateBucket, 0, len(group.buckets))
		for _, start := range slices.Sorted(maps.Keys(group.buckets)) {
			joined := group.buckets[start]
			if a.op == AggregateCount && uint64(joined.count) > 1<<53 { //nolint:gosec // a count is positive
				return nil, fmt.Errorf("%w: exact count representation", ErrLimit)
			}
			bucket := joined.result(a.op)
			bucket.Partial = bucket.Partial || bucket.From < a.from
			buckets = append(buckets, bucket)
		}
		if len(buckets) > 0 {
			results = append(results, AggregateResult{Series: group.series, Buckets: buckets})
		}
	}
	return results, nil
}

// seriesBuckets folds one series' samples, which arrive in time order, into
// its buckets; the output limit counts buckets across every series. An
// operation that counts steps carries the last sample from bucket to bucket,
// the first from before the range.
type seriesBuckets struct {
	*aggregation
	kind        Kind
	current     bucketAccumulator
	previousAt  int64
	hasPrevious bool
	buckets     []AggregateBucket
	group       *bucketGroup // when grouped, its buckets join the group's rather than its own result
	flushed     int

	carried       bool
	carriedBefore bool // the carried sample is before the range
	carriedValue  float64
	carriedUnits  big.Int
}

func (b *seriesBuckets) add(point Sample) error {
	if point.At >= b.to || point.At < b.from && (b.steps == nil || point.At < b.steps.from) {
		return nil
	}
	if b.hasPrevious && point.At <= b.previousAt {
		return fmt.Errorf("%w: overlapping samples", ErrCorrupt)
	}
	b.previousAt, b.hasPrevious = point.At, true
	if point.At < b.from {
		return b.carry(point.Value, true)
	}

	start, end := bucketEdges(b.origin, b.to, point.At, b.width)
	if err := b.begin(start, end); err != nil {
		return err
	}
	if err := b.current.add(point.Value, b.op, b.kind); err != nil {
		return fmt.Errorf("aggregate sample at %d: %w", point.At, err)
	}
	return b.carry(point.Value, false)
}

// begin makes [start, end) the current bucket, flushing the one before it, and
// starts it from the carried sample when the operation counts steps
func (b *seriesBuckets) begin(start, end int64) error {
	if b.current.count > 0 && start != b.current.from {
		if err := b.flush(); err != nil {
			return err
		}
		b.current = bucketAccumulator{}
	}
	if b.current.count == 0 && !b.current.stepFrom && b.steps != nil && b.carried {
		b.current.startFrom(&b.carriedUnits, b.carriedValue, b.carriedBefore)
	}
	b.current.from, b.current.to = start, end
	return nil
}

// carry keeps the series' last sample for the next bucket's first step
func (b *seriesBuckets) carry(value float64, beforeRange bool) error {
	if b.steps == nil {
		return nil
	}
	if b.kind == Counter && (math.IsNaN(value) || math.IsInf(value, 0) || value < 0) {
		return ErrCounterValue
	}
	if err := finiteUnits(value, &b.carriedUnits); err != nil {
		return err
	}
	b.carried, b.carriedBefore, b.carriedValue = true, beforeRange, value
	return nil
}

func (b *seriesBuckets) flush() error {
	if b.current.count == 0 {
		return nil
	}
	b.current.close(b.op)
	// a first step a retention cut lookback could not find is not counted
	b.current.partial = b.current.from < b.from ||
		b.steps != nil && b.steps.clipped && b.flushed == 0 && !b.current.stepFrom
	b.flushed++
	if b.group != nil {
		return b.group.join(&b.current, b.aggregation)
	}
	if b.op == AggregateCount && uint64(b.current.count) > 1<<53 { //nolint:gosec // positive after the zero check
		return fmt.Errorf("%w: exact count representation", ErrLimit)
	}
	if b.output == b.limit {
		return limit(LimitOutputBuckets, b.output+1, b.limit)
	}
	b.buckets = append(b.buckets, b.current.result(b.op))
	b.output++
	return nil
}
