package metrics

import (
	"context"
	"errors"
	"math"
	"math/big"
	"math/rand/v2"
	"path/filepath"
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
		Range: Range{Matchers: series.Labels, From: testEpoch, To: testEpoch + 481},
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

func TestAggregateCounterIncludesBlockTransitionButNotBucketTransition(t *testing.T) {
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
		Range: Range{Matchers: series.Labels, From: testEpoch, To: testEpoch + 242},
		Width: 242 * time.Millisecond, Op: AggregateIncrease,
	})
	if err != nil || len(all) != 1 || len(all[0].Buckets) != 1 || all[0].Buckets[0].Value != 40 || all[0].Buckets[0].Resets != 1 {
		t.Fatalf("counter transition: %+v: %v", all, err)
	}
	split, err := store.Aggregate(t.Context(), AggregateRequest{
		Range: Range{Matchers: series.Labels, From: testEpoch, To: testEpoch + 242},
		Width: 240 * time.Millisecond, Op: AggregateIncrease,
	})
	if err != nil || len(split) != 1 || len(split[0].Buckets) != 2 {
		t.Fatalf("split counter: %+v: %v", split, err)
	}
	if split[0].Buckets[0].Value != 20 || split[0].Buckets[1].Value != 15 || split[0].Buckets[1].Resets != 0 {
		t.Fatalf("bucket boundary changed counter increase: %+v", split[0].Buckets)
	}
}

func TestAggregatePreservesSignedZeroAndRejectsInvalidInputs(t *testing.T) {
	store, _ := openTestStore(t, Options{})
	series := testSeries()
	points := []Sample{{At: testEpoch, Value: math.Copysign(0, -1)}, {At: testEpoch + 1, Value: 0}}
	if err := store.Ingest(t.Context(), []Batch{{Series: series, Samples: points}}); err != nil {
		t.Fatal(err)
	}
	request := AggregateRequest{Range: Range{Matchers: series.Labels, From: testEpoch, To: testEpoch + 2}, Width: 2 * time.Millisecond}
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
	rangeRequest := Range{Matchers: series.Labels, From: testEpoch, To: testEpoch + 100}
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
		Range: Range{Matchers: series.Labels, From: testEpoch, To: testEpoch + 481},
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
			Range: Range{Matchers: series.Labels, From: testEpoch + test.from, To: testEpoch + 481},
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
	request := AggregateRequest{Range: Range{Matchers: series.Labels, From: testEpoch, To: testEpoch + int64(len(points))}, Width: time.Duration(len(points)) * time.Millisecond, Op: AggregateSum}
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
	path := filepath.Join(t.TempDir(), "aggregate.db")
	store, err := Open(t.Context(), path, Options{Retention: 100 * 365 * 24 * time.Hour})
	if err != nil {
		t.Fatal(err)
	}
	store.now = func() time.Time { return time.UnixMilli(testEpoch + 900) }
	series := testSeries()
	series.Kind = Counter
	if writeErr := store.Ingest(t.Context(), []Batch{{Series: series, Samples: []Sample{{At: testEpoch, Value: 1}, {At: testEpoch + 1, Value: -1}}}}); writeErr != nil {
		t.Fatal(writeErr)
	}
	if closeErr := store.Close(t.Context()); closeErr != nil {
		t.Fatal(closeErr)
	}
	reopened, err := Open(t.Context(), path, Options{Retention: 100 * 365 * 24 * time.Hour})
	if err != nil {
		t.Fatal(err)
	}
	defer reopened.Close(context.Background())
	reopened.now = func() time.Time { return time.UnixMilli(testEpoch + 900) }
	_, err = reopened.Aggregate(t.Context(), AggregateRequest{Range: Range{Matchers: series.Labels, From: testEpoch, To: testEpoch + 2}, Width: 2 * time.Millisecond, Op: AggregateIncrease})
	if !errors.Is(err, ErrCounterValue) {
		t.Fatalf("negative counter aggregate after reopen: %v", err)
	}
	if writeErr := reopened.Ingest(t.Context(), []Batch{{Series: series, Samples: []Sample{{At: testEpoch + 2, Value: math.Float64frombits(0x7ff8000000001234)}}}}); writeErr != nil {
		t.Fatal(writeErr)
	}
	_, err = reopened.Aggregate(t.Context(), AggregateRequest{Range: Range{Matchers: series.Labels, From: testEpoch + 2, To: testEpoch + 3}, Width: time.Millisecond, Op: AggregateCount})
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
