package metrics

import (
	"errors"
	"fmt"
	"math/rand/v2"
	"testing"
	"time"
)

// A range bounds how stale a latest sample may be: a series that stopped
// before it is left out rather than read as its last value.
func TestLatestLeavesOutASeriesOlderThanItsRange(t *testing.T) {
	store, _ := openTestStore(t, Options{})
	now := testEpoch + time.Hour.Milliseconds()
	store.now = func() time.Time { return time.UnixMilli(now) }
	web, db := testSeries(), testSeries()
	db.Labels = Labels{"host": "db"}
	if err := store.Ingest(t.Context(), []Batch{
		{Series: web, Samples: []Sample{{At: now - 120_000, Value: 0.5}, {At: now - 60_000, Value: 0.75}}},
		{Series: db, Samples: []Sample{{At: now - 600_000, Value: 0.25}}},
	}); err != nil {
		t.Fatal(err)
	}

	recent, err := store.Latest(t.Context(), Range{Name: "cpu", Since: 5 * time.Minute})
	if err != nil || len(recent) != 1 || recent[0].Series.Labels["host"] != "one" ||
		len(recent[0].Samples) != 1 || recent[0].Samples[0] != (Sample{At: now - 60_000, Value: 0.75}) {
		t.Fatalf("the latest of five minutes: %+v: %v", recent, err)
	}
	both, err := store.Latest(t.Context(), Range{Name: "cpu", Since: 15 * time.Minute})
	if err != nil || len(both) != 2 {
		t.Fatalf("the latest of fifteen minutes: %+v: %v", both, err)
	}
	before, err := store.Latest(t.Context(), Range{Name: "cpu", From: now - 900_000, To: now - 90_000})
	if err != nil || len(before) != 2 || before[1].Samples[0] != (Sample{At: now - 120_000, Value: 0.5}) {
		t.Fatalf("the latest before To: %+v: %v", before, err)
	}
}

// Latest decodes the newest head chunk the range touches, alone, and answers
// from a block's directory when the range ends after the block: 1000 samples
// in the head are five chunks, and a sealed block's payload is never read.
func TestLatestDecodesAtMostOneChunkASeries(t *testing.T) {
	store, _ := openTestStore(t, Options{})
	series := testSeries()
	points := make([]Sample, 1000)
	for i := range points {
		points[i] = Sample{At: testEpoch + int64(i), Value: float64(i)}
	}
	if err := store.Ingest(t.Context(), []Batch{{Series: series, Samples: points}}); err != nil {
		t.Fatal(err)
	}
	bounded := Range{Name: "cpu", From: testEpoch, Limits: Limits{DecodedSamples: blockSamples}}
	if _, err := store.Read(t.Context(), bounded); !errors.Is(err, ErrLimit) {
		t.Fatalf("a read of the whole head within one chunk's samples: %v", err)
	}
	latest, err := store.Latest(t.Context(), bounded)
	if err != nil || len(latest) != 1 || latest[0].Samples[0] != points[999] {
		t.Fatalf("the latest of a head of five chunks: %+v: %v", latest, err)
	}

	sealed, err := store.Maintain(t.Context())
	if err != nil || sealed.SealedBlocks != 4 {
		t.Fatalf("sealing four blocks: %+v: %v", sealed, err)
	}
	zeroPayloads(t, store)
	bounded.To = testEpoch + 900 // the last block ends at 959
	if _, err = store.Latest(t.Context(), bounded); !errors.Is(err, ErrCorrupt) {
		t.Fatalf("a block the range cuts was not decoded: %v", err)
	}
	bounded.To, bounded.Limits.DecodedSamples = testEpoch+960, 1 // after the last block, before the head
	latest, err = store.Latest(t.Context(), bounded)
	if err != nil || len(latest) != 1 || latest[0].Samples[0] != points[959] {
		t.Fatalf("the latest from a block's directory: %+v: %v", latest, err)
	}
}

// Latest is the last sample a read of the same range returns, a series at a
// time and for the batches many series are fetched in, wherever the range ends.
func TestLatestIsTheLastSampleOfARead(t *testing.T) {
	for _, count := range []int{3, batchedSeries + 4} {
		store, _ := openTestStore(t, Options{})
		random := rand.New(rand.NewPCG(uint64(count), 7))
		batches := make([]Batch, count)
		for i := range batches {
			series := testSeries()
			series.Labels = Labels{"host": fmt.Sprintf("web-%02d", i)}
			batches[i] = Batch{Series: series, Samples: steppedSamples(random, Gauge, 99)}
		}
		store.now = func() time.Time { return time.UnixMilli(testEpoch + 100_000) }
		ingestHalvesSealingTheFirst(t, store, batches...)

		for range 20 {
			from := testEpoch + random.Int64N(50_000)
			request := Range{Name: "cpu", From: from, To: from + 1 + random.Int64N(60_000)}
			read, err := store.Read(t.Context(), request)
			if err != nil {
				t.Fatal(err)
			}
			latest, err := store.Latest(t.Context(), request)
			if err != nil || len(latest) != len(read) {
				t.Fatalf("%d series in %+v: latest %d, read %d: %v", count, request, len(latest), len(read), err)
			}
			for i, result := range read {
				if want := result.Samples[len(result.Samples)-1]; latest[i].Samples[0] != want ||
					latest[i].Series.String() != result.Series.String() {
					t.Fatalf("%s in %+v: latest %+v, the read's last %+v", result.Series, request, latest[i], want)
				}
			}
		}
	}
}
