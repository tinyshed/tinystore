package metrics

import (
	"context"
	"database/sql"
	"errors"
	"testing"
	"time"
)

func TestFailedPublicationRollsBackPayloadsHeadAndIdentifiers(t *testing.T) {
	store, _ := openTestStore(t, Options{})
	points := testSamples(500)
	if err := store.Ingest(t.Context(), []Batch{{Series: testSeries(), Samples: points}}); err != nil {
		t.Fatal(err)
	}
	if err := store.file.Update(t.Context(), func(tx *sql.Tx) error {
		_, err := tx.ExecContext(t.Context(), `create trigger fail_publication before update of sealed_before on series_state begin select raise(abort,'injected failure'); end`)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	if _, err := store.Maintain(t.Context()); err == nil {
		t.Fatal("injected failure was ignored")
	}
	assertSamples(t, readAll(t, store), points)
	if err := store.file.View(t.Context(), func(tx *sql.Tx) error {
		var payloads, groups int
		var nextID int64
		err := tx.QueryRowContext(t.Context(), `select (select count(*) from payloads),(select count(*) from groups),next_payload_id from store_state where id=1`).Scan(&payloads, &groups, &nextID)
		if payloads != 0 || groups != 0 || nextID != 1 {
			t.Fatalf("rollback left payloads=%d groups=%d next=%d", payloads, groups, nextID)
		}
		return err
	}); err != nil {
		t.Fatal(err)
	}
	if err := store.file.Update(t.Context(), func(tx *sql.Tx) error {
		_, err := tx.ExecContext(t.Context(), `drop trigger fail_publication`)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	if _, err := store.Maintain(t.Context()); err != nil {
		t.Fatal(err)
	}
	assertSamples(t, readAll(t, store), points)
}

func TestDirectoryCorruptionAndRebindingAreRefused(t *testing.T) {
	store, _ := openTestStore(t, Options{})
	points := testSamples(500)
	if err := store.Ingest(t.Context(), []Batch{{Series: testSeries(), Samples: points}}); err != nil {
		t.Fatal(err)
	}
	if _, err := store.Maintain(t.Context()); err != nil {
		t.Fatal(err)
	}
	var directory []byte
	var start, end, clockID int64
	if err := store.file.View(t.Context(), func(tx *sql.Tx) error {
		return tx.QueryRowContext(t.Context(), `select start_ts,end_ts,directory,clock_id from groups where series_id=1`).Scan(&start, &end, &directory, &clockID)
	}); err != nil {
		t.Fatal(err)
	}
	if _, err := store.readDirectory(2, start, end, clockID, directory, nil); !errors.Is(err, ErrCorrupt) {
		t.Fatalf("rebound directory: %v", err)
	}
	directory[len(directory)/2] ^= 1
	if err := store.file.Update(t.Context(), func(tx *sql.Tx) error {
		_, err := tx.ExecContext(t.Context(), `update groups set directory=? where series_id=1`, directory)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	if result, err := store.Read(t.Context(), Range{Matchers: testSeries().Labels, From: testEpoch, To: testEpoch + 500}); !errors.Is(err, ErrCorrupt) || result != nil {
		t.Fatalf("corruption: %v %#v", err, result)
	}
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

func TestWatermarkIsStrictAndFollowsTheSeries(t *testing.T) {
	store, _ := openTestStore(t, Options{Lateness: 261 * time.Millisecond})
	points := append(testSamples(240), Sample{At: testEpoch + 500, Value: 1})
	if err := store.Ingest(t.Context(), []Batch{{Series: testSeries(), Samples: points}}); err != nil {
		t.Fatal(err)
	}
	store.now = func() time.Time { return time.UnixMilli(testEpoch + 3600000) }
	work, err := store.Maintain(t.Context())
	if err != nil || work.SealedBlocks != 0 {
		t.Fatalf("sealed the watermark or followed wall time: %+v %v", work, err)
	}
	if err = store.Ingest(t.Context(), []Batch{{Series: testSeries(), Samples: []Sample{{At: testEpoch + 501, Value: 2}}}}); err != nil {
		t.Fatal(err)
	}
	work, err = store.Maintain(t.Context())
	if err != nil || work.SealedBlocks != 1 {
		t.Fatalf("new sample did not release prefix: %+v %v", work, err)
	}
}
