package spike

import (
	"database/sql"
	"encoding/binary"
	"encoding/json"
	"hash/crc32"
	"math"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/klauspost/compress/zstd"

	"github.com/tinyshed/tinystore/codec"
)

type modelBlock struct {
	head    codec.Head
	summary summary
	body    []byte
}

func orderedFloat(v float64) uint64 {
	u := math.Float64bits(v)
	if u>>63 != 0 {
		return ^u
	}
	return u ^ (1 << 63)
}

func fromOrdered(u uint64) float64 {
	if u>>63 == 0 {
		return math.Float64frombits(^u)
	}
	return math.Float64frombits(u ^ (1 << 63))
}

func modelChecksum(h codec.Head, body []byte) uint32 {
	raw := binary.LittleEndian.AppendUint64(nil, uint64(h.Start))
	raw = binary.LittleEndian.AppendUint64(raw, uint64(h.End))
	raw = binary.LittleEndian.AppendUint64(raw, uint64(h.Count))
	raw = binary.LittleEndian.AppendUint64(raw, math.Float64bits(h.First))
	return crc32.Update(crc32.ChecksumIEEE(raw), crc32.IEEETable, body)
}

func encodeModel(t *testing.T, c *codec.Codec, w *zstd.Encoder, samples []codec.Sample, base []byte, scales ...int) ([]byte, int) {
	t.Helper()
	best := append([]byte{0}, base...)
	chosen := -1
	if len(scales) == 0 {
		scales = []int{0, 1, 2, 3, 4, 5, 6, 7, 8, 9}
	}
	for _, scale := range scales {
		if scale < 0 {
			continue
		}
		factor := math.Pow10(scale)
		quantized := make([]codec.Sample, len(samples))
		var corrections []byte
		valid := true
		for i, s := range samples {
			q := math.Round(s.Value * factor)
			if math.IsNaN(q) || math.IsInf(q, 0) || math.Abs(q) > 0x1p53 {
				valid = false
				break
			}
			quantized[i] = codec.Sample{At: s.At, Value: q}
			if i > 0 {
				delta := int64(orderedFloat(s.Value) - orderedFloat(q/factor))
				corrections = binary.AppendVarint(corrections, delta)
			}
		}
		if !valid {
			continue
		}
		head, body, err := c.Encode(quantized)
		if err != nil {
			t.Fatal(err)
		}
		compressed := w.EncodeAll(corrections, nil)
		flag := byte(0)
		if len(compressed) < len(corrections) {
			corrections = compressed
			flag = 1
		}
		out := []byte{1, byte(scale), flag}
		out = binary.LittleEndian.AppendUint16(out, uint16(len(body)))
		out = append(out, body...)
		out = append(out, corrections...)
		head.First = samples[0].Value
		out = binary.LittleEndian.AppendUint32(out, modelChecksum(head, out))
		if len(out) < len(best) {
			best = out
			chosen = scale
		}
	}
	return best, chosen
}

func decodeModel(t *testing.T, c *codec.Codec, r *zstd.Decoder, h codec.Head, body []byte) []codec.Sample {
	t.Helper()
	original := h
	var correction []byte
	factor := 0.0
	if body[0] == 0 {
		body = body[1:]
	} else {
		if modelChecksum(h, body[:len(body)-4]) != binary.LittleEndian.Uint32(body[len(body)-4:]) {
			t.Fatal("model checksum")
		}
		factor = math.Pow10(int(body[1]))
		h.First = math.Round(h.First * factor)
		n := int(binary.LittleEndian.Uint16(body[3:]))
		correction = body[5+n : len(body)-4]
		if body[2] == 1 {
			var err error
			correction, err = r.DecodeAll(correction, nil)
			if err != nil {
				t.Fatal(err)
			}
		}
		body = body[5 : 5+n]
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
				delta, n := binary.Varint(correction)
				if n <= 0 {
					t.Fatal("model correction")
				}
				correction = correction[n:]
				s.Value = fromOrdered(orderedFloat(s.Value/factor) + uint64(delta))
			}
		}
		out = append(out, s)
	}
	if it.Err() != nil || len(out) != h.Count || len(correction) != 0 {
		t.Fatal("model decode", it.Err())
	}
	return out
}

func groupDirectory(w *zstd.Encoder, blocks []modelBlock) ([]byte, []byte) {
	var body, raw []byte
	rows := make([][10]uint64, len(blocks))
	for i, b := range blocks {
		rows[i] = [10]uint64{uint64(b.head.Start), uint64(b.head.End), uint64(b.head.Count), math.Float64bits(b.head.First), math.Float64bits(b.summary.min), math.Float64bits(b.summary.max), math.Float64bits(b.summary.sum), math.Float64bits(b.summary.last), uint64(len(body)), uint64(len(b.body))}
		body = append(body, b.body...)
	}
	for col := range 10 {
		for _, row := range rows {
			raw = binary.LittleEndian.AppendUint64(raw, row[col])
		}
	}
	dir := []byte{byte(len(blocks))}
	dir = binary.LittleEndian.AppendUint32(dir, crc32.ChecksumIEEE(raw))
	dir = append(dir, w.EncodeAll(raw, nil)...)
	return dir, body
}

func readDirectory(t *testing.T, r *zstd.Decoder, dir, body []byte) []modelBlock {
	t.Helper()
	raw, err := r.DecodeAll(dir[5:], nil)
	if err != nil {
		t.Fatal(err)
	}
	n := int(dir[0])
	if len(raw) != 80*n || crc32.ChecksumIEEE(raw) != binary.LittleEndian.Uint32(dir[1:]) {
		t.Fatal("directory checksum")
	}
	out := make([]modelBlock, n)
	for i := range n {
		var row [10]uint64
		for col := range 10 {
			row[col] = binary.LittleEndian.Uint64(raw[(col*n+i)*8:])
		}
		if row[8] > uint64(len(body)) || row[9] > uint64(len(body))-row[8] {
			t.Fatal("directory bounds")
		}
		out[i] = modelBlock{head: codec.Head{Start: int64(row[0]), End: int64(row[1]), Count: int(row[2]), First: math.Float64frombits(row[3])}, summary: summary{min: math.Float64frombits(row[4]), max: math.Float64frombits(row[5]), sum: math.Float64frombits(row[6]), last: math.Float64frombits(row[7])}, body: body[row[8] : row[8]+row[9]]}
	}
	return out
}

func TestModelAndGroupsOnCorpus(t *testing.T) {
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
	var versions [3][][]modelBlock
	versions[0] = make([][]modelBlock, len(series))
	versions[1] = make([][]modelBlock, len(series))
	versions[2] = make([][]modelBlock, len(series))
	total, baseBytes, modelBytes := 0, 0, 0
	var baseTime, modelTime time.Duration
	var trainedTime time.Duration
	trainedBytes := 0
	scales := map[int]int{}
	for sid, s := range series {
		preferred := -1
		baseSum, modelSum := 0, 0
		for start := 0; start < len(s.Values); start += 240 {
			samples := corpusSamples(s, start, min(start+240, len(s.Values)))
			begin := time.Now()
			head, body, err := c.Encode(samples)
			baseTime += time.Since(begin)
			if err != nil {
				t.Fatal(err)
			}
			begin = time.Now()
			candidate, scale := encodeModel(t, c, w, samples, body)
			searchTime := time.Since(begin)
			modelTime += searchTime
			trained := candidate
			if start == 0 {
				preferred = scale
				trainedTime += searchTime
			} else {
				begin = time.Now()
				trained, _ = encodeModel(t, c, w, samples, body, preferred)
				trainedTime += time.Since(begin)
			}
			trainedBytes += len(trained)
			scales[scale]++
			back := decodeModel(t, c, r, head, candidate)
			trainedBack := decodeModel(t, c, r, head, trained)
			old := make([]sample, len(samples))
			for i, point := range samples {
				if back[i].At != point.At || math.Float64bits(back[i].Value) != math.Float64bits(point.Value) {
					t.Fatal("model changed a sample")
				}
				if trainedBack[i].At != point.At || math.Float64bits(trainedBack[i].Value) != math.Float64bits(point.Value) {
					t.Fatal("trained model changed sample")
				}
				old[i] = sample{at: point.At, value: point.Value}
			}
			b := modelBlock{head: head, summary: summarise(old), body: append([]byte{0}, body...)}
			versions[0][sid] = append(versions[0][sid], b)
			b.body = candidate
			versions[1][sid] = append(versions[1][sid], b)
			b.body = trained
			versions[2][sid] = append(versions[2][sid], b)
			baseSum += len(body) + 1
			modelSum += len(candidate)
			total += len(samples)
		}
		baseBytes += baseSum
		modelBytes += modelSum
		t.Logf("MODEL %s n=%d base=%.4f candidate=%.4f", s.Metric["series"], len(s.Values), float64(baseSum)/float64(len(s.Values)), float64(modelSum)/float64(len(s.Values)))
	}
	t.Logf("TOTAL n=%d base=%.4f model=%.4f scales=%v encode baseline=%s model-search-extra=%s", total, float64(baseBytes)/float64(total), float64(modelBytes)/float64(total), scales, baseTime, modelTime)
	t.Logf("TRAINED first block per series, payload=%.4f search-extra=%s", float64(trainedBytes)/float64(total), trainedTime)
	for version, all := range versions {
		for _, width := range []int{1, 8, 16} {
			schema := squeezedSchema("groups", true, true, false)
			schema.create[1] = strings.Replace(schema.create[1], "payload_id integer not null,", "payload_id integer not null, directory blob,", 1)
			schema.create = append(schema.create, `create table registry(id integer primary key, labels text not null) strict`)
			schema.indexes = append(schema.indexes, `create unique index registry_labels on registry(labels)`)
			db := openNight(t, filepath.Join(t.TempDir(), "groups.db"), schema)
			sparseExec(t, db, `pragma wal_autocheckpoint=0`)
			tx, err := db.Begin()
			if err != nil {
				t.Fatal(err)
			}
			id := 0
			for sid, blocks := range all {
				labels, err := json.Marshal(series[sid].Metric)
				if err != nil {
					t.Fatal(err)
				}
				if _, err := tx.Exec(`insert into registry values(?,?)`, sid+1, string(labels)); err != nil {
					t.Fatal(err)
				}
				for start := 0; start < len(blocks); start += width {
					part := blocks[start:min(start+width, len(blocks))]
					var dir []byte
					body := part[0].body
					if width > 1 {
						dir, body = groupDirectory(w, part)
						restored := readDirectory(t, r, dir, body)
						for i, b := range restored {
							if b.head != part[i].head || math.Float64bits(b.summary.sum) != math.Float64bits(part[i].summary.sum) {
								t.Fatal("directory changed metadata")
							}
							decodeModel(t, c, r, b.head, b.body)
						}
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
					if _, err := tx.Exec(`insert into blocks values(?,?,?,?,?,?,?,?,?,?,?,?,?)`, sid+1, first.head.Start, last.head.End-first.head.Start, count, low, high, sum, first.head.First, last.summary.last, nil, nil, id, dir); err != nil {
						t.Fatal(err)
					}
					if err := upsertState(tx, int64(sid+1), summary{startTS: first.head.Start, endTS: last.head.End}); err != nil {
						t.Fatal(err)
					}
				}
			}
			if err := tx.Commit(); err != nil {
				t.Fatal(err)
			}
			checkpoint(t, db)
			rows, readErr := db.Query(`select b.series_id,b.start_ts,b.span,b.count,b.first,b.directory,p.body from blocks b join payloads p on p.id=b.payload_id order by b.series_id,b.start_ts`)
			if readErr != nil {
				t.Fatal(readErr)
			}
			defer rows.Close()
			positions := make([]int, len(series))
			for rows.Next() {
				var sid int
				var h codec.Head
				var span int64
				var dir, body []byte
				if err := rows.Scan(&sid, &h.Start, &span, &h.Count, &h.First, &dir, &body); err != nil {
					t.Fatal(err)
				}
				h.End = h.Start + span
				part := []modelBlock{{head: h, body: body}}
				if dir != nil {
					part = readDirectory(t, r, dir, body)
				}
				for _, b := range part {
					for _, point := range decodeModel(t, c, r, b.head, b.body) {
						i := positions[sid-1]
						expected := series[sid-1]
						if i >= len(expected.Values) || point.At != expected.Times[i] || math.Float64bits(point.Value) != math.Float64bits(expected.Values[i]) {
							t.Fatal("SQLite round trip changed sample")
						}
						positions[sid-1]++
					}
				}
			}
			if err := rows.Err(); err != nil {
				t.Fatal(err)
			}
			for sid, n := range positions {
				if n != len(series[sid].Values) {
					t.Fatal("SQLite lost samples")
				}
			}
			shares, size := nightShares(t, db, "payloads")
			var waste int64
			for _, s := range shares {
				waste += s.unused
			}
			at := series[0].Times[1320]
			const runs = 200
			begin := time.Now()
			fetched := 0
			for range runs {
				var h codec.Head
				var span int64
				var dir, body []byte
				if err := db.QueryRow(`select b.start_ts,b.span,b.count,b.first,b.directory,p.body from blocks b join payloads p on p.id=b.payload_id where b.series_id=1 and b.start_ts<=? order by b.start_ts desc limit 1`, at).Scan(&h.Start, &span, &h.Count, &h.First, &dir, &body); err != nil {
					t.Fatal(err)
				}
				h.End = h.Start + span
				fetched = len(dir) + len(body)
				part := []modelBlock{{head: h, body: body}}
				if dir != nil {
					part = readDirectory(t, r, dir, body)
				}
				found := false
				for _, b := range part {
					if b.head.Start <= at && b.head.End >= at {
						for _, s := range decodeModel(t, c, r, b.head, b.body) {
							if s.At == at {
								found = true
								if math.Float64bits(s.Value) != math.Float64bits(series[0].Values[1320]) {
									t.Fatal("point changed")
								}
							}
						}
					}
				}
				if !found {
					t.Fatal("point missing")
				}
			}
			t.Logf("FILE version=%d group=%d bytes=%d B/sample=%.4f waste=%.4f warm-point=%s fetched=%d", version, width, size, float64(size)/float64(total), float64(waste)/float64(total), time.Since(begin)/runs, fetched)
			begin = time.Now()
			for range runs {
				var count int
				var sum sql.NullFloat64
				if err := db.QueryRow(`select sum(count),sum(sum) from blocks`).Scan(&count, &sum); err != nil {
					t.Fatal(err)
				}
				if count != total {
					t.Fatal("summary count")
				}
			}
			t.Logf("SUMMARY version=%d group=%d scan=%s", version, width, time.Since(begin)/runs)
			db.Close()
		}
	}
}
