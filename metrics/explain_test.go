package metrics

import (
	"testing"
	"time"
)

// A plan is what the call then spends: the same series, blocks, summaries,
// bytes and samples, found without a payload fetched or a sample decoded.
func TestAPlanSaysWhatTheCallThenSpends(t *testing.T) {
	values := make([]float64, 1441)
	for i := range values {
		values[i] = float64(i%17) / 10
	}
	store, series := sealedAggregateStore(t, Gauge, values)
	for _, width := range []time.Duration{240 * time.Millisecond, 2 * time.Second} {
		request := AggregateRequest{
			Range: Range{Name: series.Name, Match: series.Labels, From: testEpoch, To: testEpoch + 1441},
			Width: width, Op: AggregateSum,
		}
		plan, err := store.ExplainAggregate(t.Context(), request)
		if err != nil || plan.Stops != nil {
			t.Fatalf("plan: %+v, %v", plan, err)
		}
		query, err := store.checkAggregate(request)
		if err != nil {
			t.Fatal(err)
		}
		query.aggregate = &aggregateSelection{origin: query.origin, width: width.Milliseconds()}
		_, spent, err := store.fetchSnapshotSpending(t.Context(), query)
		if err != nil {
			t.Fatal(err)
		}
		if plan.Series != spent.series || plan.Blocks != spent.blocks || plan.Summarized != spent.summarized ||
			plan.PayloadBytes != spent.bytes || plan.DecodedSamples != spent.decoded || plan.Series != 1 {
			t.Fatalf("width %v: planned %+v, spent %+v", width, plan, spent)
		}
	}

	read, err := store.ExplainRead(t.Context(), Range{
		Name: series.Name, Match: series.Labels, From: testEpoch, To: testEpoch + 1441, Limits: Limits{DecodedSamples: 10},
	})
	if err != nil || read.Stops == nil || read.Stops.Name != "decoded samples" || read.Stops.Bound != 10 {
		t.Fatalf("a read past its decoded samples: %+v, %v", read, err)
	}
	nothing, err := store.ExplainRead(t.Context(), Range{Name: "absent", From: testEpoch, To: testEpoch + 1})
	if err != nil || nothing.Series != 0 || nothing.Stops != nil {
		t.Fatalf("a plan of no series: %+v, %v", nothing, err)
	}
}
