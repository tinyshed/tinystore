package spike

import (
	"bytes"
	"crypto/sha256"
	"database/sql"
	"encoding/binary"
	"encoding/json"
	"hash/crc32"
	"math"
	"math/bits"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/klauspost/compress/zstd"

	"github.com/tinyshed/tinystore/codec"
)

func combinedScalar(out []byte, v float64) []byte {
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
		return binary.AppendUvarint(out, z<<2|uint64(kind))
	}
	return binary.LittleEndian.AppendUint64(append(out, 2), math.Float64bits(v))
}

func combinedTake(t *testing.T, raw *[]byte) uint64 {
	t.Helper()
	v, n := binary.Uvarint(*raw)
	if n <= 0 {
		t.Fatal("combined varint")
	}
	*raw = (*raw)[n:]
	return v
}

func combinedFloat(t *testing.T, raw *[]byte) float64 {
	t.Helper()
	token := combinedTake(t, raw)
	if token == 2 {
		if len(*raw) < 8 {
			t.Fatal("combined float")
		}
		v := math.Float64frombits(binary.LittleEndian.Uint64(*raw))
		*raw = (*raw)[8:]
		return v
	}
	kind, z := token&3, token>>2
	if kind > 1 {
		t.Fatal("combined scalar tag")
	}
	n := int64(z>>1) ^ -int64(z&1)
	v := float64(n)
	if kind == 1 {
		v /= 100
	}
	return v
}

func combinedEvents(points []codec.Sample) []byte {
	constant := true
	first := math.Float64bits(points[0].Value)
	for _, p := range points[1:] {
		constant = constant && math.Float64bits(p.Value) == first
	}
	if constant {
		return nil
	}
	var best []byte
	for kind := range byte(3) {
		factor := 1.0
		if kind == 1 {
			factor = 100
		}
		q := make([]int64, len(points))
		valid := true
		if kind < 2 {
			for i, p := range points {
				v := math.Round(p.Value * factor)
				if math.IsNaN(v) || math.IsInf(v, 0) || math.Abs(v) >= 0x1p60 {
					valid = false
					break
				}
				q[i] = int64(v)
				if math.Float64bits(float64(q[i])/factor) != math.Float64bits(p.Value) {
					valid = false
					break
				}
			}
		}
		if !valid {
			continue
		}
		out := []byte{254, kind}
		previous := 0
		for i := 1; i < len(points); i++ {
			if math.Float64bits(points[i].Value) == math.Float64bits(points[i-1].Value) {
				continue
			}
			out = binary.AppendUvarint(out, uint64(i-previous))
			previous = i
			if kind == 2 {
				out = binary.LittleEndian.AppendUint64(out, math.Float64bits(points[i].Value))
			} else {
				out = binary.AppendVarint(out, q[i]-q[i-1])
			}
		}
		if best == nil || len(out) < len(best) {
			best = out
		}
	}
	return best
}

func combinedValues(t *testing.T, c *codec.Codec, r *zstd.Decoder, h codec.Head, body []byte) []codec.Sample {
	t.Helper()
	if len(body) > 0 && body[0] != 254 {
		return decodeFlatTrick(t, c, r, h, body)
	}
	out := make([]codec.Sample, h.Count)
	value := h.First
	next := h.Count
	var raw []byte
	kind := byte(0)
	integer := int64(0)
	factor := 1.0
	if len(body) > 0 {
		kind = body[1]
		if kind == 1 {
			factor = 100
		}
		if kind < 2 {
			integer = int64(math.Round(value * factor))
		}
		raw = body[2:]
		next = int(combinedTake(t, &raw))
		if next <= 0 || next >= h.Count {
			t.Fatal("event position")
		}
	}
	for i := range out {
		if i == next {
			if kind == 2 {
				if len(raw) < 8 {
					t.Fatal("event float")
				}
				value = math.Float64frombits(binary.LittleEndian.Uint64(raw))
				raw = raw[8:]
			} else {
				delta, n := binary.Varint(raw)
				if n <= 0 {
					t.Fatal("event delta")
				}
				raw = raw[n:]
				integer += delta
				value = float64(integer) / factor
			}
			if len(raw) > 0 {
				gap := int(combinedTake(t, &raw))
				if gap <= 0 || gap >= h.Count-i {
					t.Fatal("event gap")
				}
				next = i + gap
			} else {
				next = h.Count
			}
		}
		out[i] = codec.Sample{At: int64(i), Value: value}
	}
	if len(raw) != 0 {
		t.Fatal("event trailing bytes")
	}
	return out
}

type combinedGroup struct {
	sid, id, clock   int64
	start            int64
	allocation, live uint32
	dir              []byte
}

func combinedSeal(g combinedGroup, compressed []byte) uint32 {
	var raw []byte
	for _, v := range []uint64{uint64(g.sid), uint64(g.id), uint64(g.clock), uint64(g.start), uint64(g.allocation), uint64(g.live)} {
		raw = binary.LittleEndian.AppendUint64(raw, v)
	}
	return crc32.Update(crc32.ChecksumIEEE(raw), crc32.IEEETable, compressed)
}

func combinedDirectory(w *zstd.Encoder, g combinedGroup, blocks []modelBlock) []byte {
	raw := []byte{byte(len(blocks))}
	for i, b := range blocks {
		raw = combinedScalar(raw, b.head.First)
		flags := byte(0)
		values := []float64{b.summary.min, b.summary.max, b.summary.last, b.summary.sum}
		predicted := []float64{b.head.First, b.head.First, b.head.First, float64(b.head.Count) * b.head.First}
		for j, v := range values {
			if math.Float64bits(v) == math.Float64bits(predicted[j]) {
				flags |= 1 << j
			}
		}
		raw = append(raw, flags)
		for j, v := range values {
			if flags&(1<<j) == 0 {
				raw = combinedScalar(raw, v)
			}
		}
		raw = binary.AppendUvarint(raw, uint64(len(b.body)))
		if g.allocation&(1<<i) == 0 {
			raw = append(raw, b.body...)
		}
	}
	packed := w.EncodeAll(raw, nil)
	return append(binary.LittleEndian.AppendUint32(nil, combinedSeal(g, packed)), packed...)
}

type combinedAxis struct {
	blocks []modelBlock
	times  [][]int64
	body   []byte
}

func TestCombinedEventBits(t *testing.T) {
	c, _, r := trickResources(t)
	for _, values := range [][]float64{
		{math.Copysign(0, -1), math.Copysign(0, -1), 0, math.Float64frombits(0x7ff8000000001234), math.Inf(1), math.Inf(-1), math.SmallestNonzeroFloat64},
		{math.Float64frombits(0x7ff8000000001234), math.Float64frombits(0x7ff8000000001234)},
		{1, 1, 1, 2, 2, -3, -3, 1},
		{0.1, 0.1, 0.2, 0.2, -0.3, -0.3, 0.1},
	} {
		points := make([]codec.Sample, len(values))
		for i, v := range values {
			points[i] = codec.Sample{At: int64(i), Value: v}
		}
		body := combinedEvents(points)
		back := combinedValues(t, c, r, codec.Head{Count: len(points), First: values[0]}, body)
		for i, v := range values {
			if math.Float64bits(back[i].Value) != math.Float64bits(v) {
				t.Fatal("event edge bits")
			}
		}
	}
}

func TestCombinedStorage(t *testing.T) {
	series := readJSONLCorpus(t)
	encoder, w, r := trickResources(t)
	all := make([][]modelBlock, len(series))
	axes := make([][]modelBlock, len(series))
	total, constants, eventWins, oldBytes, newBytes := 0, 0, 0, 0, 0
	begin := time.Now()
	for sid, s := range series {
		preferred := -1
		for start := 0; start < len(s.Values); start += 240 {
			points := corpusSamples(s, start, min(start+240, len(s.Values)))
			flat := append([]codec.Sample(nil), points...)
			for i := range flat {
				flat[i].At = int64(i)
			}
			head, body, err := encoder.Encode(flat)
			if err != nil {
				t.Fatal(err)
			}
			var grid []byte
			if start == 0 {
				grid, preferred = encodeGrid(t, encoder, w, flat, body)
			} else {
				grid, _ = encodeGrid(t, encoder, w, flat, body, preferred)
			}
			packed := packFlatTrick(t, grid)
			oldBytes += len(packed)
			events := combinedEvents(points)
			if len(events) < len(packed) {
				packed = events
				if len(events) == 0 {
					constants++
				} else {
					eventWins++
				}
			}
			head.Start = points[0].At
			head.End = points[len(points)-1].At
			newBytes += len(packed)
			old := make([]sample, len(points))
			back := combinedValues(t, encoder, r, head, packed)
			for i, p := range points {
				if math.Float64bits(back[i].Value) != math.Float64bits(p.Value) {
					t.Fatal("combined value bits")
				}
				old[i] = sample{at: p.At, value: p.Value}
			}
			all[sid] = append(all[sid], modelBlock{head: head, summary: summarise(old), body: packed})
			regular := true
			for i := 2; i < len(points); i++ {
				regular = regular && points[i].At-points[i-1].At == points[1].At-points[0].At
			}
			var clock []byte
			if !regular {
				clock = encodeClock(w, s.Times[start:min(start+240, len(s.Times))])
			}
			head.First = 0
			axes[sid] = append(axes[sid], modelBlock{head: head, body: clock})
			total += len(points)
		}
	}
	t.Logf("ENCODE samples=%d constants=%d events=%d bare-values=%d->%d elapsed=%s", total, constants, eventWins, oldBytes, newBytes, time.Since(begin))
	schema := squeezedSchema("combined", true, true, false)
	schema.create[1] = strings.Replace(schema.create[1], "payload_id integer not null,", "payload_id integer not null,directory blob not null,clock_id integer not null,allocation integer not null,live integer not null,", 1)
	schema.create = append(schema.create, `create table registry(id integer primary key,labels text not null) strict`, `create table clocks(id integer primary key,digest blob not null unique,refs integer not null,body blob not null) strict`)
	schema.indexes = append(schema.indexes, `create unique index registry_labels on registry(labels)`)
	db := openNight(t, filepath.Join(t.TempDir(), "combined.db"), schema)
	defer db.Close()
	tx, beginErr := db.Begin()
	if beginErr != nil {
		t.Fatal(beginErr)
	}
	const width = 32
	const inlineLimit = 16
	id := int64(0)
	inlineCount, externalCount := 0, 0
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
			clockPart := axes[sid][start : start+len(part)]
			clockDir, clockBody := compactClockDirectory(w, clockPart)
			clockRecord := binary.AppendUvarint(nil, uint64(len(clockDir)))
			clockRecord = append(clockRecord, clockDir...)
			clockRecord = append(clockRecord, clockBody...)
			clockRecord = binary.LittleEndian.AppendUint32(clockRecord, crc32.ChecksumIEEE(clockRecord))
			digest := sha256.Sum256(clockRecord)
			var clockID int64
			var saved []byte
			lookupErr := tx.QueryRow(`select id,body from clocks where digest=?`, digest[:]).Scan(&clockID, &saved)
			switch lookupErr {
			case sql.ErrNoRows:
				result, err := tx.Exec(`insert into clocks(digest,refs,body) values(?,1,?)`, digest[:], clockRecord)
				if err != nil {
					t.Fatal(err)
				}
				clockID, err = result.LastInsertId()
				if err != nil {
					t.Fatal(err)
				}
			case nil:
				if !bytes.Equal(saved, clockRecord) {
					t.Fatal("clock hash collision")
				}
				if _, err := tx.Exec(`update clocks set refs=refs+1 where id=?`, clockID); err != nil {
					t.Fatal(err)
				}
			default:
				t.Fatal(lookupErr)
			}
			g := combinedGroup{sid: int64(sid + 1), id: id + 1, clock: clockID, start: part[0].head.Start, live: uint32((uint64(1) << len(part)) - 1)}
			count, low, high, sum := 0, part[0].summary.min, part[0].summary.max, 0.0
			for j, b := range part {
				count += b.head.Count
				low = min(low, b.summary.min)
				high = max(high, b.summary.max)
				sum += b.summary.sum
				if len(b.body) > inlineLimit {
					g.allocation |= 1 << j
					id++
					externalCount++
					bound := append(append([]byte(nil), clockPart[j].body...), b.body...)
					payload := binary.LittleEndian.AppendUint32(append([]byte(nil), b.body...), modelChecksum(b.head, bound))
					if _, err := tx.Exec(`insert into payloads values(?,?)`, id, payload); err != nil {
						t.Fatal(err)
					}
				} else if len(b.body) > 0 {
					inlineCount++
				}
			}
			g.dir = combinedDirectory(w, g, part)
			last := part[len(part)-1]
			if _, err := tx.Exec(`insert into blocks values(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)`, g.sid, g.start, last.head.End-g.start, count, low, high, sum, part[0].head.First, last.summary.last, nil, nil, g.id, g.dir, g.clock, int64(g.allocation), int64(g.live)); err != nil {
				t.Fatal(err)
			}
			if err := upsertState(tx, g.sid, summary{startTS: g.start, endTS: last.head.End}); err != nil {
				t.Fatal(err)
			}
		}
	}
	if err := tx.Commit(); err != nil {
		t.Fatal(err)
	}
	checkpoint(t, db)
	decoder, _, reader := trickResources(t)
	var groups []combinedGroup
	func() {
		rows, err := db.Query(`select series_id,start_ts,payload_id,clock_id,allocation,live,directory from blocks order by series_id,start_ts`)
		if err != nil {
			t.Fatal(err)
		}
		defer rows.Close()
		for rows.Next() {
			var g combinedGroup
			if err := rows.Scan(&g.sid, &g.start, &g.id, &g.clock, &g.allocation, &g.live, &g.dir); err != nil {
				t.Fatal(err)
			}
			groups = append(groups, g)
		}
		if err := rows.Err(); err != nil {
			t.Fatal(err)
		}
	}()
	cache := map[int64]combinedAxis{}
	var fifo []int64
	cacheHits := 0
	loadAxis := func(id int64) combinedAxis {
		if a, ok := cache[id]; ok {
			cacheHits++
			return a
		}
		var body []byte
		if err := db.QueryRow(`select body from clocks where id=?`, id).Scan(&body); err != nil {
			t.Fatal(err)
		}
		end := len(body) - 4
		if crc32.ChecksumIEEE(body[:end]) != binary.LittleEndian.Uint32(body[end:]) {
			t.Fatal("axis checksum")
		}
		raw := body[:end]
		n := int(combinedTake(t, &raw))
		if n > len(raw) {
			t.Fatal("axis directory")
		}
		a := combinedAxis{blocks: readCompactClockDirectory(t, reader, raw[:n], raw[n:]), body: body}
		a.times = make([][]int64, len(a.blocks))
		for i, b := range a.blocks {
			a.times[i] = trickTimes(t, reader, b.head, b.body)
		}
		if len(fifo) == 8 {
			delete(cache, fifo[0])
			fifo = fifo[1:]
		}
		fifo = append(fifo, id)
		cache[id] = a
		return a
	}
	readGroup := func(g combinedGroup) []modelBlock {
		if len(g.dir) < 4 || binary.LittleEndian.Uint32(g.dir) != combinedSeal(g, g.dir[4:]) {
			t.Fatal("group integrity")
		}
		raw, err := reader.DecodeAll(g.dir[4:], nil)
		if err != nil {
			t.Fatal(err)
		}
		n := int(raw[0])
		raw = raw[1:]
		axis := loadAxis(g.clock)
		if len(axis.blocks) != n {
			t.Fatal("axis group mismatch")
		}
		out := make([]modelBlock, n)
		for j := range out {
			h := axis.blocks[j].head
			h.First = combinedFloat(t, &raw)
			flags := raw[0]
			raw = raw[1:]
			values := []float64{h.First, h.First, h.First, float64(h.Count) * h.First}
			for k := range values {
				if flags&(1<<k) == 0 {
					values[k] = combinedFloat(t, &raw)
				}
			}
			length := int(combinedTake(t, &raw))
			var packed []byte
			if g.allocation&(1<<j) == 0 {
				if length > len(raw) || length > inlineLimit {
					t.Fatal("inline bounds")
				}
				packed = raw[:length]
				raw = raw[length:]
			} else if g.live&(1<<j) != 0 {
				payloadID := g.id + int64(bits.OnesCount32(g.allocation&uint32((uint64(1)<<j)-1)))
				var body []byte
				if err := db.QueryRow(`select body from payloads where id=?`, payloadID).Scan(&body); err != nil {
					t.Fatal(err)
				}
				if len(body) != length+4 {
					t.Fatal("external length")
				}
				packed = body[:length]
				bound := append(append([]byte(nil), axis.blocks[j].body...), packed...)
				if modelChecksum(h, bound) != binary.LittleEndian.Uint32(body[length:]) {
					t.Fatal("external checksum")
				}
			}
			out[j] = modelBlock{head: h, summary: summary{min: values[0], max: values[1], last: values[2], sum: values[3]}, body: packed}
		}
		if len(raw) != 0 {
			t.Fatal("directory trailing bytes")
		}
		return out
	}
	verify := func(g combinedGroup, ordinal int) {
		part := readGroup(g)
		axis := loadAxis(g.clock)
		for j, b := range part {
			if g.live&(1<<j) == 0 {
				continue
			}
			expected := all[g.sid-1][ordinal+j]
			got := []float64{b.summary.min, b.summary.max, b.summary.sum, b.summary.last}
			want := []float64{expected.summary.min, expected.summary.max, expected.summary.sum, expected.summary.last}
			for k := range got {
				if math.Float64bits(got[k]) != math.Float64bits(want[k]) {
					t.Fatal("summary changed")
				}
			}
			values := combinedValues(t, decoder, reader, b.head, b.body)
			for i, p := range values {
				at := (ordinal+j)*240 + i
				s := series[g.sid-1]
				if axis.times[j][i] != s.Times[at] || math.Float64bits(p.Value) != math.Float64bits(s.Values[at]) {
					t.Fatal("persisted combined bits changed")
				}
			}
		}
	}
	begin = time.Now()
	ordinals := make([]int, len(series))
	for _, g := range groups {
		verify(g, ordinals[g.sid-1])
		ordinals[g.sid-1] += len(loadAxis(g.clock).blocks)
	}
	for sid, n := range ordinals {
		if n != len(all[sid]) {
			t.Fatal("missing blocks")
		}
	}
	t.Logf("READBACK samples=%d elapsed=%s clock-cache-hits=%d cache-limit=8", total, time.Since(begin), cacheHits)
	shares, size := nightShares(t, db, "payloads")
	var physical, free int64
	if err := db.QueryRow(`select (select page_count from pragma_page_count)*(select page_size from pragma_page_size),(select freelist_count from pragma_freelist_count)`).Scan(&physical, &free); err != nil {
		t.Fatal(err)
	}
	if physical != size || free != 0 {
		t.Fatal("file accounting")
	}
	t.Logf("COMBINED file=%d B/sample=%.6f constants=%d inline=%d external=%d", size, float64(size)/float64(total), constants, inlineCount, externalCount)
	for _, s := range shares {
		t.Logf("OBJECT %s %.6f", s.name, float64(s.bytes)/float64(total))
	}
	checkClocks := func() {
		var n int
		if err := db.QueryRow(`select count(*) from clocks c left join (select clock_id,count(*) n from blocks group by clock_id) b on c.id=b.clock_id where b.n is null or c.refs!=b.n`).Scan(&n); err != nil || n != 0 {
			t.Fatal("clock refs")
		}
		if err := db.QueryRow(`select count(*) from blocks b left join clocks c on b.clock_id=c.id where c.id is null`).Scan(&n); err != nil || n != 0 {
			t.Fatal("clock missing")
		}
	}
	checkClocks()
	g := groups[0]
	originalAllocation := g.allocation
	deletedSlot := 0
	if g.allocation != 0 {
		deletedSlot = bits.TrailingZeros32(g.allocation)
	}
	g.live &^= 1 << deletedSlot
	g.dir = append(binary.LittleEndian.AppendUint32(nil, combinedSeal(g, g.dir[4:])), g.dir[4:]...)
	tx, beginErr = db.Begin()
	if beginErr != nil {
		t.Fatal(beginErr)
	}
	if originalAllocation != 0 {
		if _, err := tx.Exec(`delete from payloads where id=?`, g.id); err != nil {
			t.Fatal(err)
		}
	}
	aliveCount, aliveLow, aliveHigh, aliveSum := 0, math.Inf(1), math.Inf(-1), 0.0
	var aliveFirst, aliveLast float64
	for j, b := range all[g.sid-1][:min(width, len(all[g.sid-1]))] {
		if g.live&(1<<j) == 0 {
			continue
		}
		if aliveCount == 0 {
			aliveFirst = b.head.First
		}
		aliveCount += b.head.Count
		aliveLow = min(aliveLow, b.summary.min)
		aliveHigh = max(aliveHigh, b.summary.max)
		aliveSum += b.summary.sum
		aliveLast = b.summary.last
	}
	if _, err := tx.Exec(`update blocks set live=?,directory=?,count=?,min=?,max=?,sum=?,first=?,last=? where series_id=? and start_ts=?`, int64(g.live), g.dir, aliveCount, aliveLow, aliveHigh, aliveSum, aliveFirst, aliveLast, g.sid, g.start); err != nil {
		t.Fatal(err)
	}
	if err := tx.Commit(); err != nil {
		t.Fatal(err)
	}
	if err := db.QueryRow(`select allocation,live,directory from blocks where series_id=? and start_ts=?`, g.sid, g.start).Scan(&g.allocation, &g.live, &g.dir); err != nil {
		t.Fatal(err)
	}
	if g.allocation != originalAllocation {
		t.Fatal("allocation moved")
	}
	verify(g, 0)
	var parentCount int
	var parentLow, parentHigh, parentSum, parentFirst, parentLast float64
	if err := db.QueryRow(`select count,min,max,sum,first,last from blocks where series_id=? and start_ts=?`, g.sid, g.start).Scan(&parentCount, &parentLow, &parentHigh, &parentSum, &parentFirst, &parentLast); err != nil {
		t.Fatal(err)
	}
	if parentCount != aliveCount {
		t.Fatal("parent count after expiry")
	}
	for i, v := range []float64{parentLow, parentHigh, parentSum, parentFirst, parentLast} {
		if math.Float64bits(v) != math.Float64bits([]float64{aliveLow, aliveHigh, aliveSum, aliveFirst, aliveLast}[i]) {
			t.Fatal("parent summary after expiry")
		}
	}
	checkClocks()
	tx, beginErr = db.Begin()
	if beginErr != nil {
		t.Fatal(beginErr)
	}
	removed := 0
	for _, v := range groups {
		if v.sid != g.sid {
			continue
		}
		n := bits.OnesCount32(v.allocation)
		removed += n
		if _, err := tx.Exec(`delete from payloads where id>=? and id<?`, v.id, v.id+int64(n)); err != nil {
			t.Fatal(err)
		}
		if _, err := tx.Exec(`update clocks set refs=refs-1 where id=?`, v.clock); err != nil {
			t.Fatal(err)
		}
	}
	if _, err := tx.Exec(`delete from blocks where series_id=?`, g.sid); err != nil {
		t.Fatal(err)
	}
	if _, err := tx.Exec(`delete from clocks where refs=0`); err != nil {
		t.Fatal(err)
	}
	if err := tx.Commit(); err != nil {
		t.Fatal(err)
	}
	checkClocks()
	var remaining int
	if err := db.QueryRow(`select count(*) from payloads`).Scan(&remaining); err != nil || remaining != externalCount-removed {
		t.Fatal("payload lifecycle")
	}
	t.Log("LIFECYCLE partial expiry kept payload addresses, whole-series deletion kept shared clocks")
}
