package metrics

import (
	"context"
	"database/sql"
	"errors"
	"math"
	"math/big"
	"math/rand/v2"
	"path/filepath"
	"reflect"
	"testing"
	"time"
)

func TestAggregateRoundsExactSumAcrossSealedBlocks(t *testing.T) {
	store, _ := openTestStore(t, Options{MaxHeadSamples: 1024})
	series := testSeries()
	points := make([]Sample, 481)
	for index := range points {
		points[index] = Sample{At: testEpoch + int64(index)}
	}
	points[0].Value = 1e16
	points[1].Value = 1
	points[240].Value = -1e16
	points[241].Value = 1
	if err := store.Ingest(t.Context(), []Batch{{Series: series, Samples: points}}); err != nil {
		t.Fatal(err)
	}
	sealed, err := store.Maintain(t.Context())
	if err != nil || sealed.SealedBlocks < 2 {
		t.Fatalf("seal sum blocks: %+v: %v", sealed, err)
	}
	result, err := store.Aggregate(t.Context(), AggregateRequest{
		Range: Range{Name: series.Name, Match: series.Labels, From: testEpoch, To: testEpoch + 481},
		Width: 481 * time.Millisecond, Op: AggregateSum,
	})
	if err != nil || len(result) != 1 || len(result[0].Buckets) != 1 {
		t.Fatalf("sum result: %+v: %v", result, err)
	}
	bucket := result[0].Buckets[0]
	if bucket.Count != 481 || bucket.Value != 2 || bucket.Overflow || bucket.From != testEpoch || bucket.To != testEpoch+481 {
		t.Fatalf("exact sum across blocks: %+v", bucket)
	}
}

func TestACounterStepCountsInTheBucketItEndsIn(t *testing.T) {
	store, _ := openTestStore(t, Options{MaxHeadSamples: 1024})
	series := testSeries()
	series.Kind = Counter
	points := make([]Sample, 242)
	for index := range points {
		points[index] = Sample{At: testEpoch + int64(index), Value: 110}
	}
	points[0].Value = 100
	points[239].Value = 120
	points[240].Value = 5
	points[241].Value = 20
	if err := store.Ingest(t.Context(), []Batch{{Series: series, Samples: points}}); err != nil {
		t.Fatal(err)
	}
	if sealed, err := store.Maintain(t.Context()); err != nil || sealed.SealedBlocks == 0 {
		t.Fatalf("seal counter block: %+v: %v", sealed, err)
	}
	all, err := store.Aggregate(t.Context(), AggregateRequest{
		Range: Range{Name: series.Name, Match: series.Labels, From: testEpoch, To: testEpoch + 242},
		Width: 242 * time.Millisecond, Op: AggregateIncrease,
	})
	if err != nil || len(all) != 1 || len(all[0].Buckets) != 1 || all[0].Buckets[0].Value != 40 || all[0].Buckets[0].Resets != 1 {
		t.Fatalf("counter transition: %+v: %v", all, err)
	}
	split, err := store.Aggregate(t.Context(), AggregateRequest{
		Range: Range{Name: series.Name, Match: series.Labels, From: testEpoch, To: testEpoch + 242},
		Width: 240 * time.Millisecond, Op: AggregateIncrease,
	})
	if err != nil || len(split) != 1 || len(split[0].Buckets) != 2 {
		t.Fatalf("split counter: %+v: %v", split, err)
	}
	first, second := split[0].Buckets[0], split[0].Buckets[1]
	if first.Value != 20 || second.Value != 20 || second.Resets != 1 || first.Value+second.Value != all[0].Buckets[0].Value {
		t.Fatalf("the step into the second bucket, a reset to 5, is not its own: %+v", split[0].Buckets)
	}
}

func TestAggregatePreservesSignedZeroAndRejectsInvalidInputs(t *testing.T) {
	store, _ := openTestStore(t, Options{})
	series := testSeries()
	points := []Sample{{At: testEpoch, Value: math.Copysign(0, -1)}, {At: testEpoch + 1, Value: 0}}
	if err := store.Ingest(t.Context(), []Batch{{Series: series, Samples: points}}); err != nil {
		t.Fatal(err)
	}
	request := AggregateRequest{Range: Range{Name: series.Name, Match: series.Labels, From: testEpoch, To: testEpoch + 2}, Width: 2 * time.Millisecond}
	for _, test := range []struct {
		op   AggregateOp
		bits uint64
	}{
		{AggregateMin, math.Float64bits(math.Copysign(0, -1))},
		{AggregateMax, math.Float64bits(0)},
		{AggregateSum, math.Float64bits(0)},
	} {
		request.Op = test.op
		result, err := store.Aggregate(t.Context(), request)
		if err != nil || len(result) != 1 || len(result[0].Buckets) != 1 || math.Float64bits(result[0].Buckets[0].Value) != test.bits {
			t.Fatalf("signed zero %s: %+v: %v", test.op, result, err)
		}
	}
	if err := store.Ingest(t.Context(), []Batch{{Series: series, Samples: []Sample{{At: testEpoch + 2, Value: math.Float64frombits(0x7ff8000000001234)}}}}); err != nil {
		t.Fatal(err)
	}
	request.Range.To = testEpoch + 3
	request.Width = 3 * time.Millisecond
	if _, err := store.Aggregate(t.Context(), request); !errors.Is(err, ErrNonFinite) {
		t.Fatalf("nonfinite aggregate: %v", err)
	}
	if _, err := store.Aggregate(t.Context(), AggregateRequest{Range: request.Range, Width: request.Width, Op: AggregateIncrease}); !errors.Is(err, ErrInvalid) {
		t.Fatalf("increase on gauge: %v", err)
	}
}

func TestAggregateOutputBudgetCountsBuckets(t *testing.T) {
	store, _ := openTestStore(t, Options{Limits: Limits{Series: 1, Blocks: 16, PayloadBytes: 1 << 20, DecodedSamples: 1000, OutputSamples: 1}})
	series := testSeries()
	points := testSamples(100)
	for index := range points {
		points[index].Value = 1
	}
	if err := store.Ingest(t.Context(), []Batch{{Series: series, Samples: points}}); err != nil {
		t.Fatal(err)
	}
	rangeRequest := Range{Name: series.Name, Match: series.Labels, From: testEpoch, To: testEpoch + 100}
	if _, err := store.Read(t.Context(), rangeRequest); !errors.Is(err, ErrLimit) {
		t.Fatalf("raw output limit: %v", err)
	}
	result, err := store.Aggregate(t.Context(), AggregateRequest{Range: rangeRequest, Width: 100 * time.Millisecond, Op: AggregateSum})
	if err != nil || len(result) != 1 || len(result[0].Buckets) != 1 || result[0].Buckets[0].Value != 100 {
		t.Fatalf("aggregate bucket budget: %+v: %v", result, err)
	}
}

func TestAggregateBucketEdgesAcrossSignedTimestampRange(t *testing.T) {
	start, end := bucketEdges(math.MinInt64+1, math.MaxInt64, math.MaxInt64-2, 10)
	if start != math.MaxInt64-4 || end != math.MaxInt64 {
		t.Fatalf("extreme bucket: [%d,%d)", start, end)
	}
}

func TestAggregateBucketEdgesEndTheLastBucketAtTo(t *testing.T) {
	for _, test := range []struct{ at, start, end int64 }{{7, 0, 10}, {23, 20, 25}} {
		if start, end := bucketEdges(0, 25, test.at, 10); start != test.start || end != test.end {
			t.Errorf("at %d: [%d, %d)", test.at, start, end)
		}
	}
}

func TestAggregateClipsRetentionBeforeSummingSealedEdges(t *testing.T) {
	store, _ := openTestStore(t, Options{Retention: time.Second, MaxHeadSamples: 1024})
	series := testSeries()
	points := make([]Sample, 481)
	for index := range points {
		points[index] = Sample{At: testEpoch + int64(index), Value: 1}
	}
	if err := store.Ingest(t.Context(), []Batch{{Series: series, Samples: points}}); err != nil {
		t.Fatal(err)
	}
	if _, err := store.Maintain(t.Context()); err != nil {
		t.Fatal(err)
	}
	store.now = func() time.Time { return time.UnixMilli(testEpoch + 1200) }
	result, err := store.Aggregate(t.Context(), AggregateRequest{
		Range: Range{Name: series.Name, Match: series.Labels, From: testEpoch, To: testEpoch + 481},
		Width: 481 * time.Millisecond, Op: AggregateSum,
	})
	if err != nil || len(result) != 1 || len(result[0].Buckets) != 1 {
		t.Fatalf("clipped sum: %+v: %v", result, err)
	}
	bucket := result[0].Buckets[0]
	if bucket.Count != 281 || bucket.Value != 281 || bucket.From != testEpoch || !bucket.Partial {
		t.Fatalf("retention changed aggregate: %+v", bucket)
	}
}

func TestAggregateMarksOnlyTheBucketRetentionCut(t *testing.T) {
	store, _ := openTestStore(t, Options{Retention: time.Second, MaxHeadSamples: 1024})
	series := testSeries()
	points := make([]Sample, 481)
	for index := range points {
		points[index] = Sample{At: testEpoch + int64(index), Value: 1}
	}
	if err := store.Ingest(t.Context(), []Batch{{Series: series, Samples: points}}); err != nil {
		t.Fatal(err)
	}
	store.now = func() time.Time { return time.UnixMilli(testEpoch + 1200) }
	type bucket struct {
		from    int64
		count   int
		partial bool
	}
	for _, test := range []struct {
		name        string
		from, width int64
		want        []bucket
	}{
		{"cutoff inside a bucket", 0, 150, []bucket{{150, 100, true}, {300, 150, false}, {450, 31, false}}},
		{"cutoff on a bucket edge", 0, 200, []bucket{{200, 200, false}, {400, 81, false}}},
		{"range after the cutoff", 250, 100, []bucket{{250, 100, false}, {350, 100, false}, {450, 31, false}}},
	} {
		result, err := store.Aggregate(t.Context(), AggregateRequest{
			Range: Range{Name: series.Name, Match: series.Labels, From: testEpoch + test.from, To: testEpoch + 481},
			Width: time.Duration(test.width) * time.Millisecond, Op: AggregateCount,
		})
		if err != nil || len(result) != 1 || len(result[0].Buckets) != len(test.want) {
			t.Fatalf("%s: %+v: %v", test.name, result, err)
		}
		for index, got := range result[0].Buckets {
			want := test.want[index]
			if got.From != testEpoch+want.from || got.Count != want.count || got.Partial != want.partial {
				t.Errorf("%s: bucket %d is %+v, want %+v", test.name, index, got, want)
			}
		}
	}
}

func TestAggregateHandlesCancellationOfOverflowAndSubnormals(t *testing.T) {
	store, _ := openTestStore(t, Options{})
	series := testSeries()
	values := []float64{math.MaxFloat64, math.MaxFloat64, -math.MaxFloat64, -math.MaxFloat64, math.SmallestNonzeroFloat64, math.SmallestNonzeroFloat64}
	points := make([]Sample, len(values))
	for index, value := range values {
		points[index] = Sample{At: testEpoch + int64(index), Value: value}
	}
	if err := store.Ingest(t.Context(), []Batch{{Series: series, Samples: points}}); err != nil {
		t.Fatal(err)
	}
	request := AggregateRequest{Range: Range{Name: series.Name, Match: series.Labels, From: testEpoch, To: testEpoch + int64(len(points))}, Width: time.Duration(len(points)) * time.Millisecond, Op: AggregateSum}
	result, err := store.Aggregate(t.Context(), request)
	if err != nil || len(result) != 1 || len(result[0].Buckets) != 1 {
		t.Fatalf("canceled overflow: %+v: %v", result, err)
	}
	if bucket := result[0].Buckets[0]; bucket.Overflow || math.Float64bits(bucket.Value) != math.Float64bits(2*math.SmallestNonzeroFloat64) {
		t.Fatalf("overflow cancellation lost subnormals: %+v", bucket)
	}
	request.Range.To = testEpoch + 2
	request.Width = 2 * time.Millisecond
	result, err = store.Aggregate(t.Context(), request)
	if err != nil || !result[0].Buckets[0].Overflow || !math.IsInf(result[0].Buckets[0].Value, 1) {
		t.Fatalf("finite sum overflow: %+v: %v", result, err)
	}
}

func TestAggregateCounterRejectsNegativeValuesAfterReopen(t *testing.T) {
	path := filepath.Join(t.TempDir(), fileName)
	store, err := openAt(t, path, Options{Retention: 100 * 365 * 24 * time.Hour})
	if err != nil {
		t.Fatal(err)
	}
	store.now = func() time.Time { return time.UnixMilli(testEpoch + 900) }
	series := testSeries()
	series.Kind = Counter
	if writeErr := store.Ingest(t.Context(), []Batch{{Series: series, Samples: []Sample{{At: testEpoch, Value: 1}, {At: testEpoch + 1, Value: -1}}}}); writeErr != nil {
		t.Fatal(writeErr)
	}
	if closeErr := store.runtime.Close(t.Context()); closeErr != nil {
		t.Fatal(closeErr)
	}
	reopened, err := openAt(t, path, Options{Retention: 100 * 365 * 24 * time.Hour})
	if err != nil {
		t.Fatal(err)
	}
	defer reopened.Close(context.Background())
	reopened.now = func() time.Time { return time.UnixMilli(testEpoch + 900) }
	_, err = reopened.Aggregate(t.Context(), AggregateRequest{Range: Range{Name: series.Name, Match: series.Labels, From: testEpoch, To: testEpoch + 2}, Width: 2 * time.Millisecond, Op: AggregateIncrease})
	if !errors.Is(err, ErrCounterValue) {
		t.Fatalf("negative counter aggregate after reopen: %v", err)
	}
	if writeErr := reopened.Ingest(t.Context(), []Batch{{Series: series, Samples: []Sample{{At: testEpoch + 2, Value: math.Float64frombits(0x7ff8000000001234)}}}}); writeErr != nil {
		t.Fatal(writeErr)
	}
	_, err = reopened.Aggregate(t.Context(), AggregateRequest{Range: Range{Name: series.Name, Match: series.Labels, From: testEpoch + 2, To: testEpoch + 3}, Width: time.Millisecond, Op: AggregateCount})
	if !errors.Is(err, ErrCounterValue) {
		t.Fatalf("nonfinite counter aggregate: %v", err)
	}
}

func TestExactAccumulatorMatchesRationalOracle(t *testing.T) {
	source := rand.New(rand.NewPCG(1, 2))
	for vector := range 100 {
		var exact big.Int
		var term big.Int
		rational := new(big.Rat)
		for range 20 {
			bits := source.Uint64()
			if bits>>52&0x7ff == 0x7ff {
				bits &^= 1 << 52
			}
			value := math.Float64frombits(bits)
			if err := finiteUnits(value, &term); err != nil {
				t.Fatal(err)
			}
			exact.Add(&exact, &term)
			rational.Add(rational, new(big.Rat).SetFloat64(value))
		}
		got, overflow := roundedExact(&exact)
		want, _ := rational.Float64()
		if math.Float64bits(got) != math.Float64bits(want) || overflow != math.IsInf(want, 0) {
			t.Fatalf("vector %d: got %v overflow=%t, want %v", vector, got, overflow, want)
		}
	}
}

// groupedStore holds three counters of one name, two routes, and two gauges.
func groupedStore(t *testing.T) *Store {
	t.Helper()
	store, _ := openTestStore(t, Options{Retention: time.Hour})
	ingest := func(series Series, values ...float64) {
		samples := make([]Sample, len(values))
		for i, value := range values {
			samples[i] = Sample{At: testEpoch + int64(i)*1000, Value: value}
		}
		if err := store.Ingest(t.Context(), []Batch{{Series: series, Samples: samples}}); err != nil {
			t.Fatal(err)
		}
	}
	requests := func(route, host string) Series {
		return Series{Name: "http_requests_total", Kind: Counter, Labels: Labels{"route": route, "host": host}}
	}
	ingest(requests("/a", "1"), 10, 15, 5, 8) // an increase of 5 + 8 after its reset
	ingest(requests("/a", "2"), 0, 2, 4, 6)
	ingest(requests("/b", "1"), 100, 100, 101, 103)
	ingest(Series{Name: "temperature", Kind: Gauge, Labels: Labels{"room": "a"}}, 0.1, 0.2, 0.3, 0.7)
	ingest(Series{Name: "temperature", Kind: Gauge, Labels: Labels{"room": "b"}}, 1, 2)
	return store
}

func aggregateOnce(t *testing.T, store *Store, name string, op AggregateOp, by, without []string) []AggregateResult {
	t.Helper()
	results, err := store.Aggregate(t.Context(), AggregateRequest{
		Range: Range{Name: name, From: testEpoch, To: testEpoch + 4000}, Width: 4 * time.Second, Op: op,
		By: by, Without: without,
	})
	if err != nil {
		t.Fatal(err)
	}
	return results
}

// A grouped aggregate joins its series' exact accumulators bucket by bucket and
// rounds once, so a group's increase counts each series' resets and its average
// weighs every sample.
func TestAGroupJoinsItsSeriesExactlyAndRoundsOnce(t *testing.T) {
	store := groupedStore(t)

	byRoute := aggregateOnce(t, store, "http_requests_total", AggregateIncrease, []string{"route"}, nil)
	if len(byRoute) != 2 || byRoute[0].Series.String() != `http_requests_total{route="/a"}` ||
		byRoute[0].Buckets[0].Value != 5+8+6 || byRoute[0].Buckets[0].Resets != 1 ||
		byRoute[1].Series.String() != `http_requests_total{route="/b"}` || byRoute[1].Buckets[0].Value != 3 {
		t.Fatalf("increase by route: %+v", byRoute)
	}
	withoutHost := aggregateOnce(t, store, "http_requests_total", AggregateIncrease, nil, []string{"host"})
	if len(withoutHost) != 2 || withoutHost[0].Buckets[0].Value != byRoute[0].Buckets[0].Value {
		t.Fatalf("increase without host is not increase by route: %+v", withoutHost)
	}
	total := aggregateOnce(t, store, "http_requests_total", AggregateCount, []string{}, nil)
	if len(total) != 1 || len(total[0].Series.Labels) != 0 || total[0].Buckets[0].Value != 12 {
		t.Fatalf("a count by nothing joins every series of the name: %+v", total)
	}

	// (0.1+0.2+0.3+0.7+1+2)/6, exact over every sample, rounded once
	avg := aggregateOnce(t, store, "temperature", AggregateAvg, []string{}, nil)
	sum := new(big.Rat)
	for _, value := range []float64{0.1, 0.2, 0.3, 0.7, 1, 2} {
		sum.Add(sum, new(big.Rat).SetFloat64(value))
	}
	want, _ := new(big.Rat).Quo(sum, big.NewRat(6, 1)).Float64()
	if len(avg) != 1 || math.Float64bits(avg[0].Buckets[0].Value) != math.Float64bits(want) {
		t.Fatalf("the average of every sample: %+v, want %v", avg, want)
	}
}

// rate is a counter's exact increase over its bucket's seconds; delta is a
// gauge's last sample less its first, each a series' own, then joined
//
//	increase 5+8 over a bucket of 4 s  →  rate 3.25
//	room a 0.1 → 0.7, room b 1 → 2     →  delta 0.6 and 1, by nothing 1.6
func TestRateAndDeltaAreExactPerSeriesThenJoined(t *testing.T) {
	store := groupedStore(t)
	rates := aggregateOnce(t, store, "http_requests_total", AggregateRate, nil, nil)
	if len(rates) != 3 || rates[0].Buckets[0].Value != 13.0/4 {
		t.Fatalf("rate of each series: %+v", rates)
	}
	deltas := aggregateOnce(t, store, "temperature", AggregateDelta, nil, nil)
	if len(deltas) != 2 || deltas[0].Buckets[0].Value != 0.6 || deltas[1].Buckets[0].Value != 1 {
		t.Fatalf("delta of each gauge: %+v", deltas)
	}
	sum, _ := new(big.Rat).Add(new(big.Rat).Sub(new(big.Rat).SetFloat64(0.7), new(big.Rat).SetFloat64(0.1)),
		big.NewRat(1, 1)).Float64()
	joined := aggregateOnce(t, store, "temperature", AggregateDelta, []string{}, nil)
	if len(joined) != 1 || joined[0].Buckets[0].Value != sum {
		t.Fatalf("delta joined exactly: %+v, want %v", joined, sum)
	}
}

func TestAGroupingOrOperationTheSeriesCannotTakeIsRefused(t *testing.T) {
	store := groupedStore(t)
	for _, request := range []AggregateRequest{
		{Range: Range{Name: "temperature"}, Op: AggregateRate},
		{Range: Range{Name: "http_requests_total"}, Op: AggregateDelta},
		{Range: Range{Name: "temperature"}, Op: AggregateSum, By: []string{"room"}, Without: []string{"host"}},
		{Range: Range{Name: "temperature"}, Op: AggregateSum, By: []string{"__name__"}},
		{Range: Range{Name: "temperature"}, Op: AggregateSum, Without: []string{""}},
	} {
		request.Range.From, request.Range.To, request.Width = testEpoch, testEpoch+4000, time.Second
		if _, err := store.Aggregate(t.Context(), request); !errors.Is(err, ErrInvalid) {
			t.Errorf("%v by %v without %v: %v, want ErrInvalid", request.Op, request.By, request.Without, err)
		}
	}
}

// An hour of a counter that grows by one a second, sampled every 15 s, is an
// increase of 3585 at every width. Buckets that dropped the step into them
// added up to 2700 at a minute and 3420 at five. A sample at the hour seals
// the hour before it into a block, which the hour's bucket answers from.
func TestAnHourOfACounterIncreasesBy3585AtEveryWidth(t *testing.T) {
	store, _ := openTestStore(t, Options{})
	hour := time.Hour.Milliseconds()
	store.now = func() time.Time { return time.UnixMilli(testEpoch + hour) }
	series := testSeries()
	series.Kind = Counter
	points := make([]Sample, 241)
	for i := range points {
		points[i] = Sample{At: testEpoch + int64(i)*15000, Value: float64(i * 15)}
	}
	if sealed := ingestAndSeal(t, store, Batch{Series: series, Samples: points}); sealed != 1 {
		t.Fatalf("the hour sealed into %d blocks", sealed)
	}

	for _, test := range []struct {
		width   time.Duration
		buckets int
	}{{time.Minute, 60}, {5 * time.Minute, 12}, {time.Hour, 1}} {
		buckets := aggregateBuckets(t, store, AggregateRequest{
			Range: Range{Name: series.Name, Match: series.Labels, From: testEpoch, To: testEpoch + hour},
			Width: test.width, Op: AggregateIncrease,
		})
		if len(buckets) != test.buckets || total(buckets) != 3585 {
			t.Fatalf("%s: %d buckets adding up to %v", test.width, len(buckets), total(buckets))
		}
	}
}

// The first bucket steps from the newest sample a lookback before the range,
// here the last of a block the range does not touch, which its summary answers
// undecoded: a block of 240 samples ending in 110 at 2390 ms, then 130 at 2500
// and 135 at 3500, read from 2400.
func TestTheFirstBucketTakesItsStepFromBeforeTheRange(t *testing.T) {
	store, _ := openTestStore(t, Options{})
	counter, gauge := testSeries(), testSeries()
	counter.Name, counter.Kind = "requests", Counter
	points := make([]Sample, 241)
	for i := range points {
		points[i] = Sample{At: testEpoch + int64(i)*10, Value: 100}
	}
	points[239].Value, points[240] = 110, Sample{At: testEpoch + 2500, Value: 130}
	sealed := ingestAndSeal(t, store, Batch{Series: counter, Samples: points}, Batch{Series: gauge, Samples: points})
	later := []Sample{{At: testEpoch + 3500, Value: 135}}
	if err := store.Ingest(t.Context(), []Batch{{Series: counter, Samples: later}, {Series: gauge, Samples: later}}); err != nil || sealed != 2 {
		t.Fatalf("a block a series: %d: %v", sealed, err)
	}

	stepped := func(from, to int64) AggregateBucket {
		return AggregateBucket{From: testEpoch + from, To: testEpoch + to, Count: 1, Value: 20, Lookback: true}
	}
	last := func(from, to int64) AggregateBucket {
		return AggregateBucket{From: testEpoch + from, To: testEpoch + to, Count: 1, Value: 5}
	}
	tests := []struct {
		width, lookback time.Duration
		want            []AggregateBucket
	}{
		{time.Second, 0, []AggregateBucket{stepped(2400, 3400), last(3400, 4400)}},
		{5 * time.Millisecond, 0, []AggregateBucket{ // 2390 is past a lookback of one width
			{From: testEpoch + 2500, To: testEpoch + 2505, Count: 1}, last(3500, 3505),
		}},
		{5 * time.Millisecond, time.Second, []AggregateBucket{stepped(2500, 2505), last(3500, 3505)}},
	}
	var asked []AggregateRequest
	for _, test := range tests {
		for _, series := range []Series{counter, gauge} {
			request := AggregateRequest{
				Range: Range{Name: series.Name, Match: series.Labels, From: testEpoch + 2400, To: testEpoch + 4400},
				Width: test.width, Op: AggregateIncrease, Lookback: test.lookback,
			}
			if series.Kind == Gauge {
				request.Op = AggregateDelta
			}
			if got := aggregateBuckets(t, store, request); !reflect.DeepEqual(got, test.want) {
				t.Fatalf("%s of %s buckets, a lookback of %s: got %+v, want %+v",
					request.Op, test.width, test.lookback, got, test.want)
			}
			asked = append(asked, request)
		}
	}

	zeroPayloads(t, store)
	for i, request := range asked {
		got, err := store.Aggregate(t.Context(), request)
		if err != nil || len(got) != 1 || !reflect.DeepEqual(got[0].Buckets, tests[i/2].want) {
			t.Fatalf("the step from a block before the range decoded it: %+v: %v", got, err)
		}
	}
}

// A lookback that retention cut leaves the step from an expired sample
// uncounted, and says so: with a minute's retention, 100 at 10 s, 110 at 50 s
// and 130 at 70 s, read at 60 s and again at 75 s, when 10 s has expired.
func TestAStepFromAnExpiredSampleIsNotCounted(t *testing.T) {
	store, _ := openTestStore(t, Options{Retention: time.Minute})
	store.now = func() time.Time { return time.UnixMilli(testEpoch + 60000) }
	series := testSeries()
	series.Kind = Counter
	ingestAndSeal(t, store, Batch{Series: series, Samples: []Sample{
		{At: testEpoch + 10000, Value: 100}, {At: testEpoch + 50000, Value: 110}, {At: testEpoch + 70000, Value: 130},
	}})
	request := AggregateRequest{
		Range: Range{Name: series.Name, Match: series.Labels, From: testEpoch + 20000, To: testEpoch + 80000},
		Width: 20 * time.Second, Op: AggregateIncrease,
	}

	want := []AggregateBucket{
		{From: testEpoch + 40000, To: testEpoch + 60000, Count: 1, Value: 10, Lookback: true},
		{From: testEpoch + 60000, To: testEpoch + 80000, Count: 1, Value: 20},
	}
	if got := aggregateBuckets(t, store, request); !reflect.DeepEqual(got, want) {
		t.Fatalf("a step from a live sample: got %+v, want %+v", got, want)
	}
	store.now = func() time.Time { return time.UnixMilli(testEpoch + 75000) }
	want[0].Value, want[0].Lookback, want[0].Partial = 0, false, true
	if got := aggregateBuckets(t, store, request); !reflect.DeepEqual(got, want) {
		t.Fatalf("a step from an expired sample: got %+v, want %+v", got, want)
	}
}

// FuzzIncreasesAndDeltasOfAdjacentBucketsAddUpToTheRange holds the step rule
// on any counter and gauge: buckets of any width add up to the whole range,
// and whatever summaries answer, raw samples answer alike.
func FuzzIncreasesAndDeltasOfAdjacentBucketsAddUpToTheRange(f *testing.F) {
	f.Add(uint64(1), uint16(999), uint32(59), uint32(0))
	f.Add(uint64(2), uint16(0), uint32(0), uint32(7))
	f.Add(uint64(3), uint16(29999), uint32(899999), uint32(4000000))
	f.Add(uint64(4), uint16(15000), uint32(60000), uint32(123456))
	f.Fuzz(func(t *testing.T, seed uint64, gap uint16, width, skip uint32) {
		store, _ := openTestStore(t, Options{})
		random := rand.New(rand.NewPCG(seed, seed))
		counter, gauge := testSeries(), testSeries()
		counter.Name, counter.Kind = "requests", Counter
		batches := []Batch{
			{Series: counter, Samples: steppedSamples(random, Counter, gap)},
			{Series: gauge, Samples: steppedSamples(random, Gauge, gap)},
		}
		lastOf := func(batch Batch) int64 { return batch.Samples[len(batch.Samples)-1].At }
		last := max(lastOf(batches[0]), lastOf(batches[1]))
		store.now = func() time.Time { return time.UnixMilli(last) }
		ingestHalvesSealingTheFirst(t, store, batches...)

		to := last + 1 + random.Int64N(1000)
		for _, batch := range batches {
			from := testEpoch + int64(skip)%(lastOf(batch)-testEpoch+1)
			whole := AggregateRequest{
				Range: Range{Name: batch.Series.Name, Match: batch.Series.Labels, From: from, To: to},
				Width: time.Duration(to-from) * time.Millisecond, Op: AggregateIncrease,
				Lookback: time.Duration(from-testEpoch+1) * time.Millisecond, // to the first sample
			}
			if batch.Series.Kind == Gauge {
				whole.Op = AggregateDelta
			}
			split := whole
			split.Width = time.Duration(1+int64(width)%(to-from)) * time.Millisecond
			checkStepsAddUp(t, aggregateBuckets(t, store, whole), aggregateBuckets(t, store, split))
		}
	})
}

// steppedSamples is a series of whole values, a counter's rising with a reset
// now and then, at most gap+1 ms apart from testEpoch on.
func steppedSamples(random *rand.Rand, kind Kind, gap uint16) []Sample {
	points := make([]Sample, 1+random.IntN(1000))
	at, value := testEpoch, float64(random.IntN(100))
	for i := range points {
		switch {
		case kind == Gauge:
			value = float64(random.IntN(2001) - 1000)
		case random.IntN(10) == 0:
			value = float64(random.IntN(20))
		default:
			value += float64(random.IntN(50))
		}
		points[i] = Sample{At: at, Value: value}
		at += 1 + random.Int64N(int64(gap)+1)
	}
	return points
}

// ingestHalvesSealingTheFirst leaves blocks and a head behind each series.
func ingestHalvesSealingTheFirst(t *testing.T, store *Store, batches ...Batch) {
	t.Helper()
	var first, second []Batch
	for _, batch := range batches {
		half := len(batch.Samples) / 2
		if half > 0 {
			first = append(first, Batch{Series: batch.Series, Samples: batch.Samples[:half]})
		}
		second = append(second, Batch{Series: batch.Series, Samples: batch.Samples[half:]})
	}
	if len(first) > 0 {
		ingestAndSeal(t, store, first...)
	}
	if err := store.Ingest(t.Context(), second); err != nil {
		t.Fatal(err)
	}
}

func checkStepsAddUp(t *testing.T, whole, split []AggregateBucket) {
	t.Helper()
	var count, resets int
	for _, bucket := range split {
		count, resets = count+bucket.Count, resets+bucket.Resets
		if bucket.Partial {
			t.Fatalf("a bucket no retention cut is partial: %+v", bucket)
		}
	}
	if len(whole) != 1 || total(split) != whole[0].Value || count != whole[0].Count || resets != whole[0].Resets ||
		split[0].Lookback != whole[0].Lookback {
		t.Fatalf("buckets %+v do not add up to the range's %+v", split, whole)
	}
}

// aggregateBuckets is one series' answer, which raw samples must give alike.
func aggregateBuckets(t *testing.T, store *Store, request AggregateRequest) []AggregateBucket {
	t.Helper()
	got, err := store.Aggregate(t.Context(), request)
	if err != nil {
		t.Fatal(err)
	}
	if want := rawAggregate(t, store, request); !reflect.DeepEqual(got, want) {
		t.Fatalf("summaries answered %+v, raw samples %+v", got, want)
	}
	if len(got) != 1 {
		t.Fatalf("results for one series: %+v", got)
	}
	return got[0].Buckets
}

func total(buckets []AggregateBucket) (sum float64) {
	for _, bucket := range buckets {
		sum += bucket.Value
	}
	return sum
}

// ingestAndSeal returns how many blocks the samples sealed.
func ingestAndSeal(t *testing.T, store *Store, batches ...Batch) int {
	t.Helper()
	if err := store.Ingest(t.Context(), batches); err != nil {
		t.Fatal(err)
	}
	sealed, err := store.Maintain(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	return sealed.SealedBlocks
}

// zeroPayloads leaves every block's summary and corrupts its samples.
func zeroPayloads(t *testing.T, store *Store) {
	t.Helper()
	if err := store.file.Update(t.Context(), func(tx *sql.Tx) error {
		_, err := tx.ExecContext(t.Context(), `update payloads set body=zeroblob(length(body))`)
		return err
	}); err != nil {
		t.Fatal(err)
	}
}
