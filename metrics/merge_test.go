package metrics

import (
	"database/sql"
	"errors"
	"testing"
	"time"
)

func TestIncrementalSealingMergesGroupsWithoutChangingSamples(t *testing.T) {
	store, path := openTestStore(t, Options{})
	points := testSamples(33*blockSamples + 1)
	constant := Series{Kind: Counter, Labels: []Label{{Name: "__name__", Value: "constant"}}}
	for start := 0; start < len(points); {
		end := min(start+blockSamples, len(points))
		if start == 0 {
			end++
		}
		quiet := make([]Sample, end-start)
		for i := range quiet {
			quiet[i] = Sample{At: points[start+i].At, Value: 7}
		}
		if err := store.Ingest(t.Context(), []Batch{{Series: testSeries(), Samples: points[start:end]}, {Series: constant, Samples: quiet}}); err != nil {
			t.Fatal(err)
		}
		work, err := store.Maintain(t.Context())
		if err != nil || work.SealedBlocks != 2 {
			t.Fatalf("newly sealed blocks: %+v %v", work, err)
		}
		start = end
	}
	assertSamples(t, readAll(t, store), points)
	if err := store.file.View(t.Context(), func(tx *sql.Tx) error {
		var groups, clocks, refs, payloads, next int
		err := tx.QueryRowContext(t.Context(), `select (select count(*) from groups),(select count(*) from clocks),(select sum(refs) from clocks),(select count(*) from payloads),next_payload_id from store_state`).Scan(&groups, &clocks, &refs, &payloads, &next)
		if groups != 4 || clocks != 2 || refs != 4 || payloads != 33 || next != 34 {
			t.Fatalf("groups=%d clocks=%d refs=%d payloads=%d next=%d", groups, clocks, refs, payloads, next)
		}
		return err
	}); err != nil {
		t.Fatal(err)
	}
	if err := store.runtime.Close(t.Context()); err != nil {
		t.Fatal(err)
	}
	reopened, err := openAt(t, path, Options{})
	if err != nil {
		t.Fatal(err)
	}
	defer reopened.Close(t.Context())
	reopened.now = func() time.Time { return time.UnixMilli(testEpoch + 900) }
	assertSamples(t, readAll(t, reopened), points)
	if err = reopened.Ingest(t.Context(), []Batch{{Series: testSeries(), Samples: points[:1]}}); !errors.Is(err, ErrTooOld) {
		t.Fatalf("merge reopened sealed frontier: %v", err)
	}
	reopened.now = func() time.Time { return time.UnixMilli(testEpoch).Add(31 * 24 * time.Hour) }
	for range 3 {
		if _, err = reopened.Maintain(t.Context()); err != nil {
			t.Fatal(err)
		}
	}
	if err = reopened.file.View(t.Context(), func(tx *sql.Tx) error {
		var remaining int
		readErr := tx.QueryRowContext(t.Context(), `select (select count(*) from groups)+(select count(*) from clocks)+(select count(*) from payloads)`).Scan(&remaining)
		if remaining != 0 {
			t.Fatalf("retention left %d owned objects", remaining)
		}
		return readErr
	}); err != nil {
		t.Fatal(err)
	}
}

func TestMergeSkipsExpiredSlotsAndRollsBackAsOnePublication(t *testing.T) {
	store, _ := openTestStore(t, Options{Retention: time.Second})
	points := testSamples(3*blockSamples + 1)
	if err := store.Ingest(t.Context(), []Batch{{Series: testSeries(), Samples: points[:2*blockSamples+1]}}); err != nil {
		t.Fatal(err)
	}
	if _, err := store.Maintain(t.Context()); err != nil {
		t.Fatal(err)
	}
	cutoff := testEpoch + blockSamples
	store.now = func() time.Time { return time.UnixMilli(cutoff + 1000) }
	if _, _, err := store.expireSeries(t.Context(), 1, cutoff); err != nil {
		t.Fatal(err)
	}
	if err := store.Ingest(t.Context(), []Batch{{Series: testSeries(), Samples: points[2*blockSamples+1:]}}); err != nil {
		t.Fatal(err)
	}
	if err := store.file.Update(t.Context(), func(tx *sql.Tx) error {
		_, err := tx.ExecContext(t.Context(), `create trigger fail_merge before update of sealed_before on series_state begin select raise(abort,'injected merge failure'); end`)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	if _, err := store.Maintain(t.Context()); err == nil {
		t.Fatal("merge failure was ignored")
	}
	assertSamples(t, readAll(t, store), points[blockSamples:])
	if err := store.file.View(t.Context(), func(tx *sql.Tx) error {
		var payloads, groups, refs int
		var next int64
		err := tx.QueryRowContext(t.Context(), `select (select count(*) from payloads),(select count(*) from groups),(select sum(refs) from clocks),next_payload_id from store_state`).Scan(&payloads, &groups, &refs, &next)
		if payloads != 1 || groups != 1 || refs != 1 || next != 3 {
			t.Fatalf("rollback left payloads=%d groups=%d refs=%d next=%d", payloads, groups, refs, next)
		}
		return err
	}); err != nil {
		t.Fatal(err)
	}
	if err := store.file.Update(t.Context(), func(tx *sql.Tx) error {
		_, err := tx.ExecContext(t.Context(), `drop trigger fail_merge`)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	work, err := store.Maintain(t.Context())
	if err != nil || work.SealedBlocks != 1 {
		t.Fatalf("retry merge: %+v %v", work, err)
	}
	assertSamples(t, readAll(t, store), points[blockSamples:])
	if err = store.file.View(t.Context(), func(tx *sql.Tx) error {
		group, _, readErr := store.firstGroup(t.Context(), tx, 1)
		if group.start != cutoff || len(group.blocks) != 2 || group.live != 3 {
			t.Fatalf("merged expired prefix: %+v", group)
		}
		return readErr
	}); err != nil {
		t.Fatal(err)
	}
}
