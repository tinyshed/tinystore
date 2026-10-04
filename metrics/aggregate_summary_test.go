package metrics

import (
	"context"
	"database/sql"
	"errors"
	"math"
	"reflect"
	"testing"
	"time"

	"github.com/tinyshed/tinystore"
)

func sealedAggregateStore(t *testing.T, kind Kind, values []float64) (*Store, Series) {
	t.Helper()
	store, _ := openTestStore(t, Options{MaxHeadSamples: 4096})
	series := testSeries()
	series.Kind = kind
	points := make([]Sample, len(values))
	for i, value := range values {
		points[i] = Sample{At: testEpoch + int64(i), Value: value}
	}
	if err := store.Ingest(t.Context(), []Batch{{Series: series, Samples: points}}); err != nil {
		t.Fatal(err)
	}
	if _, err := store.Maintain(t.Context()); err != nil {
		t.Fatal(err)
	}
	return store, series
}

func TestWholeExactBlocksNeedNoDecodedSampleBudget(t *testing.T) {
	values := make([]float64, 961)
	for i := range values {
		values[i] = float64(i%23) / 10
	}
	store, series := sealedAggregateStore(t, Gauge, values)
	request := AggregateRequest{Range: Range{
		Name: series.Name, Match: series.Labels, From: testEpoch, To: testEpoch + 961,
		Limits: Limits{DecodedSamples: 1},
	}, Width: time.Second, Op: AggregateSum}
	got, err := store.Aggregate(t.Context(), request)
	if err != nil {
		t.Fatal(err)
	}
	if len(got) != 1 || len(got[0].Buckets) != 1 || got[0].Buckets[0].Count != len(values) {
		t.Fatalf("result: %+v", got)
	}
	var reached *tinystore.LimitError
	if _, err = store.Read(t.Context(), request.Range); !errors.As(err, &reached) || !errors.Is(err, ErrLimit) ||
		reached.Name != "decoded samples" || reached.Bound != 1 || reached.Wanted <= 1 {
		t.Fatalf("a read past its decoded samples: %v", err)
	}
	if _, err = store.Read(t.Context(), request.Range); !errors.Is(err, ErrLimit) {
		t.Fatalf("raw ignored budget: %v", err)
	}
}

func TestWholeSummarySkipsPayloadButPartialBlocksCheckIt(t *testing.T) {
	values := make([]float64, 481)
	for i := range values {
		values[i] = float64(i%37) * 0.125
	}
	store, series := sealedAggregateStore(t, Gauge, values)
	if err := store.file.Update(t.Context(), func(tx *sql.Tx) error {
		_, err := tx.ExecContext(t.Context(), `update payloads set body=zeroblob(length(body))`)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	request := AggregateRequest{Range: Range{Name: series.Name, Match: series.Labels, From: testEpoch, To: testEpoch + 481}, Width: time.Second, Op: AggregateSum}
	if _, err := store.Aggregate(t.Context(), request); err != nil {
		t.Fatalf("summary fetched unused raw: %v", err)
	}
	request.Range.From++
	if _, err := store.Aggregate(t.Context(), request); !errors.Is(err, ErrCorrupt) {
		t.Fatalf("partial did not check raw: %v", err)
	}
}

func TestSummaryAndRawAggregatesAgreeAtEveryBoundary(t *testing.T) {
	for _, kind := range []Kind{Gauge, Counter} {
		values := make([]float64, 1441)
		for i := range values {
			values[i] = float64((i*173)%211) * 0.1
		}
		if kind == Gauge {
			values[12], values[13], values[14] = math.MaxFloat64, math.MaxFloat64, -math.MaxFloat64
			values[500], values[501] = math.Copysign(0, -1), 0
		}
		store, series := sealedAggregateStore(t, kind, values)
		for _, op := range []AggregateOp{
			AggregateCount, AggregateSum, AggregateMin, AggregateMax, AggregateAvg, AggregateIncrease, AggregateRate,
			AggregateDelta, AggregateFirst, AggregateLast,
		} {
			if (op == AggregateIncrease || op == AggregateRate) && kind != Counter || op == AggregateDelta && kind != Gauge {
				continue
			}
			for _, bounds := range [][3]int64{{0, 1441, 2000}, {1, 1440, 480}, {240, 1200, 240}, {239, 1201, 481}, {0, 961, 500}} {
				request := AggregateRequest{
					Range: Range{Name: series.Name, Match: series.Labels, From: testEpoch + bounds[0], To: testEpoch + bounds[1]},
					Width: time.Duration(bounds[2]) * time.Millisecond, Op: op,
				}
				got, err := store.Aggregate(t.Context(), request)
				if err != nil {
					t.Fatal(err)
				}
				want := rawAggregate(t, store, request)
				if !reflect.DeepEqual(got, want) {
					t.Fatalf("kind %v op %v bounds %v: got %+v want %+v", kind, op, bounds, got, want)
				}
				for i := range got {
					for j := range got[i].Buckets {
						if math.Float64bits(got[i].Buckets[j].Value) != math.Float64bits(want[i].Buckets[j].Value) {
							t.Fatal("aggregate bits changed")
						}
					}
				}
			}
		}
	}
}

func rawAggregate(t *testing.T, store *Store, request AggregateRequest) []AggregateResult {
	t.Helper()
	query, err := store.checkAggregate(request)
	if err != nil {
		t.Fatal(err)
	}
	from := query.from
	steps := withLookback(request, &query)
	query.aggregate = nil // every block decoded, none answered from its summary
	reads, err := store.fetchSnapshot(context.Background(), query)
	if err != nil {
		t.Fatal(err)
	}
	a := aggregation{
		store: store, op: request.Op, origin: request.Range.From, width: request.Width.Milliseconds(),
		from: from, to: query.to, limit: query.limits.OutputSamples, steps: steps,
	}
	result, err := a.fold(t.Context(), reads)
	if err != nil {
		t.Fatal(err)
	}
	return result
}

func TestLargeExactSummariesStayWithinDirectoryBounds(t *testing.T) {
	store, _ := openTestStore(t, Options{MaxHeadSamples: 12000, MaxBatchSamples: 12000})
	points := make([]Sample, 9000)
	for i := range points {
		value := math.SmallestNonzeroFloat64
		if i%2 == 0 {
			value = math.MaxFloat64
		}
		points[i] = Sample{At: testEpoch + int64(i), Value: value}
	}
	series := testSeries()
	if err := store.Ingest(t.Context(), []Batch{{Series: series, Samples: points}}); err != nil {
		t.Fatal(err)
	}
	for range 4 {
		if _, err := store.Maintain(t.Context()); err != nil {
			t.Fatal(err)
		}
	}
	request := AggregateRequest{
		Range: Range{Name: series.Name, Match: series.Labels, From: testEpoch, To: testEpoch + 9000},
		Width: 9 * time.Second, Op: AggregateSum,
	}
	got, err := store.Aggregate(t.Context(), request)
	if err != nil {
		t.Fatal(err)
	}
	want := rawAggregate(t, store, request)
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("large summaries changed results: %+v %+v", got, want)
	}
}
