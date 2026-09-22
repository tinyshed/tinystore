package spike

import (
	"bufio"
	"encoding/json"
	"math"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/klauspost/compress/zstd"

	"github.com/tinyshed/tinystore/codec"
)

// readJSONLCorpus reads the normalized corpus that bench/tsbs writes, which is
// also the body VictoriaMetrics imports, so all three engines see one input
func readJSONLCorpus(t testing.TB) []corpusSeries {
	t.Helper()
	path := os.Getenv("TINYSTORE_JSONL")
	if path == "" {
		t.Skip("set TINYSTORE_JSONL to the normalized corpus")
	}
	f, err := os.Open(path)
	if err != nil {
		t.Fatal(err)
	}
	defer f.Close()
	scanner := bufio.NewScanner(f)
	scanner.Buffer(make([]byte, 0, 1<<20), 1<<28)
	var out []corpusSeries
	for scanner.Scan() {
		var s corpusSeries
		if err := json.Unmarshal(scanner.Bytes(), &s); err != nil {
			t.Fatal(err)
		}
		if len(s.Values) != len(s.Times) {
			t.Fatal("series lengths disagree")
		}
		for i := 1; i < len(s.Times); i++ {
			if s.Times[i] <= s.Times[i-1] {
				t.Fatal("corpus is not ordered")
			}
		}
		out = append(out, s)
	}
	if err := scanner.Err(); err != nil {
		t.Fatal(err)
	}
	return out
}

func TestTSBSCorpus(t *testing.T) {
	series := readJSONLCorpus(t)
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
	total, controlBytes, learnedBytes, modelWins := 0, 0, 0, 0
	var controlTime, learnedTime time.Duration
	for sid, s := range series {
		preferred := -1
		for start := 0; start < len(s.Values); start += 240 {
			points := corpusSamples(s, start, min(start+240, len(s.Values)))
			begin := time.Now()
			head, body, encodeErr := c.Encode(points)
			controlTime += time.Since(begin)
			if encodeErr != nil {
				t.Fatal(encodeErr)
			}
			begin = time.Now()
			var chosen []byte
			if start == 0 {
				chosen, preferred = encodeGrid(t, c, w, points, body)
			} else {
				chosen, _ = encodeGrid(t, c, w, points, body, preferred)
			}
			learnedTime += time.Since(begin)
			if chosen[0] != 0 {
				modelWins++
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
			controlBytes += len(body) + 1
			learnedBytes += len(chosen)
			total += len(points)
		}
	}
	blockCount := 0
	for _, all := range blocks {
		blockCount += len(all)
	}
	t.Logf("SERIES %d BLOCKS %d SAMPLES %d", len(series), blockCount, total)
	t.Logf("PAYLOAD control=%.4f learned=%.4f B/sample, model chosen in %d of %d blocks",
		float64(controlBytes)/float64(total), float64(learnedBytes)/float64(total), modelWins, blockCount)
	t.Logf("ENCODE control=%s learned-total=%s", controlTime, learnedTime)

	schema := squeezedSchema("tsbs", true, true, false)
	schema.create = append(schema.create, `create table registry(id integer primary key, labels text not null) strict`)
	schema.indexes = append(schema.indexes, `create unique index registry_labels on registry(labels)`)
	db := openNight(t, filepath.Join(t.TempDir(), "tsbs.db"), schema)
	defer db.Close()
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
	t.Logf("FILE bytes=%d B/sample=%.4f", size, float64(size)/float64(total))
	for _, s := range shares {
		t.Logf("   %-16s %9d B  %.4f B/sample  unused %8d B", s.name, s.bytes, float64(s.bytes)/float64(total), s.unused)
	}
}
