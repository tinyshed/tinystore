package spike

import (
	"context"
	"database/sql"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/klauspost/compress/zstd"
)

func TestRetentionClipsAPersistedBlockBeforeSummarising(t *testing.T) {
	db := openBlocks(t, filepath.Join(t.TempDir(), "boundary.db"))
	defer db.Close()
	writer, reader := baselineCodec(t)
	samples := []sample{
		{at: 0, value: 100},
		{at: 1000, value: 110},
		{at: 2000, value: 5},
		{at: 3000, value: 20},
	}
	tx, err := db.Begin()
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = tx.Rollback() }()
	writeBlock(t, tx, 1, summarise(samples), encode(samples, writer))
	if _, err = tx.Exec(`insert into head values(1, 4000, 30)`); err != nil {
		t.Fatal(err)
	}
	if err = tx.Commit(); err != nil {
		t.Fatal(err)
	}

	for _, test := range []struct {
		from, cutoff int64
		count        int
		increase     float64
	}{
		{from: 0, cutoff: 2000, count: 3, increase: 25},
		{from: 3000, cutoff: 2000, count: 2, increase: 10},
		{from: 0, cutoff: 3001, count: 1, increase: 0},
		{from: 0, cutoff: 5000, count: 0, increase: 0},
	} {
		got := readRetainedSamples(t, db, reader, test.from, 5000, test.cutoff)
		s := summarise(got)
		if s.count != test.count || s.increase != test.increase {
			t.Fatalf("from=%d cutoff=%d: count=%d increase=%v", test.from, test.cutoff, s.count, s.increase)
		}
	}

	var physicallyHeld int
	if err = db.QueryRow(`select count from blocks`).Scan(&physicallyHeld); err != nil {
		t.Fatal(err)
	}
	if physicallyHeld != 4 {
		t.Fatal("query changed the retained block")
	}
}

func TestASilentTailExpiresWithoutBecomingABlock(t *testing.T) {
	db := openSparseHead(t)
	defer db.Close()
	sparseExec(t, db, `insert into series_state values(1, 3000, 0, 1000)`)
	sparseExec(t, db, `insert into head values(1, 1000, 10), (1, 2000, 11), (1, 3000, 12)`)
	writer, _ := baselineCodec(t)

	// a day of silence changes neither the series watermark nor the packing threshold
	if readySparsePrefix([]sample{{at: 1000}, {at: 2000}, {at: 3000}}, 3000, writer) {
		t.Fatal("silence manufactured a block")
	}
	if n := sweepSparseHead(t, db, 3000); n != 1 {
		t.Fatalf("swept %d series", n)
	}
	var rows, oldest int64
	if err := db.QueryRow(`select count(*), min(at) from head`).Scan(&rows, &oldest); err != nil {
		t.Fatal(err)
	}
	if rows != 1 || oldest != 3000 {
		t.Fatalf("cutoff equality lost: count=%d oldest=%d", rows, oldest)
	}
	sweepSparseHead(t, db, 3001)
	var pending sql.NullInt64
	if err := db.QueryRow(`select oldest_head_ts from series_state`).Scan(&pending); err != nil {
		t.Fatal(err)
	}
	if pending.Valid {
		t.Fatal("empty head remains scheduled for expiry")
	}
	var sealed, blocks int64
	if err := db.QueryRow(`select sealed_before from series_state`).Scan(&sealed); err != nil {
		t.Fatal(err)
	}
	if err := db.QueryRow(`select count(*) from blocks`).Scan(&blocks); err != nil {
		t.Fatal(err)
	}
	if sealed != 0 || blocks != 0 {
		t.Fatal("retention performed sealing")
	}
	if acceptsRetained(2999, sealed, 3001) || !acceptsRetained(3001, sealed, 3001) {
		t.Fatal("retention frontier can be bypassed after head expiry")
	}
	if acceptsRetained(3001, 4000, 3001) || !acceptsRetained(4000, 4000, 3001) {
		t.Fatal("retention check lost the sealed frontier")
	}
}

func TestSparseHeadLifecycle(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1 to measure")
	}

	writer, _ := baselineCodec(t)
	for _, count := range []int{20_000, 100_000} {
		db := openSparseHead(t)
		seedSparseSeries(t, db, count)
		var peak capacitySnapshot
		for tick := range 90 {
			now := epoch + int64(time.Duration(tick)*day/time.Millisecond)
			cutoff := now - int64(retention/time.Millisecond)
			sweepSparseHead(t, db, cutoff)
			appendSparseDay(t, db, count, tick, now)
			live, err := headOf(db, int64(seriesTotal/count), targetSamples+1)
			if err != nil {
				t.Fatal(err)
			}
			if readySparsePrefix(live, now, writer) {
				t.Fatal("daily series should expire before its safe prefix reaches a packing threshold")
			}
			checkpoint(t, db)
			state := readCapacitySnapshot(t, db)
			if state.physical > peak.physical {
				peak = state
			}
			if tick == 29 || tick == 30 || tick == 59 || tick == 89 {
				var rows int64
				if err := db.QueryRow(`select count(*) from head`).Scan(&rows); err != nil {
					t.Fatal(err)
				}
				if want := int64(min(tick+1, 31) * count); rows != want {
					t.Fatalf("day %d holds %d head rows, want %d", tick+1, rows, want)
				}
				t.Logf("series=%d day=%d rows=%d blocks=%d file=%.3f MiB allocated=%.3f MiB free=%.3f MiB",
					count, tick+1, rows, state.blocks, bytesMiB(state.physical), bytesMiB(state.live), bytesMiB(state.free))
			}
		}
		projected := float64(peak.physical) * seriesTotal / float64(count) / (1 << 20)
		t.Logf("series=%d observed peak %.3f MiB; projection at 1M + old registry %.1f MiB; per-series expiry index included",
			count, bytesMiB(peak.physical), projected+134.3)
		if err := db.Close(); err != nil {
			t.Fatal(err)
		}
	}
}

func acceptsRetained(at, sealedBefore, cutoff int64) bool {
	return at >= max(sealedBefore, cutoff)
}

func readySparsePrefix(samples []sample, seen int64, writer *zstd.Encoder) bool {
	prefix := safePrefix(samples, seen-int64(5*time.Minute/time.Millisecond))
	if len(prefix) == 0 {
		return false
	}
	return len(prefix) >= targetSamples || len(encode(prefix, writer)) >= 2048 ||
		prefix[len(prefix)-1].at-prefix[0].at >= int64(maxBlockSpan/time.Millisecond)
}

func openSparseHead(t *testing.T) *sql.DB {
	t.Helper()
	db := openBlocks(t, filepath.Join(t.TempDir(), "sparse.db"))
	sparseExec(t, db, `pragma foreign_keys=on`)
	sparseExec(t, db, `alter table series_state add column oldest_head_ts integer`)
	sparseExec(t, db, `create index head_expiry on series_state(oldest_head_ts) where oldest_head_ts is not null`)
	var id, parent, unused int
	var plan string
	err := db.QueryRow(`explain query plan select series_id from series_state where oldest_head_ts < ?`, epoch).
		Scan(&id, &parent, &unused, &plan)
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(plan, "SEARCH") || !strings.Contains(plan, "head_expiry") {
		t.Fatalf("expiry discovery scans the registry: %s", plan)
	}
	return db
}

func seedSparseSeries(t *testing.T, db *sql.DB, count int) {
	t.Helper()
	tx, err := db.Begin()
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = tx.Rollback() }()
	insert, err := tx.Prepare(`insert into series_state values(?, ?, ?, null)`)
	if err != nil {
		t.Fatal(err)
	}
	defer insert.Close()
	for i := 1; i <= count; i++ {
		if _, err = insert.Exec(i*(seriesTotal/count), epoch, epoch-1); err != nil {
			t.Fatal(err)
		}
	}
	if err = tx.Commit(); err != nil {
		t.Fatal(err)
	}
}

func appendSparseDay(t *testing.T, db *sql.DB, count, tick int, at int64) {
	t.Helper()
	tx, err := db.Begin()
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = tx.Rollback() }()
	insert, err := tx.Prepare(`insert into head values(?, ?, ?)`)
	if err != nil {
		t.Fatal(err)
	}
	defer insert.Close()
	for i := 1; i <= count; i++ {
		id := i * (seriesTotal / count)
		if _, err = insert.Exec(id, at, 1000+(id+tick)%97); err != nil {
			t.Fatal(err)
		}
	}
	if _, err = tx.Exec(`update series_state set max_seen_ts=?, oldest_head_ts=coalesce(oldest_head_ts, ?)`, at, at); err != nil {
		t.Fatal(err)
	}
	if err = tx.Commit(); err != nil {
		t.Fatal(err)
	}
}

func sweepSparseHead(t *testing.T, db *sql.DB, cutoff int64) int {
	t.Helper()
	total := 0
	for {
		// each batch releases the writer before finding the next expired series
		count := expireHeadBatch(t, db, cutoff)
		total += count
		if count < 1000 {
			return total
		}
	}
}

func expireHeadBatch(t *testing.T, db *sql.DB, cutoff int64) int {
	t.Helper()
	tx, err := db.Begin()
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = tx.Rollback() }()
	ids, err := dueHeadSeries(tx, cutoff)
	if err != nil {
		t.Fatal(err)
	}
	remove, err := tx.Prepare(`delete from head where series_id=? and at < ?`)
	if err != nil {
		t.Fatal(err)
	}
	defer remove.Close()
	advance, err := tx.Prepare(`update series_state set oldest_head_ts=(select min(at) from head where series_id=?) where series_id=?`)
	if err != nil {
		t.Fatal(err)
	}
	defer advance.Close()
	for _, id := range ids {
		if _, err = remove.Exec(id, cutoff); err != nil {
			t.Fatal(err)
		}
		if _, err = advance.Exec(id, id); err != nil {
			t.Fatal(err)
		}
	}
	if err = tx.Commit(); err != nil {
		t.Fatal(err)
	}
	return len(ids)
}

func dueHeadSeries(tx *sql.Tx, cutoff int64) ([]int64, error) {
	rows, err := tx.Query(`select series_id from series_state where oldest_head_ts < ? order by oldest_head_ts limit 1000`, cutoff)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var ids []int64
	for rows.Next() {
		var id int64
		if err = rows.Scan(&id); err != nil {
			return nil, err
		}
		ids = append(ids, id)
	}
	return ids, rows.Err()
}

func readRetainedSamples(t *testing.T, db *sql.DB, reader *zstd.Decoder, from, to, cutoff int64) []sample {
	t.Helper()
	from = max(from, cutoff)
	tx, err := db.BeginTx(context.Background(), &sql.TxOptions{ReadOnly: true})
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = tx.Rollback() }()
	var body []byte
	if err = tx.QueryRow(`select body from payloads`).Scan(&body); err != nil {
		t.Fatal(err)
	}
	var tail sample
	if err = tx.QueryRow(`select at, value from head`).Scan(&tail.at, &tail.value); err != nil {
		t.Fatal(err)
	}
	if err = tx.Commit(); err != nil {
		t.Fatal(err)
	}
	samples, err := decode(body, reader)
	if err != nil {
		t.Fatal(err)
	}
	samples = append(samples, tail)
	return slices.DeleteFunc(samples, func(s sample) bool { return s.at < from || s.at >= to })
}

func sparseExec(t *testing.T, db *sql.DB, statement string, args ...any) {
	t.Helper()
	if _, err := db.Exec(statement, args...); err != nil {
		t.Fatal(err)
	}
}
