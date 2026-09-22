package spike

import (
	"encoding/json"
	"math"
	"path/filepath"
	"testing"

	"github.com/klauspost/compress/zstd"

	"github.com/tinyshed/tinystore/codec"
)

// TestGridFileDivision divides the winning ungrouped file into its b-trees at
// several page sizes, because a payload that shrank by a third only shrinks
// the file where the rows still land on the same pages.
func TestGridFileDivision(t *testing.T) {
	series := readCorpus(t)
	c, err := codec.New()
	if err != nil {
		t.Fatal(err)
	}
	defer c.Close()
	w, err := zstd.NewWriter(nil, zstd.WithEncoderConcurrency(1), zstd.WithWindowSize(8192), zstd.WithLowerEncoderMem(true))
	if err != nil {
		t.Fatal(err)
	}
	defer w.Close()
	r, err := zstd.NewReader(nil, zstd.WithDecoderConcurrency(1), zstd.WithDecoderMaxMemory(8192))
	if err != nil {
		t.Fatal(err)
	}
	defer r.Close()

	blocks := make([][]modelBlock, len(series))
	total, payloadBytes, controlBytes := 0, 0, 0
	for sid, s := range series {
		preferred := -1
		for start := 0; start < len(s.Values); start += 240 {
			points := corpusSamples(s, start, min(start+240, len(s.Values)))
			head, body, err := c.Encode(points)
			if err != nil {
				t.Fatal(err)
			}
			var chosen []byte
			if start == 0 {
				chosen, preferred = encodeGrid(t, c, w, points, body)
			} else {
				chosen, _ = encodeGrid(t, c, w, points, body, preferred)
			}
			for i, point := range decodeGrid(t, c, r, head, chosen) {
				if point.At != points[i].At || math.Float64bits(point.Value) != math.Float64bits(points[i].Value) {
					t.Fatal("model changed a sample")
				}
			}
			old := make([]sample, len(points))
			for i, point := range points {
				old[i] = sample{at: point.At, value: point.Value}
			}
			blocks[sid] = append(blocks[sid], modelBlock{head: head, summary: summarise(old), body: chosen})
			payloadBytes += len(chosen)
			controlBytes += len(body) + 1
			total += len(points)
		}
	}
	t.Logf("PAYLOAD control=%.4f learned=%.4f B/sample over %d samples",
		float64(controlBytes)/float64(total), float64(payloadBytes)/float64(total), total)

	for _, pageSize := range []int{1024, 2048, 4096, 8192, 16384} {
		schema := squeezedSchema("pages", true, true, false)
		schema.pageSize = pageSize
		schema.create = append(schema.create, `create table registry(id integer primary key, labels text not null) strict`)
		schema.indexes = append(schema.indexes, `create unique index registry_labels on registry(labels)`)
		db := openNight(t, filepath.Join(t.TempDir(), "pages.db"), schema)
		sparseExec(t, db, `pragma wal_autocheckpoint=0`)
		tx, err := db.Begin()
		if err != nil {
			t.Fatal(err)
		}
		id := 0
		for sid, all := range blocks {
			labels, labelErr := json.Marshal(series[sid].Metric)
			if labelErr != nil {
				t.Fatal(labelErr)
			}
			if _, err := tx.Exec(`insert into registry values(?,?)`, sid+1, string(labels)); err != nil {
				t.Fatal(err)
			}
			for _, b := range all {
				id++
				if _, err := tx.Exec(`insert into payloads values(?,?)`, id, b.body); err != nil {
					t.Fatal(err)
				}
				if _, err := tx.Exec(`insert into blocks values(?,?,?,?,?,?,?,?,?,?,?,?)`,
					sid+1, b.head.Start, b.head.End-b.head.Start, b.head.Count,
					b.summary.min, b.summary.max, b.summary.sum, b.head.First, b.summary.last,
					nil, nil, id); err != nil {
					t.Fatal(err)
				}
				if err := upsertState(tx, int64(sid+1), summary{startTS: b.head.Start, endTS: b.head.End}); err != nil {
					t.Fatal(err)
				}
			}
		}
		if commitErr := tx.Commit(); commitErr != nil {
			t.Fatal(commitErr)
		}
		checkpoint(t, db)

		positions := make([]int, len(series))
		func() {
			rows, queryErr := db.Query(`select b.series_id,b.start_ts,b.span,b.count,b.first,p.body from blocks b join payloads p on p.id=b.payload_id order by b.series_id,b.start_ts`)
			if queryErr != nil {
				t.Fatal(queryErr)
			}
			defer rows.Close()
			for rows.Next() {
				var sid int
				var h codec.Head
				var span int64
				var body []byte
				if scanErr := rows.Scan(&sid, &h.Start, &span, &h.Count, &h.First, &body); scanErr != nil {
					t.Fatal(scanErr)
				}
				h.End = h.Start + span
				for _, point := range decodeGrid(t, c, r, h, body) {
					i := positions[sid-1]
					expected := series[sid-1]
					if i >= len(expected.Values) || point.At != expected.Times[i] ||
						math.Float64bits(point.Value) != math.Float64bits(expected.Values[i]) {
						t.Fatal("SQLite round trip changed sample")
					}
					positions[sid-1]++
				}
			}
			if rowsErr := rows.Err(); rowsErr != nil {
				t.Fatal(rowsErr)
			}
		}()
		for sid, n := range positions {
			if n != len(series[sid].Values) {
				t.Fatal("SQLite lost samples")
			}
		}

		shares, size := nightShares(t, db, "payloads")
		t.Logf("PAGE %5d file=%7d B/sample=%.4f", pageSize, size, float64(size)/float64(total))
		for _, s := range shares {
			t.Logf("   %-16s %7d B  %.4f B/sample  unused %6d B", s.name, s.bytes, float64(s.bytes)/float64(total), s.unused)
		}
		db.Close()
	}
}
