package metrics

import (
	"bytes"
	"context"
	"database/sql"
	"errors"
	"math"
	"testing"
)

func TestLongPackedHeadAppendKeepsExactBitsAndFrontier(t *testing.T) {
	store, _ := openTestStore(t, Options{MaxHeadSamples: 4096})
	series := testSeries()
	seed := testSamples(2000)
	if err := store.Ingest(t.Context(), []Batch{{Series: series, Samples: seed}}); err != nil {
		t.Fatal(err)
	}
	keptBefore := func(at int64) ([]byte, int) {
		head := headSnapshot{seriesID: 1}
		if err := store.file.View(t.Context(), func(tx *sql.Tx) error {
			return tx.QueryRowContext(t.Context(), `select tail,head_count,head_start,head_end from series_state where series_id=1`).
				Scan(&head.packed, &head.count, &head.start, &head.end)
		}); err != nil {
			t.Fatal(err)
		}
		chunks, err := store.parseHead(head)
		if err != nil {
			t.Fatal(err)
		}
		kept := reusableChunks(chunks, at)
		return storedBytes(kept), countChunkSamples(kept)
	}
	incoming := Sample{At: testEpoch + 2000, Value: math.Float64frombits(0x7ff8000000004321)}
	prefix, count := keptBefore(incoming.At)
	if count != 1920 {
		t.Fatalf("reusable prefix: %d", count)
	}
	if err := store.Ingest(t.Context(), []Batch{{Series: series, Samples: []Sample{incoming}}}); err != nil {
		t.Fatal(err)
	}
	kept, keptCount := keptBefore(incoming.At)
	if keptCount != count || !bytes.Equal(prefix, kept) {
		t.Fatalf("encoded prefix changed: %d", keptCount)
	}
	var headCount, ready int
	var first, last, maxSeen int64
	if err := store.file.View(t.Context(), func(tx *sql.Tx) error {
		return tx.QueryRowContext(t.Context(), `select head_count,head_start,head_end,max_seen_ts,ready from series_state where series_id=1`).Scan(&headCount, &first, &last, &maxSeen, &ready)
	}); err != nil {
		t.Fatal(err)
	}
	if headCount != 2001 || first != testEpoch || last != incoming.At || maxSeen != incoming.At || ready != 1 {
		t.Fatalf("head state count=%d first=%d last=%d max_seen=%d ready=%d", headCount, first, last, maxSeen, ready)
	}
	want := append(seed, incoming)
	assertRead := func() {
		results, err := store.Read(t.Context(), Range{Matchers: series.Labels, From: testEpoch, To: incoming.At + 1})
		if err != nil || len(results) != 1 || len(results[0].Samples) != len(want) {
			t.Fatalf("read appended head: results=%d error=%v", len(results), err)
		}
		assertSamples(t, results[0].Samples, want)
	}
	assertRead()
	result, err := store.Maintain(t.Context())
	if err != nil || result.SealedBlocks == 0 {
		t.Fatalf("seal appended head: %+v: %v", result, err)
	}
	assertRead()
}

func TestCanceledIngestDoesNotCreateASeries(t *testing.T) {
	store, _ := openTestStore(t, Options{})
	ctx, cancel := context.WithCancel(t.Context())
	cancel()
	if err := store.Ingest(ctx, []Batch{{Series: testSeries(), Samples: testSamples(1)}}); !errors.Is(err, context.Canceled) {
		t.Fatalf("canceled ingest: %v", err)
	}
	if got := readAll(t, store); len(got) != 0 {
		t.Fatal("canceled ingest committed")
	}
}
