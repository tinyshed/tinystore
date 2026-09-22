package spike

import (
	"encoding/binary"
	"encoding/json"
	"math"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/klauspost/compress/zstd"

	"github.com/tinyshed/tinystore/codec"
)

// encodeGrid is the model of TestGridDiagnosis as real bytes: a decimal grid,
// the integers on it, and the exact bit corrections in whichever residual mode
// is smallest. It keeps the plain codec whenever the model does not pay.
func encodeGrid(t *testing.T, c *codec.Codec, w *zstd.Encoder, samples []codec.Sample, base []byte, scales ...int) ([]byte, int) {
	t.Helper()
	best, chosen := append([]byte{0}, base...), -1
	if len(scales) == 0 {
		for k := range 16 {
			scales = append(scales, k)
		}
	}
	for _, scale := range scales {
		if scale < 0 {
			continue
		}
		q, residual, ok := quantise(samples, math.Pow10(scale))
		if !ok {
			continue
		}
		_, body, err := c.Encode(q)
		if err != nil {
			t.Fatal(err)
		}
		if len(body) > math.MaxUint16 {
			continue
		}
		out := []byte{1, byte(scale)}
		out = binary.LittleEndian.AppendUint16(out, uint16(len(body)))
		out = append(out, body...)
		out = append(out, encodeResiduals(w, residual)...)
		out = binary.LittleEndian.AppendUint32(out, modelChecksum(codec.Head{
			Start: samples[0].At, End: samples[len(samples)-1].At,
			Count: len(samples), First: samples[0].Value,
		}, out))
		if len(out) < len(best) {
			best, chosen = out, scale
		}
	}
	return best, chosen
}

func decodeGrid(t *testing.T, c *codec.Codec, r *zstd.Decoder, h codec.Head, body []byte) []codec.Sample {
	t.Helper()
	original := h
	var residual []int64
	factor := 0.0
	if body[0] == 0 {
		body = body[1:]
	} else {
		if modelChecksum(h, body[:len(body)-4]) != binary.LittleEndian.Uint32(body[len(body)-4:]) {
			t.Fatal("model checksum")
		}
		factor = math.Pow10(int(body[1]))
		h.First = math.Round(h.First * factor)
		n := int(binary.LittleEndian.Uint16(body[2:]))
		residual = decodeResiduals(t, r, body[4+n:len(body)-4], h.Count-1)
		body = body[4 : 4+n]
	}
	it, err := c.Decode(h, body)
	if err != nil {
		t.Fatal(err)
	}
	out := make([]codec.Sample, 0, h.Count)
	for it.Next() {
		s := it.Sample()
		if factor != 0 {
			if len(out) == 0 {
				s.Value = original.First
			} else {
				s.Value = fromOrdered(orderedFloat(s.Value/factor) + uint64(residual[len(out)-1]))
			}
		}
		out = append(out, s)
	}
	if it.Err() != nil || len(out) != h.Count {
		t.Fatal("model decode", it.Err())
	}
	return out
}

func TestGroupSizeKnee(t *testing.T) {
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

	control := make([][]modelBlock, len(series))
	learned := make([][]modelBlock, len(series))
	total, controlBytes, learnedBytes := 0, 0, 0
	var searchTime time.Duration
	for sid, s := range series {
		preferred := -1
		for start := 0; start < len(s.Values); start += 240 {
			points := corpusSamples(s, start, min(start+240, len(s.Values)))
			head, body, err := c.Encode(points)
			if err != nil {
				t.Fatal(err)
			}
			begin := time.Now()
			var chosen []byte
			if start == 0 {
				chosen, preferred = encodeGrid(t, c, w, points, body)
			} else {
				chosen, _ = encodeGrid(t, c, w, points, body, preferred)
			}
			searchTime += time.Since(begin)
			for i, point := range decodeGrid(t, c, r, head, chosen) {
				if point.At != points[i].At || math.Float64bits(point.Value) != math.Float64bits(points[i].Value) {
					t.Fatal("model changed a sample")
				}
			}
			old := make([]sample, len(points))
			for i, point := range points {
				old[i] = sample{at: point.At, value: point.Value}
			}
			b := modelBlock{head: head, summary: summarise(old), body: append([]byte{0}, body...)}
			control[sid] = append(control[sid], b)
			b.body = chosen
			learned[sid] = append(learned[sid], b)
			controlBytes += len(body) + 1
			learnedBytes += len(chosen)
			total += len(points)
		}
	}
	t.Logf("PAYLOAD control=%.4f learned=%.4f B/sample, scale search %s",
		float64(controlBytes)/float64(total), float64(learnedBytes)/float64(total), searchTime)

	for _, version := range []struct {
		name string
		all  [][]modelBlock
	}{{"control", control}, {"learned", learned}} {
		for _, width := range []int{1, 2, 4, 8, 16, 32} {
			schema := squeezedSchema("groups", true, true, false)
			schema.create[1] = strings.Replace(schema.create[1], "payload_id integer not null,", "payload_id integer not null, directory blob,", 1)
			schema.create = append(schema.create, `create table registry(id integer primary key, labels text not null) strict`)
			schema.indexes = append(schema.indexes, `create unique index registry_labels on registry(labels)`)
			db := openNight(t, filepath.Join(t.TempDir(), "knee.db"), schema)
			sparseExec(t, db, `pragma wal_autocheckpoint=0`)
			tx, err := db.Begin()
			if err != nil {
				t.Fatal(err)
			}
			id := 0
			for sid, blocks := range version.all {
				labels, labelErr := json.Marshal(series[sid].Metric)
				if labelErr != nil {
					t.Fatal(labelErr)
				}
				if _, err := tx.Exec(`insert into registry values(?,?)`, sid+1, string(labels)); err != nil {
					t.Fatal(err)
				}
				for start := 0; start < len(blocks); start += width {
					part := blocks[start:min(start+width, len(blocks))]
					var dir []byte
					body := part[0].body
					if len(part) > 1 {
						dir, body = groupDirectory(w, part)
					}
					first, last := part[0], part[len(part)-1]
					low, high, sum, count := first.summary.min, first.summary.max, 0.0, 0
					for _, b := range part {
						low = min(low, b.summary.min)
						high = max(high, b.summary.max)
						sum += b.summary.sum
						count += b.head.Count
					}
					id++
					if _, err := tx.Exec(`insert into payloads values(?,?)`, id, body); err != nil {
						t.Fatal(err)
					}
					if _, err := tx.Exec(`insert into blocks values(?,?,?,?,?,?,?,?,?,?,?,?,?)`,
						sid+1, first.head.Start, last.head.End-first.head.Start, count, low, high, sum,
						first.head.First, last.summary.last, nil, nil, id, dir); err != nil {
						t.Fatal(err)
					}
					if err := upsertState(tx, int64(sid+1), summary{startTS: first.head.Start, endTS: last.head.End}); err != nil {
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
				rows, queryErr := db.Query(`select b.series_id,b.start_ts,b.span,b.count,b.first,b.directory,p.body from blocks b join payloads p on p.id=b.payload_id order by b.series_id,b.start_ts`)
				if queryErr != nil {
					t.Fatal(queryErr)
				}
				defer rows.Close()
				for rows.Next() {
					var sid int
					var h codec.Head
					var span int64
					var dir, body []byte
					if scanErr := rows.Scan(&sid, &h.Start, &span, &h.Count, &h.First, &dir, &body); scanErr != nil {
						t.Fatal(scanErr)
					}
					h.End = h.Start + span
					part := []modelBlock{{head: h, body: body}}
					if dir != nil {
						part = readDirectory(t, r, dir, body)
					}
					for _, b := range part {
						for _, point := range decodeGrid(t, c, r, b.head, b.body) {
							i := positions[sid-1]
							expected := series[sid-1]
							if i >= len(expected.Values) || point.At != expected.Times[i] ||
								math.Float64bits(point.Value) != math.Float64bits(expected.Values[i]) {
								t.Fatal("SQLite round trip changed sample")
							}
							positions[sid-1]++
						}
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

			_, size := nightShares(t, db, "payloads")
			pageReads := func(bytes int) float64 { return math.Ceil(float64(bytes) / 4096) }

			const runs = 200
			at := series[0].Times[1320]
			begin := time.Now()
			fetched, pages := 0, 0.0
			for range runs {
				var h codec.Head
				var span int64
				var dir, body []byte
				if err := db.QueryRow(`select b.start_ts,b.span,b.count,b.first,b.directory,p.body from blocks b join payloads p on p.id=b.payload_id where b.series_id=1 and b.start_ts<=? order by b.start_ts desc limit 1`, at).
					Scan(&h.Start, &span, &h.Count, &h.First, &dir, &body); err != nil {
					t.Fatal(err)
				}
				h.End = h.Start + span
				fetched, pages = len(dir)+len(body), pageReads(len(dir)+len(body))
				part := []modelBlock{{head: h, body: body}}
				if dir != nil {
					part = readDirectory(t, r, dir, body)
				}
				found := false
				for _, b := range part {
					if b.head.Start <= at && b.head.End >= at {
						for _, s := range decodeGrid(t, c, r, b.head, b.body) {
							if s.At == at && math.Float64bits(s.Value) == math.Float64bits(series[0].Values[1320]) {
								found = true
							}
						}
					}
				}
				if !found {
					t.Fatal("point missing")
				}
			}
			point := time.Since(begin) / runs

			// a day of five-minute samples, the shape a dashboard panel asks for
			from, to := series[0].Times[1320], series[0].Times[1320]+24*3600*1000
			begin = time.Now()
			rangeFetched, rangeDecoded := 0, 0
			for range runs {
				func() {
					rows, queryErr := db.Query(`select b.start_ts,b.span,b.count,b.first,b.directory,p.body from blocks b join payloads p on p.id=b.payload_id where b.series_id=1 and b.start_ts<=? and b.start_ts+b.span>=? order by b.start_ts`, to, from)
					if queryErr != nil {
						t.Fatal(queryErr)
					}
					defer rows.Close()
					rangeFetched, rangeDecoded = 0, 0
					for rows.Next() {
						var h codec.Head
						var span int64
						var dir, body []byte
						if scanErr := rows.Scan(&h.Start, &span, &h.Count, &h.First, &dir, &body); scanErr != nil {
							t.Fatal(scanErr)
						}
						h.End = h.Start + span
						rangeFetched += len(dir) + len(body)
						part := []modelBlock{{head: h, body: body}}
						if dir != nil {
							part = readDirectory(t, r, dir, body)
						}
						for _, b := range part {
							if b.head.End < from || b.head.Start > to {
								continue
							}
							for _, s := range decodeGrid(t, c, r, b.head, b.body) {
								if s.At >= from && s.At <= to {
									rangeDecoded++
								}
							}
						}
					}
					if rowsErr := rows.Err(); rowsErr != nil {
						t.Fatal(rowsErr)
					}
				}()
			}
			window := time.Since(begin) / runs

			begin = time.Now()
			for range runs {
				var count int
				if err := db.QueryRow(`select sum(count) from blocks`).Scan(&count); err != nil {
					t.Fatal(err)
				}
				if count != total {
					t.Fatal("summary count")
				}
			}
			t.Logf("KNEE %-8s group=%-3d file=%7d B/sample=%.4f | point %6s fetched=%5d pages=%2.0f | day %6s fetched=%6d points=%4d | scan %s",
				version.name, width, size, float64(size)/float64(total),
				point, fetched, pages, window, rangeFetched, rangeDecoded, time.Since(begin)/runs)
			db.Close()
		}
	}
}
