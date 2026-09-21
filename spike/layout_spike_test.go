package spike

import (
	"os"
	"path/filepath"
	"testing"
	"time"
)

func TestBlockLayoutsWithTheSameLifetime(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1 to measure")
	}
	writer, _ := baselineCodec(t)
	for _, shape := range []struct {
		name     string
		count    int
		interval time.Duration
		kind     string
	}{
		{"dense integer", 240, 15 * time.Second, "whole numbers"},
		{"dense float", 240, 15 * time.Second, "noisy gauge"},
		{"daily one", 1, day, "whole numbers"},
		{"daily seven", 7, day, "whole numbers"},
		{"daily fourteen", 14, day, "whole numbers"},
		{"daily thirty", 30, day, "whole numbers"},
	} {
		for _, layout := range []string{"separate", "inline rowid", "inline clustered"} {
			db := openBlocks(t, filepath.Join(t.TempDir(), "layout.db"))
			sparseExec(t, db, `pragma foreign_keys=on`)
			seedStateRows(t, db, 1000)
			sparseExec(t, db, `update series_state set max_seen_ts=?, sealed_before=?`, epoch, epoch)
			inline := layout != "separate"
			if inline {
				dll := `create table inline_blocks (
				 series_id integer not null, start_ts integer not null, end_ts integer not null,
				 count integer not null, min real, max real, sum real, first real, last real,
				 increase real, resets integer not null, body blob not null,
				 primary key(series_id, start_ts)) strict`
				if layout == "inline clustered" {
					dll += ", without rowid"
				}
				sparseExec(t, db, dll)
				sparseExec(t, db, `create index block_expiry on inline_blocks(end_ts)`)
			} else {
				sparseExec(t, db, `create index block_expiry on blocks(end_ts)`)
				sparseExec(t, db, `create index block_payload on blocks(payload_id)`)
			}
			tx, err := db.Begin()
			if err != nil {
				t.Fatal(err)
			}
			payload, err := tx.Prepare(`insert into payloads(id, body) values(?, ?)`)
			if err != nil {
				t.Fatal(err)
			}
			defer payload.Close()
			statement := `insert into blocks values(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`
			if inline {
				statement = `insert into inline_blocks values(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`
			}
			block, err := tx.Prepare(statement)
			if err != nil {
				t.Fatal(err)
			}
			defer block.Close()
			const blocks = 20_000
			payloadBytes := 0
			for i := range blocks {
				id := i%1000 + 1
				ordinal := i / 1000
				samples := capacitySamples(shape.kind, shape.count, shape.interval)
				for j := range samples {
					samples[j].at += int64(time.Duration(ordinal*shape.count) * shape.interval / time.Millisecond)
					samples[j].value += float64(id % 97)
				}
				packed := encode(samples, writer)
				payloadBytes += len(packed)
				s := summarise(samples)
				var body any = packed
				if !inline {
					body = i + 1
					if _, err = payload.Exec(i+1, packed); err != nil {
						t.Fatal(err)
					}
				}
				if _, err = block.Exec(id, s.startTS, s.endTS, s.count, s.min, s.max, s.sum,
					s.first, s.last, s.increase, s.resets, body); err != nil {
					t.Fatal(err)
				}
			}
			if err = tx.Commit(); err != nil {
				t.Fatal(err)
			}
			checkpoint(t, db)
			s := readCapacitySnapshot(t, db)
			table := "blocks"
			if inline {
				table = "inline_blocks"
			}
			queryStart := time.Now()
			for range 10 {
				var samples int
				var low, high, sum float64
				if err = db.QueryRow(`select sum(count), min(min), max(max), sum(sum) from `+table).
					Scan(&samples, &low, &high, &sum); err != nil {
					t.Fatal(err)
				}
				if samples != blocks*shape.count {
					t.Fatal("summary lost samples")
				}
			}
			t.Logf("%-14s %-16s payload %.3f B/sample sqlite %.3f B/sample summary scan %s (expiry + FK indexes)",
				shape.name, layout, float64(payloadBytes)/float64(blocks*shape.count), float64(s.physical)/float64(blocks*shape.count), time.Since(queryStart)/10)
			if err = db.Close(); err != nil {
				t.Fatal(err)
			}
		}
	}
}
