package metrics

import (
	"bytes"
	"database/sql"
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
	readTail := func() []byte {
		var packed []byte
		if err := store.file.View(t.Context(), func(tx *sql.Tx) error {
			return tx.QueryRowContext(t.Context(), `select tail from series_state where series_id=1`).Scan(&packed)
		}); err != nil {
			t.Fatal(err)
		}
		return packed
	}
	before := readTail()
	incoming := Sample{At: testEpoch + 2000, Value: math.Float64frombits(0x7ff8000000004321)}
	prefix, count, prefixErr := reusableHeadPrefix(before, incoming.At, store.opts.MaxHeadSamples)
	if prefixErr != nil || count != 1920 {
		t.Fatalf("reusable prefix: %d: %v", count, prefixErr)
	}
	if err := store.Ingest(t.Context(), []Batch{{Series: series, Samples: []Sample{incoming}}}); err != nil {
		t.Fatal(err)
	}
	after := readTail()
	kept, keptCount, keptErr := reusableHeadPrefix(after, incoming.At, store.opts.MaxHeadSamples)
	if keptErr != nil || keptCount != count || !bytes.Equal(prefix, kept) {
		t.Fatalf("encoded prefix changed: %d: %v", keptCount, keptErr)
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
