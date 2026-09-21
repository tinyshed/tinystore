package spike

import (
	"database/sql"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/tinyshed/tinystore/codec"
)

// both paths carry series_state, because a store needs the frontier whichever
// way it finds its due work; only the index beside it differs
const seriesStateTable = `create table series_state (series_id integer primary key,
  max_seen_ts integer not null, sealed_before integer not null,
  next_gc_ts integer) strict`

// what the store has now: the foreign key, its index, and a global expiry index
func currentSchema() denseSchema {
	schema := todaysSchema()
	schema.name = "as it stands today"
	schema.create = append(schema.create, seriesStateTable)
	inner := schema.insert
	schema.insert = func(tx *sql.Tx, i int, seriesID int64, s summary, packed []byte) error {
		if err := inner(tx, i, seriesID, s, packed); err != nil {
			return err
		}
		return upsertState(tx, seriesID, s)
	}
	return schema
}

func expirySchema() denseSchema {
	schema := todaysSchema()
	schema.name = "expiry index over blocks"
	schema.create = append(schema.create, seriesStateTable)
	schema.indexes = []string{`create index block_expiry on blocks(end_ts)`}
	inner := schema.insert
	schema.insert = func(tx *sql.Tx, i int, seriesID int64, s summary, packed []byte) error {
		if err := inner(tx, i, seriesID, s, packed); err != nil {
			return err
		}
		return upsertState(tx, seriesID, s)
	}
	return schema
}

// without the foreign key nothing has to be searched when a payload goes, and
// the two deletes travel in one transaction instead
func plainSchema() denseSchema {
	schema := todaysSchema()
	schema.name = "no foreign key, per series"
	schema.create = []string{
		`create table payloads (id integer primary key, body blob not null) strict`,
		`create table blocks (series_id integer not null, start_ts integer not null,
		  end_ts integer not null, count integer not null,
		  min real, max real, sum real, first real, last real, increase real,
		  resets integer not null, payload_id integer not null,
		  primary key (series_id, start_ts)) strict, without rowid`,
		seriesStateTable,
	}
	schema.indexes = []string{
		`create index series_due on series_state(next_gc_ts) where next_gc_ts is not null`,
	}
	inner := schema.insert
	schema.insert = func(tx *sql.Tx, i int, seriesID int64, s summary, packed []byte) error {
		if err := inner(tx, i, seriesID, s, packed); err != nil {
			return err
		}
		return upsertState(tx, seriesID, s)
	}
	return schema
}

func seriesGCSchema() denseSchema {
	schema := todaysSchema()
	schema.name = "due work per series"
	schema.create = append(schema.create, seriesStateTable)
	schema.indexes = []string{
		`create index series_due on series_state(next_gc_ts) where next_gc_ts is not null`,
	}
	inner := schema.insert
	schema.insert = func(tx *sql.Tx, i int, seriesID int64, s summary, packed []byte) error {
		if err := inner(tx, i, seriesID, s, packed); err != nil {
			return err
		}
		return upsertState(tx, seriesID, s)
	}
	return schema
}

func upsertState(tx *sql.Tx, seriesID int64, s summary) error {
	_, err := tx.Exec(`insert into series_state(series_id, max_seen_ts, sealed_before, next_gc_ts)
	  values(?,?,?,?)
	  on conflict(series_id) do update set
	    max_seen_ts = max(max_seen_ts, excluded.max_seen_ts),
	    next_gc_ts = min(next_gc_ts, excluded.next_gc_ts)`,
		seriesID, s.endTS, s.startTS, s.endTS)
	return err //nolint:wrapcheck // the caller names the schema
}

// a retention pass that deletes every block whose last sample is behind the
// cutoff, and leaves the store able to find its next work
func sweepByExpiry(t *testing.T, db *sql.DB, cutoff int64) (int64, time.Duration) {
	t.Helper()

	start := time.Now()
	var deleted int64
	for {
		tx, txErr := db.Begin()
		if txErr != nil {
			t.Fatal(txErr)
		}
		if _, err := tx.Exec(`delete from payloads where id in (
		    select payload_id from blocks where end_ts < ? limit 500)`, cutoff); err != nil {
			t.Fatal(err)
		}
		result, err := tx.Exec(`delete from blocks where (series_id, start_ts) in (
		    select series_id, start_ts from blocks where end_ts < ? limit 500)`, cutoff)
		if err != nil {
			t.Fatal(err)
		}
		n, err := result.RowsAffected()
		if err != nil {
			t.Fatal(err)
		}
		if err = tx.Commit(); err != nil {
			t.Fatal(err)
		}
		deleted += n
		if n == 0 {
			break
		}
	}
	return deleted, time.Since(start)
}

func dueSeries(t *testing.T, db *sql.DB, cutoff int64) []int64 {
	t.Helper()

	rows, err := db.Query(`select series_id from series_state
	  where next_gc_ts is not null and next_gc_ts < ? order by next_gc_ts limit 200`, cutoff)
	if err != nil {
		t.Fatal(err)
	}
	defer rows.Close()

	var due []int64
	for rows.Next() {
		var id int64
		if err = rows.Scan(&id); err != nil {
			t.Fatal(err)
		}
		due = append(due, id)
	}
	if err = rows.Err(); err != nil {
		t.Fatal(err)
	}
	return due
}

func sweepBySeries(t *testing.T, db *sql.DB, cutoff int64) (int64, time.Duration) {
	t.Helper()

	start := time.Now()
	var deleted int64
	for {
		due := dueSeries(t, db, cutoff)
		if len(due) == 0 {
			break
		}
		tx, txErr := db.Begin()
		if txErr != nil {
			t.Fatal(txErr)
		}
		for _, id := range due {
			if _, execErr := tx.Exec(`delete from payloads where id in (
			    select payload_id from blocks where series_id = ? and end_ts < ?)`, id, cutoff); execErr != nil {
				t.Fatal(execErr)
			}
			result, execErr := tx.Exec(`delete from blocks where series_id = ? and end_ts < ?`, id, cutoff)
			if execErr != nil {
				t.Fatal(execErr)
			}
			n, affectedErr := result.RowsAffected()
			if affectedErr != nil {
				t.Fatal(affectedErr)
			}
			deleted += n
			if _, updateErr := tx.Exec(`update series_state set next_gc_ts =
			    (select min(end_ts) from blocks where series_id = ?) where series_id = ?`,
				id, id); updateErr != nil {
				t.Fatal(updateErr)
			}
		}
		if commitErr := tx.Commit(); commitErr != nil {
			t.Fatal(commitErr)
		}
	}
	return deleted, time.Since(start)
}

func TestWhatRetentionCostsWhenItWalksSeries(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1 to measure")
	}
	blocks, err := codec.New()
	if err != nil {
		t.Fatal(err)
	}
	defer blocks.Close()

	for _, shape := range []struct {
		name           string
		series, blocks int
	}{
		{"dense, ten blocks a series", 1000, 10000},
		{"sparse, one block a series", 10000, 10000},
	} {
		for _, path := range []struct {
			schema denseSchema
			sweep  func(*testing.T, *sql.DB, int64) (int64, time.Duration)
		}{
			{currentSchema(), sweepByExpiry},
			{expirySchema(), sweepByExpiry},
			{seriesGCSchema(), sweepBySeries},
			{plainSchema(), sweepBySeries},
		} {
			db := openNight(t, filepath.Join(t.TempDir(), "gc.db"), path.schema)
			tx, beginErr := db.Begin()
			if beginErr != nil {
				t.Fatal(beginErr)
			}
			var payload int64
			for i := range shape.blocks {
				samples := adaptiveSamples("integers", 240, i)
				old := make([]sample, len(samples))
				for j, s := range samples {
					old[j] = sample{at: s.At, value: s.Value}
				}
				_, packed, encodeErr := blocks.Encode(samples)
				if encodeErr != nil {
					t.Fatal(encodeErr)
				}
				payload += int64(len(packed))
				if err = path.schema.insert(tx, i, int64(i%shape.series)+1, summarise(old), packed); err != nil {
					t.Fatal(err)
				}
			}
			if err = tx.Commit(); err != nil {
				t.Fatal(err)
			}
			checkpoint(t, db)

			shares, file := nightShares(t, db, "payloads")
			samples := float64(shape.blocks * 240)
			metadata := 0.0
			for _, share := range shares {
				if share.name != "payloads" {
					metadata += float64(share.bytes) / samples
				}
			}
			var cutoff int64
			if err = db.QueryRow(`select min(end_ts) + (max(end_ts) - min(end_ts)) / 2 from blocks`).
				Scan(&cutoff); err != nil {
				t.Fatal(err)
			}
			deleted, elapsed := path.sweep(t, db, cutoff)

			t.Logf("%-28s %-24s file=%.3f B/sample  metadata=%.3f  sweep=%s for %d blocks (%.1f us a block)",
				shape.name, path.schema.name, float64(file)/samples, metadata,
				elapsed, deleted, float64(elapsed.Microseconds())/float64(max(deleted, 1)))
			for _, share := range shares {
				if share.name != "payloads" && share.name != "sqlite_schema" {
					t.Logf("        %-14s %7.4f B/sample", share.name, float64(share.bytes)/samples)
				}
			}
			if err = db.Close(); err != nil {
				t.Fatal(err)
			}
		}
	}
}
