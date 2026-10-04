package metrics

import (
	"database/sql"
	"testing"
	"time"

	"github.com/ncruces/go-sqlite3"
)

const expiryWorkSeries = `
	with recursive ids(id) as (values(1) union all select id+1 from ids where id<10000)
	insert into series(id, identity, label_ids, kind)
	select id, 'expiry-'||id, x'', 'gauge' from ids`

const expiryWorkState = `
	insert into series_state(series_id, max_seen_ts, next_gc_ts, keep)
	select id, 0, 0, iif(id%2=0, 60000, null) from series`

func TestExpirySelectsABoundedPrefixOfEachIndex(t *testing.T) {
	store, path := openTestStore(t, Options{MaintenanceSeries: 2})
	err := store.file.Update(t.Context(), func(tx *sql.Tx) error {
		if _, err := tx.ExecContext(t.Context(), expiryWorkSeries); err != nil {
			return err
		}
		_, err := tx.ExecContext(t.Context(), expiryWorkState)
		return err
	})
	if err != nil {
		t.Fatal(err)
	}
	conn, err := sqlite3.Open(path)
	if err != nil {
		t.Fatal(err)
	}
	defer conn.Close()
	statement, _, err := conn.Prepare(expiryDueQuery)
	if err != nil {
		t.Fatal(err)
	}
	defer statement.Close()
	now := testEpoch + 5*time.Hour.Milliseconds()
	for i, value := range []int64{store.retention.fallback, earlier(now, store.retention.fallback), now, 2} {
		if err := statement.BindInt64(i+1, value); err != nil {
			t.Fatal(err)
		}
	}
	count := 0
	for statement.Step() {
		count++
	}
	if err := statement.Err(); err != nil || count != 2 {
		t.Fatalf("expiry selected %d series: %v", count, err)
	}
	// VM work bounds the scan without a wall-clock threshold under CI load.
	if steps := statement.Status(sqlite3.STMTSTATUS_VM_STEP, false); steps > 10000 {
		t.Fatalf("selecting two expired series walked the backlog: %d VM steps", steps)
	}
}

func TestMaintenanceRotatesReadySeries(t *testing.T) {
	s, _ := openTestStore(t, Options{MaintenanceSeries: 1})
	a := Series{Name: "a"}
	b := Series{Name: "b"}
	points := testSamples(481)
	if err := s.Ingest(t.Context(), []Batch{{Series: a, Samples: points[:241]}, {Series: b, Samples: points[:241]}}); err != nil {
		t.Fatal(err)
	}
	first, err := s.Maintain(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	if first.SealedBlocks != 1 {
		t.Fatalf("first pass sealed %d blocks", first.SealedBlocks)
	}
	if err = s.Ingest(t.Context(), []Batch{{Series: a, Samples: points[241:]}}); err != nil {
		t.Fatal(err)
	}
	second, err := s.Maintain(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	if second.SealedBlocks != 1 {
		t.Fatalf("second pass sealed %d blocks", second.SealedBlocks)
	}
	if err := s.file.View(t.Context(), func(tx *sql.Tx) error {
		var frontier sql.NullInt64
		if err := tx.QueryRowContext(t.Context(), `select state.sealed_before from series_state state join postings p on p.series_id=state.series_id join label_values v on v.id=p.label_id where v.name='__name__' and v.value='b'`).Scan(&frontier); err != nil {
			return err
		}
		if !frontier.Valid || frontier.Int64 != points[240].At {
			t.Fatalf("later series frontier: %v", frontier)
		}
		return nil
	}); err != nil {
		t.Fatal(err)
	}
}
