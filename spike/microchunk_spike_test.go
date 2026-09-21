package spike

import (
	"database/sql"
	"math"
	"os"
	"slices"
	"testing"
	"time"

	"github.com/tinyshed/tinystore/codec"
)

func TestImmutableMicrochunkLifecycle(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1 to measure")
	}
	c, err := codec.New()
	if err != nil {
		t.Fatal(err)
	}
	defer c.Close()
	const seriesCount = 10000
	for _, chunkSize := range []int{0, 8, 16} {
		db := openSparseHead(t)
		sparseExec(t, db, `alter table series_state rename column oldest_head_ts to next_gc_ts`)
		sparseExec(t, db, `create table microchunks(series_id integer not null, start_ts integer not null,
		 end_ts integer not null, count integer not null, body blob not null, primary key(series_id,start_ts)) strict, without rowid`)
		seedSparseSeries(t, db, seriesCount)
		sparseExec(t, db, `pragma wal_autocheckpoint=0`)
		checkpoint(t, db)
		var peak, walTotal int64
		start := time.Now()
		for tick := range 90 {
			now := epoch + int64(time.Duration(tick)*day/time.Millisecond)
			cutoff := now - int64(retention/time.Millisecond)
			for offset := 0; offset < seriesCount; offset += 500 {
				microchunkDay(t, db, c, chunkSize, offset, offset+500, seriesCount, tick, now, cutoff)
			}
			var path string
			var seq int
			var name string
			if err = db.QueryRow(`pragma database_list`).Scan(&seq, &name, &path); err != nil {
				t.Fatal(err)
			}
			info, statErr := os.Stat(path + "-wal")
			if statErr != nil {
				t.Fatal(statErr)
			}
			walTotal += info.Size()
			checkpoint(t, db)
			state := readCapacitySnapshot(t, db)
			peak = max(peak, state.physical)
			verifyMicroSeries(t, db, c, seriesTotal/seriesCount, tick, cutoff)
			if tick == 30 || tick == 59 || tick == 89 {
				var heads, chunks int
				if err = db.QueryRow(`select (select count(*) from head),(select count(*) from microchunks)`).Scan(&heads, &chunks); err != nil {
					t.Fatal(err)
				}
				t.Logf("chunk=%d day=%d file=%.3f MiB head=%d chunks=%d", chunkSize, tick+1, bytesMiB(state.physical), heads, chunks)
			}
		}
		var timings []time.Duration
		for i := 1; i <= 100; i++ {
			begin := time.Now()
			verifyMicroSeries(t, db, c, i*(seriesTotal/seriesCount), 89, epoch+int64(59*day/time.Millisecond))
			timings = append(timings, time.Since(begin))
		}
		slices.Sort(timings)
		t.Logf("chunk=%d peak=%.3f MiB projected 1M+registry=%.1f MiB WAL=%.1f B/ingested-sample query100 p50=%s p99=%s elapsed=%s",
			chunkSize, bytesMiB(peak), bytesMiB(peak)*seriesTotal/seriesCount+134.3,
			float64(walTotal)/(seriesCount*90), timings[49], timings[98], time.Since(start))
		if err = db.Close(); err != nil {
			t.Fatal(err)
		}
	}
}

func microchunkDay(t *testing.T, db *sql.DB, c *codec.Codec, chunkSize, from, to, count, tick int, now, cutoff int64) {
	t.Helper()
	tx, err := db.Begin()
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = tx.Rollback() }()
	insert, err := tx.Prepare(`insert into head values(?,?,?)`)
	if err != nil {
		t.Fatal(err)
	}
	defer insert.Close()
	expire, err := tx.Prepare(`delete from head where series_id=? and at<?`)
	if err != nil {
		t.Fatal(err)
	}
	defer expire.Close()
	collect, err := tx.Prepare(`delete from microchunks where series_id=? and end_ts<?`)
	if err != nil {
		t.Fatal(err)
	}
	defer collect.Close()
	state, err := tx.Prepare(`update series_state set max_seen_ts=?,next_gc_ts=(select min(at) from
	 (select min(at) at from head where series_id=? union all select min(end_ts) at from microchunks where series_id=?)) where series_id=?`)
	if err != nil {
		t.Fatal(err)
	}
	defer state.Close()
	for i := from + 1; i <= to; i++ {
		id := int64(i * (seriesTotal / count))
		if _, err = expire.Exec(id, cutoff); err != nil {
			t.Fatal(err)
		}
		if _, err = collect.Exec(id, cutoff); err != nil {
			t.Fatal(err)
		}
		if _, err = insert.Exec(id, now, 1000+(id+int64(tick))%97); err != nil {
			t.Fatal(err)
		}
		if chunkSize > 0 && tick > 0 && tick%chunkSize == 0 {
			rows, readErr := readHeadInTransaction(tx, id)
			if readErr != nil {
				t.Fatal(readErr)
			}
			safe := safePrefix(rows, now-int64(5*time.Minute/time.Millisecond))
			if len(safe) != chunkSize {
				t.Fatalf("safe prefix %d != chunk %d", len(safe), chunkSize)
			}
			samples := make([]codec.Sample, len(safe))
			for j, s := range safe {
				samples[j] = codec.Sample{At: s.at, Value: s.value}
			}
			payload, encodeErr := c.Encode(samples)
			if encodeErr != nil {
				t.Fatal(encodeErr)
			}
			last := safe[len(safe)-1].at
			if _, err = tx.Exec(`insert into microchunks values(?,?,?,?,?)`, id, safe[0].at, last, len(safe), payload); err != nil {
				t.Fatal(err)
			}
			if _, err = tx.Exec(`delete from head where series_id=? and at<=?`, id, last); err != nil {
				t.Fatal(err)
			}
			if _, err = tx.Exec(`update series_state set sealed_before=? where series_id=?`, last+1, id); err != nil {
				t.Fatal(err)
			}
		}
		if _, err = state.Exec(now, id, id, id); err != nil {
			t.Fatal(err)
		}
	}
	if err = tx.Commit(); err != nil {
		t.Fatal(err)
	}
}

func verifyMicroSeries(t *testing.T, db *sql.DB, c *codec.Codec, id int, tick int, cutoff int64) {
	t.Helper()
	tx, err := db.Begin()
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = tx.Rollback() }()
	head, err := readHeadInTransaction(tx, int64(id))
	if err != nil {
		t.Fatal(err)
	}
	bodies, err := microBodies(tx, int64(id))
	if err != nil {
		t.Fatal(err)
	}
	if err = tx.Commit(); err != nil {
		t.Fatal(err)
	}
	all := map[int64]uint64{}
	add := func(at int64, value float64) {
		if at < cutoff {
			return
		}
		if _, ok := all[at]; ok {
			t.Fatal("sample duplicated by compaction")
		}
		all[at] = math.Float64bits(value)
	}
	for _, s := range head {
		add(s.at, s.value)
	}
	for _, body := range bodies {
		it, decodeErr := c.Decode(body)
		if decodeErr != nil {
			t.Fatal(decodeErr)
		}
		for it.Next() {
			s := it.Sample()
			add(s.At, s.Value)
		}
		if it.Err() != nil {
			t.Fatal(it.Err())
		}
	}
	if len(all) != min(tick+1, 31) {
		t.Fatalf("logical count %d day %d", len(all), tick)
	}
	for d := max(0, tick-30); d <= tick; d++ {
		at := epoch + int64(time.Duration(d)*day/time.Millisecond)
		if got, ok := all[at]; !ok || got != math.Float64bits(float64(1000+(id+d)%97)) {
			t.Fatal("retained sample changed")
		}
	}
}

func microBodies(tx *sql.Tx, id int64) ([][]byte, error) {
	rows, err := tx.Query(`select body from microchunks where series_id=? order by start_ts`, id)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var out [][]byte
	for rows.Next() {
		var body []byte
		if err = rows.Scan(&body); err != nil {
			return nil, err
		}
		out = append(out, body)
	}
	return out, rows.Err()
}
