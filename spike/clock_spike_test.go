package spike

import (
	"crypto/sha256"
	"encoding/binary"
	"encoding/json"
	"hash/crc32"
	"math"
	"path/filepath"
	"slices"
	"sort"
	"strings"
	"testing"

	"github.com/klauspost/compress/zstd"

	"github.com/tinyshed/tinystore/codec"
)

func clockGCD(a, b uint64) uint64 {
	for b != 0 {
		a, b = b, a%b
	}
	return a
}

func compactClockDirectory(w *zstd.Encoder, blocks []modelBlock) ([]byte, []byte) {
	anchor := blocks[0].head.Start
	unit := uint64(0)
	for _, b := range blocks {
		unit = clockGCD(unit, uint64(b.head.Start)-uint64(anchor))
		unit = clockGCD(unit, uint64(b.head.End)-uint64(anchor))
	}
	if unit == 0 {
		unit = 1
	}
	raw := binary.LittleEndian.AppendUint64(nil, uint64(anchor))
	raw = binary.AppendUvarint(raw, unit)
	var body []byte
	previous := anchor
	for _, b := range blocks {
		raw = binary.AppendUvarint(raw, (uint64(b.head.Start)-uint64(previous))/unit)
		raw = binary.AppendUvarint(raw, (uint64(b.head.End)-uint64(b.head.Start))/unit)
		previous = b.head.End
		raw = binary.AppendUvarint(raw, uint64(b.head.Count))
		raw = binary.AppendUvarint(raw, uint64(len(b.body)))
		for _, v := range []float64{b.head.First, b.summary.min, b.summary.max, b.summary.sum, b.summary.last} {
			written := false
			for kind, factor := range []float64{1, 100} {
				q := math.Round(v * factor)
				if math.IsNaN(q) || math.IsInf(q, 0) || math.Abs(q) >= 0x1p60 {
					continue
				}
				n := int64(q)
				if math.Float64bits(float64(n)/factor) != math.Float64bits(v) {
					continue
				}
				z := uint64(n)<<1 ^ uint64(n>>63)
				raw = binary.AppendUvarint(raw, z<<2|uint64(kind))
				written = true
				break
			}
			if !written {
				raw = append(raw, 2)
				raw = binary.LittleEndian.AppendUint64(raw, math.Float64bits(v))
			}
		}
		body = append(body, b.body...)
	}
	dir := binary.LittleEndian.AppendUint32([]byte{byte(len(blocks))}, crc32.ChecksumIEEE(raw))
	return append(dir, w.EncodeAll(raw, nil)...), body
}

func readCompactClockDirectory(t *testing.T, r *zstd.Decoder, dir, body []byte) []modelBlock {
	t.Helper()
	raw, err := r.DecodeAll(dir[5:], nil)
	if err != nil {
		t.Fatal(err)
	}
	if crc32.ChecksumIEEE(raw) != binary.LittleEndian.Uint32(dir[1:]) || len(raw) < 8 {
		t.Fatal("compact directory checksum")
	}
	previous := int64(binary.LittleEndian.Uint64(raw))
	raw = raw[8:]
	take := func() uint64 {
		v, n := binary.Uvarint(raw)
		if n <= 0 {
			t.Fatal("compact directory varint")
		}
		raw = raw[n:]
		return v
	}
	unit := take()
	out := make([]modelBlock, int(dir[0]))
	offset := 0
	for i := range out {
		start := int64(uint64(previous) + take()*unit)
		end := int64(uint64(start) + take()*unit)
		previous = end
		count, length := int(take()), int(take())
		var values [5]float64
		for j := range values {
			token := take()
			if token == 2 {
				if len(raw) < 8 {
					t.Fatal("compact float")
				}
				values[j] = math.Float64frombits(binary.LittleEndian.Uint64(raw))
				raw = raw[8:]
			} else {
				kind, z := token&3, token>>2
				if kind > 1 {
					t.Fatal("compact float tag")
				}
				n := int64(z>>1) ^ -int64(z&1)
				values[j] = float64(n)
				if kind == 1 {
					values[j] /= 100
				}
			}
		}
		if length < 0 || offset > len(body)-length {
			t.Fatal("compact body bounds")
		}
		out[i] = modelBlock{head: codec.Head{Start: start, End: end, Count: count, First: values[0]}, summary: summary{min: values[1], max: values[2], sum: values[3], last: values[4]}, body: body[offset : offset+length]}
		offset += length
	}
	if len(raw) != 0 || offset != len(body) {
		t.Fatal("compact trailing bytes")
	}
	return out
}

func encodeClock(w *zstd.Encoder, times []int64) []byte {
	if len(times) < 2 {
		return nil
	}
	deltas := make([]uint64, len(times)-1)
	gcd := uint64(0)
	freq := map[uint64]int{}
	for i := range deltas {
		deltas[i] = uint64(times[i+1]) - uint64(times[i])
		gcd = clockGCD(gcd, deltas[i])
		freq[deltas[i]]++
	}
	common := deltas[0]
	for d, n := range freq {
		if n > freq[common] || (n == freq[common] && d < common) {
			common = d
		}
	}
	var best []byte
	for mode := range byte(3) {
		raw := binary.AppendUvarint([]byte{mode}, gcd)
		switch mode {
		case 0:
			for _, d := range deltas {
				raw = binary.AppendUvarint(raw, d/gcd)
			}
		case 1:
			for i := 0; i < len(deltas); {
				j := i + 1
				for j < len(deltas) && deltas[j] == deltas[i] {
					j++
				}
				raw = binary.AppendUvarint(raw, uint64(j-i))
				raw = binary.AppendUvarint(raw, deltas[i]/gcd)
				i = j
			}
		case 2:
			raw = binary.AppendUvarint(raw, common/gcd)
			previous := -1
			for i, d := range deltas {
				if d != common {
					raw = binary.AppendUvarint(raw, uint64(i-previous))
					raw = binary.AppendUvarint(raw, d/gcd)
					previous = i
				}
			}
		}
		candidate := append([]byte{0}, raw...)
		compressed := w.EncodeAll(raw, nil)
		if len(compressed) < len(raw) {
			candidate = append([]byte{1}, compressed...)
		}
		if best == nil || len(candidate) < len(best) {
			best = candidate
		}
	}
	return best
}

func decodeClock(t *testing.T, r *zstd.Decoder, h codec.Head, body []byte) []int64 {
	t.Helper()
	if h.Count == 1 {
		return []int64{h.Start}
	}
	raw := body[1:]
	if body[0] == 1 {
		var err error
		raw, err = r.DecodeAll(raw, nil)
		if err != nil {
			t.Fatal(err)
		}
	}
	mode := raw[0]
	raw = raw[1:]
	take := func() uint64 {
		v, n := binary.Uvarint(raw)
		if n <= 0 {
			t.Fatal("clock varint")
		}
		raw = raw[n:]
		return v
	}
	gcd := take()
	deltas := make([]uint64, h.Count-1)
	switch mode {
	case 0:
		for i := range deltas {
			deltas[i] = take()
		}
	case 1:
		for i := 0; i < len(deltas); {
			n, d := int(take()), take()
			if n < 1 || n > len(deltas)-i {
				t.Fatal("clock run")
			}
			for j := range n {
				deltas[i+j] = d
			}
			i += n
		}
	case 2:
		common := take()
		for i := range deltas {
			deltas[i] = common
		}
		pos := -1
		for len(raw) > 0 {
			gap := int(take())
			if gap < 1 || gap > len(deltas)-1-pos {
				t.Fatal("clock exception")
			}
			pos += gap
			deltas[pos] = take()
		}
	default:
		t.Fatal("clock mode")
	}
	if len(raw) != 0 || gcd == 0 {
		t.Fatal("clock trailing bytes")
	}
	times := make([]int64, h.Count)
	times[0] = h.Start
	for i, d := range deltas {
		if d == 0 || d > math.MaxUint64/gcd {
			t.Fatal("clock delta")
		}
		delta := d * gcd
		if delta > uint64(math.MaxInt64)-uint64(times[i]) {
			t.Fatal("clock overflow")
		}
		times[i+1] = int64(uint64(times[i]) + delta)
	}
	if times[len(times)-1] != h.End {
		t.Fatal("clock end")
	}
	return times
}

func decodeClockBlock(t *testing.T, c *codec.Codec, r *zstd.Decoder, h codec.Head, body []byte) []codec.Sample {
	t.Helper()
	if body[0] == 249 {
		return decodeInlineTrick(t, c, r, h, body)
	}
	if body[0] != 250 {
		return decodeGrid(t, c, r, h, body)
	}
	if modelChecksum(h, body[:len(body)-4]) != binary.LittleEndian.Uint32(body[len(body)-4:]) {
		t.Fatal("clock checksum")
	}
	n := int(binary.LittleEndian.Uint16(body[1:]))
	times := decodeClock(t, r, h, body[3:3+n])
	flat := h
	flat.Start = 0
	flat.End = int64(h.Count - 1)
	out := decodeGrid(t, c, r, flat, body[3+n:len(body)-4])
	for i := range out {
		out[i].At = times[i]
	}
	return out
}

func TestClockAndMetadataOnCorpus(t *testing.T) {
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
	var versions [2][][]modelBlock
	for i := range versions {
		versions[i] = make([][]modelBlock, len(series))
	}
	hist := map[int64]int{}
	axes := map[[32]byte]int{}
	allGCD := uint64(0)
	sampleCount, baseBytes, newBytes, wins := 0, 0, 0, 0
	clockBytes, uniqueClockBytes := 0, 0
	clockSet := map[string]bool{}
	for sid, s := range series {
		axis := make([]byte, 0, 8*len(s.Times))
		for i, at := range s.Times {
			axis = binary.LittleEndian.AppendUint64(axis, uint64(at))
			if i > 0 {
				d := at - s.Times[i-1]
				hist[d]++
				allGCD = clockGCD(allGCD, uint64(d))
			}
		}
		hash := sha256.Sum256(axis)
		if previous, ok := axes[hash]; ok {
			if !slices.Equal(s.Times, series[previous].Times) {
				t.Fatal("axis hash collision")
			}
		} else {
			axes[hash] = sid
		}
		preferred, flatPreferred := -1, -1
		for start := 0; start < len(s.Values); start += 240 {
			points := corpusSamples(s, start, min(start+240, len(s.Values)))
			head, body, err := c.Encode(points)
			if err != nil {
				t.Fatal(err)
			}
			var base []byte
			if start == 0 {
				base, preferred = encodeGrid(t, c, w, points, body)
			} else {
				base, _ = encodeGrid(t, c, w, points, body, preferred)
			}
			flat := append([]codec.Sample(nil), points...)
			for i := range flat {
				flat[i].At = int64(i)
			}
			_, flatBody, err := c.Encode(flat)
			if err != nil {
				t.Fatal(err)
			}
			var values []byte
			if start == 0 {
				values, flatPreferred = encodeGrid(t, c, w, flat, flatBody)
			} else {
				values, _ = encodeGrid(t, c, w, flat, flatBody, flatPreferred)
			}
			times := s.Times[start:min(start+240, len(s.Times))]
			clock := encodeClock(w, times)
			clockBytes += len(clock)
			key := string(binary.LittleEndian.AppendUint64(binary.LittleEndian.AppendUint64(nil, uint64(head.Start)), uint64(head.Count))) + string(clock)
			if !clockSet[key] {
				clockSet[key] = true
				uniqueClockBytes += len(clock)
			}
			candidate := binary.LittleEndian.AppendUint16([]byte{250}, uint16(len(clock)))
			candidate = append(candidate, clock...)
			candidate = append(candidate, values...)
			candidate = binary.LittleEndian.AppendUint32(candidate, modelChecksum(head, candidate))
			if len(candidate) >= len(base) {
				candidate = base
			} else {
				wins++
			}
			back := decodeClockBlock(t, c, r, head, candidate)
			old := make([]sample, len(points))
			for i, p := range points {
				if back[i].At != p.At || math.Float64bits(back[i].Value) != math.Float64bits(p.Value) {
					t.Fatal("clock round trip")
				}
				old[i] = sample{at: p.At, value: p.Value}
			}
			b := modelBlock{head: head, summary: summarise(old), body: base}
			versions[0][sid] = append(versions[0][sid], b)
			b.body = candidate
			versions[1][sid] = append(versions[1][sid], b)
			sampleCount += len(points)
			baseBytes += len(base)
			newBytes += len(candidate)
		}
	}
	type frequency struct {
		delta int64
		count int
	}
	var ranking []frequency
	var deltas int
	for d, n := range hist {
		ranking = append(ranking, frequency{d, n})
		deltas += n
	}
	sort.Slice(ranking, func(i, j int) bool { return ranking[i].count > ranking[j].count })
	t.Logf("CLOCK series=%d distinct-full-axes=%d gcd=%dms distinct-deltas=%d", len(series), len(axes), allGCD, len(hist))
	for _, v := range ranking[:min(10, len(ranking))] {
		t.Logf("DELTA %dms %.4f%%", v.delta, 100*float64(v.count)/float64(deltas))
	}
	t.Logf("PAYLOAD samples=%d baseline=%.6f candidate=%.6f wins=%d", sampleCount, float64(baseBytes)/float64(sampleCount), float64(newBytes)/float64(sampleCount), wins)
	t.Logf("CLOCK BODY sum=%d unique=%d saving-before-references=%d", clockBytes, uniqueClockBytes, clockBytes-uniqueClockBytes)
	for variant, blocks := range versions {
		for _, group := range []int{1, 8, 16} {
			clockFile(t, c, w, r, series, blocks, variant, group, sampleCount)
		}
	}
}

func clockFile(t *testing.T, c *codec.Codec, w *zstd.Encoder, r *zstd.Decoder, series []corpusSeries, all [][]modelBlock, variant, width, total int) {
	t.Helper()
	schema := squeezedSchema("clock", true, true, false)
	if width > 1 {
		schema.create[1] = strings.Replace(schema.create[1], "payload_id integer not null,", "payload_id integer not null, directory blob,", 1)
	}
	schema.create = append(schema.create, `create table registry(id integer primary key, labels text not null) strict`)
	schema.indexes = append(schema.indexes, `create unique index registry_labels on registry(labels)`)
	db := openNight(t, filepath.Join(t.TempDir(), "clock.db"), schema)
	defer db.Close()
	tx, beginErr := db.Begin()
	if beginErr != nil {
		t.Fatal(beginErr)
	}
	id := 0
	for sid, blocks := range all {
		labels, labelErr := json.Marshal(series[sid].Metric)
		if labelErr != nil {
			t.Fatal(labelErr)
		}
		if _, err := tx.Exec(`insert into registry values(?,?)`, sid+1, string(labels)); err != nil {
			t.Fatal(err)
		}
		for start := 0; start < len(blocks); start += width {
			part := blocks[start:min(start+width, len(blocks))]
			first, last := part[0], part[len(part)-1]
			body := first.body
			var dir []byte
			if width > 1 {
				dir, body = compactClockDirectory(w, part)
			}
			count, low, high, sum := 0, first.summary.min, first.summary.max, 0.0
			for _, b := range part {
				count += b.head.Count
				low = min(low, b.summary.min)
				high = max(high, b.summary.max)
				sum += b.summary.sum
			}
			id++
			if _, err := tx.Exec(`insert into payloads values(?,?)`, id, body); err != nil {
				t.Fatal(err)
			}
			args := []any{sid + 1, first.head.Start, last.head.End - first.head.Start, count, low, high, sum, first.head.First, last.summary.last, nil, nil, id}
			query := `insert into blocks values(?,?,?,?,?,?,?,?,?,?,?,?)`
			if width > 1 {
				args = append(args, dir)
				query = `insert into blocks values(?,?,?,?,?,?,?,?,?,?,?,?,?)`
			}
			if _, err := tx.Exec(query, args...); err != nil {
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
	directory := `null`
	if width > 1 {
		directory = `b.directory`
	}
	rows, err := db.Query(`select b.series_id,b.start_ts,b.span,b.count,b.first,` + directory + `,p.body from blocks b join payloads p on p.id=b.payload_id order by b.series_id,b.start_ts`)
	if err != nil {
		t.Fatal(err)
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
		if width > 1 {
			part = readCompactClockDirectory(t, r, dir, body)
		}
		for _, b := range part {
			if width > 1 {
				expected := all[sid-1][positions[sid-1]/240]
				got := []float64{b.summary.min, b.summary.max, b.summary.sum, b.summary.last}
				want := []float64{expected.summary.min, expected.summary.max, expected.summary.sum, expected.summary.last}
				for i := range got {
					if math.Float64bits(got[i]) != math.Float64bits(want[i]) {
						t.Fatal("directory summary changed")
					}
				}
			}
			for _, p := range decodeClockBlock(t, c, r, b.head, b.body) {
				i := positions[sid-1]
				expected := series[sid-1]
				if i >= len(expected.Values) || p.At != expected.Times[i] || math.Float64bits(p.Value) != math.Float64bits(expected.Values[i]) {
					t.Fatal("persisted clock changed sample")
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
			t.Fatal("missing persisted clock samples")
		}
	}
	shares, size := nightShares(t, db, "payloads")
	t.Logf("FILE variant=%d group=%d bytes=%d B/sample=%.6f", variant, width, size, float64(size)/float64(total))
	for _, s := range shares {
		if s.name == "blocks" || s.name == "payloads" {
			t.Logf("OBJECT %s %.6f unused=%.6f", s.name, float64(s.bytes)/float64(total), float64(s.unused)/float64(total))
		}
	}
}
