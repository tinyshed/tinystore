package spike

import (
	"database/sql"
	"os"
	"testing"
	"time"

	"github.com/klauspost/compress/zstd"
)

func TestSparsePackingSpans(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1 to measure")
	}
	writer, reader := baselineCodec(t)
	const count = 20_000
	for _, span := range []time.Duration{7 * day, 14 * day, 30 * day} {
		db := openSparseHead(t)
		sparseExec(t, db, `create index block_expiry on blocks(end_ts)`)
		sparseExec(t, db, `create index block_payload on blocks(payload_id)`)
		seedSparseSeries(t, db, count)
		var peak int64
		var firstPlateau int64
		packedSamples := 0
		for tick := range 90 {
			now := epoch + int64(time.Duration(tick)*day/time.Millisecond)
			cutoff := now - int64(retention/time.Millisecond)
			sweepSparseHead(t, db, cutoff)
			deleteExpiredBlocks(t, db, cutoff)
			appendSparseDay(t, db, count, tick, now)
			first, err := headOf(db, seriesTotal/count, targetSamples+1)
			if err != nil {
				t.Fatal(err)
			}
			safe := safePrefix(first, now-int64(5*time.Minute/time.Millisecond))
			if len(safe) > 0 && safe[len(safe)-1].at-safe[0].at >= int64(span/time.Millisecond) {
				packedSamples = len(safe)
				// the fixture synchronises arrivals, so every series becomes ready together
				for offset := 0; offset < count; offset += 1000 {
					packSparseBatch(t, db, writer, offset, min(offset+1000, count), count, now)
				}
			}
			checkpoint(t, db)
			s := readCapacitySnapshot(t, db)
			peak = max(peak, s.physical)
			if tick == 59 {
				firstPlateau = s.physical
			}
			if tick == 59 || tick == 89 {
				var headRows int64
				if err = db.QueryRow(`select count(*) from head`).Scan(&headRows); err != nil {
					t.Fatal(err)
				}
				t.Logf("span=%dd day=%d file=%.3f MiB free=%.3f MiB blocks=%d head=%d packed/block=%d",
					int(span/day), tick+1, bytesMiB(s.physical), bytesMiB(s.free), s.blocks, headRows, packedSamples)
			}
			verifySparseSeries(t, db, reader, seriesTotal/count, now, cutoff)
		}
		if peak > firstPlateau+firstPlateau/20 {
			t.Fatalf("span=%s did not plateau: day60=%d peak=%d", span, firstPlateau, peak)
		}
		t.Logf("span=%dd observed peak=%.3f MiB, projection at 1M + old registry=%.1f MiB",
			int(span/day), bytesMiB(peak), bytesMiB(peak)*seriesTotal/count+134.3)
		if err := db.Close(); err != nil {
			t.Fatal(err)
		}
	}
}

func packSparseBatch(t *testing.T, db *sql.DB, writer *zstd.Encoder, begin, end, count int, seen int64) {
	t.Helper()
	tx, err := db.Begin()
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = tx.Rollback() }()
	for i := begin + 1; i <= end; i++ {
		id := int64(i * (seriesTotal / count))
		samples, readErr := readHeadInTransaction(tx, id)
		if readErr != nil {
			t.Fatal(readErr)
		}
		safe := safePrefix(samples, seen-int64(5*time.Minute/time.Millisecond))
		if len(safe) == 0 {
			t.Fatal("scheduled an empty safe prefix")
		}
		packed := encode(safe, writer)
		if len(packed) > 2048 {
			t.Fatal("sparse fixture crossed the byte limit")
		}
		writeBlock(t, tx, id, summarise(safe), packed)
		last := safe[len(safe)-1].at
		if _, err = tx.Exec(`delete from head where series_id=? and at<=?`, id, last); err != nil {
			t.Fatal(err)
		}
		if _, err = tx.Exec(`update series_state set sealed_before=?, oldest_head_ts=(select min(at) from head where series_id=?) where series_id=?`, last+1, id, id); err != nil {
			t.Fatal(err)
		}
	}
	if err = tx.Commit(); err != nil {
		t.Fatal(err)
	}
}

func readHeadInTransaction(tx *sql.Tx, id int64) ([]sample, error) {
	rows, err := tx.Query(`select at, value from head where series_id=? order by at`, id)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var samples []sample
	for rows.Next() {
		var s sample
		if err = rows.Scan(&s.at, &s.value); err != nil {
			return nil, err
		}
		samples = append(samples, s)
	}
	return samples, rows.Err()
}

func verifySparseSeries(t *testing.T, db *sql.DB, reader *zstd.Decoder, id, now, cutoff int64) {
	t.Helper()
	rows, err := db.Query(`select p.body from blocks b join payloads p on p.id=b.payload_id where b.series_id=? order by b.start_ts`, id)
	if err != nil {
		t.Fatal(err)
	}
	var bodies [][]byte
	func() {
		defer rows.Close()
		for rows.Next() {
			var body []byte
			if err = rows.Scan(&body); err != nil {
				t.Fatal(err)
			}
			bodies = append(bodies, body)
		}
		if err = rows.Err(); err != nil {
			t.Fatal(err)
		}
	}()
	all := make(map[int64]float64)
	add := func(samples []sample) {
		for _, s := range samples {
			if s.at < cutoff {
				continue
			}
			if _, exists := all[s.at]; exists {
				t.Fatal("sample appears in both head and block")
			}
			all[s.at] = s.value
		}
	}
	for _, body := range bodies {
		samples, decodeErr := decode(body, reader)
		if decodeErr != nil {
			t.Fatal(decodeErr)
		}
		add(samples)
	}
	head, err := headOf(db, id, targetSamples+1)
	if err != nil {
		t.Fatal(err)
	}
	add(head)
	expected := 0
	for at := max(epoch, cutoff); at <= now; at += int64(day / time.Millisecond) {
		tick := (at - epoch) / int64(day/time.Millisecond)
		want := float64(1000 + (id+tick)%97)
		if got, found := all[at]; !found || got != want {
			t.Fatalf("at=%d got=%v found=%v want=%v", at, got, found, want)
		}
		expected++
	}
	if len(all) != expected {
		t.Fatalf("read %d samples, expected %d", len(all), expected)
	}
}
