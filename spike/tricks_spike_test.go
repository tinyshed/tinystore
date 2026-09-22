package spike

import (
	"bytes"
	"crypto/sha256"
	"database/sql"
	"encoding/binary"
	"encoding/json"
	"hash/crc32"
	"math"
	"path/filepath"
	"strings"
	"testing"

	"github.com/klauspost/compress/zstd"

	"github.com/tinyshed/tinystore/codec"
)

func trickResources(t *testing.T) (*codec.Codec, *zstd.Encoder, *zstd.Decoder) {
	t.Helper()
	c, err := codec.New()
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { c.Close() })
	w, err := zstd.NewWriter(nil, zstd.WithEncoderConcurrency(1), zstd.WithWindowSize(8192), zstd.WithLowerEncoderMem(true))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { w.Close() })
	r, err := zstd.NewReader(nil, zstd.WithDecoderConcurrency(1), zstd.WithDecoderMaxMemory(8192))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(r.Close)
	return c, w, r
}

// the new envelope owns integrity; the nested codec envelope is reconstructed only to reuse its decoder
func packFlatTrick(t *testing.T, body []byte) []byte {
	t.Helper()
	if body[0] != 0 {
		return append([]byte{255}, body...)
	}
	b := body[1:]
	if b[0] != 1 || b[1]&3 != 0 || binary.LittleEndian.Uint16(b[2:]) != 0 {
		t.Fatal("not a flat codec body")
	}
	return append([]byte{b[1]}, b[4:len(b)-4]...)
}

func decodeFlatTrick(t *testing.T, c *codec.Codec, r *zstd.Decoder, h codec.Head, packed []byte) []codec.Sample {
	t.Helper()
	h.Start = 0
	h.End = int64(h.Count - 1)
	if packed[0] == 255 {
		return decodeGrid(t, c, r, h, packed[1:])
	}
	b := append([]byte{1, packed[0], 0, 0}, packed[1:]...)
	head := binary.LittleEndian.AppendUint64(nil, uint64(h.Start))
	head = binary.LittleEndian.AppendUint64(head, uint64(h.End))
	head = binary.LittleEndian.AppendUint16(head, uint16(h.Count))
	head = binary.LittleEndian.AppendUint64(head, math.Float64bits(h.First))
	table := crc32.MakeTable(crc32.Castagnoli)
	sum := crc32.Update(crc32.Checksum(head, table), table, b)
	b = binary.LittleEndian.AppendUint32(b, sum)
	return decodeGrid(t, c, r, h, append([]byte{0}, b...))
}

func trickTimes(t *testing.T, r *zstd.Decoder, h codec.Head, clock []byte) []int64 {
	t.Helper()
	if len(clock) > 0 {
		return decodeClock(t, r, h, clock)
	}
	out := make([]int64, h.Count)
	out[0] = h.Start
	if h.Count > 1 {
		span := uint64(h.End) - uint64(h.Start)
		if span%uint64(h.Count-1) != 0 {
			t.Fatal("nonintegral step")
		}
		step := span / uint64(h.Count-1)
		for i := 1; i < h.Count; i++ {
			out[i] = int64(uint64(out[i-1]) + step)
		}
	}
	return out
}

func decodeInlineTrick(t *testing.T, c *codec.Codec, r *zstd.Decoder, h codec.Head, body []byte) []codec.Sample {
	t.Helper()
	end := len(body) - 4
	if modelChecksum(h, body[:end]) != binary.LittleEndian.Uint32(body[end:]) {
		t.Fatal("inline checksum")
	}
	n, k := binary.Uvarint(body[1:])
	if k <= 0 || int(n) > end-1-k {
		t.Fatal("inline length")
	}
	start := 1 + k
	clock := body[start : start+int(n)]
	out := decodeFlatTrick(t, c, r, h, body[start+int(n):end])
	times := trickTimes(t, r, h, clock)
	for i := range out {
		out[i].At = times[i]
	}
	return out
}

func decodeSharedTrick(t *testing.T, c *codec.Codec, r *zstd.Decoder, h codec.Head, body, clock []byte) []codec.Sample {
	t.Helper()
	end := len(body) - 4
	bound := append(append([]byte(nil), clock...), body[:end]...)
	if modelChecksum(h, bound) != binary.LittleEndian.Uint32(body[end:]) {
		t.Fatal("shared checksum")
	}
	out := decodeFlatTrick(t, c, r, h, body[:end])
	times := trickTimes(t, r, h, clock)
	for i := range out {
		out[i].At = times[i]
	}
	return out
}

func clockBundle(clocks [][]byte) []byte {
	out := []byte{byte(len(clocks))}
	nonempty := false
	for _, c := range clocks {
		nonempty = nonempty || len(c) > 0
		out = binary.AppendUvarint(out, uint64(len(c)))
		out = append(out, c...)
	}
	if !nonempty {
		return nil
	}
	return binary.LittleEndian.AppendUint32(out, crc32.ChecksumIEEE(out))
}

func readClockBundle(t *testing.T, body []byte, n int) [][]byte {
	t.Helper()
	out := make([][]byte, n)
	if len(body) == 0 {
		return out
	}
	end := len(body) - 4
	if int(body[0]) != n || crc32.ChecksumIEEE(body[:end]) != binary.LittleEndian.Uint32(body[end:]) {
		t.Fatal("clock bundle checksum")
	}
	raw := body[1:end]
	for i := range out {
		length, k := binary.Uvarint(raw)
		if k <= 0 || length > uint64(len(raw)-k) {
			t.Fatal("clock bundle length")
		}
		out[i] = raw[k : k+int(length)]
		raw = raw[k+int(length):]
	}
	if len(raw) != 0 {
		t.Fatal("clock bundle trailing bytes")
	}
	return out
}

func TestStorageTricks(t *testing.T) {
	series := readJSONLCorpus(t)
	c, w, r := trickResources(t)
	inline, shared := make([][]modelBlock, len(series)), make([][]modelBlock, len(series))
	clocks := make([][][]byte, len(series))
	total := 0
	for sid, s := range series {
		preferred, flatPreferred := -1, -1
		for start := 0; start < len(s.Values); start += 240 {
			points := corpusSamples(s, start, min(start+240, len(s.Values)))
			h, body, err := c.Encode(points)
			if err != nil {
				t.Fatal(err)
			}
			var original []byte
			if start == 0 {
				original, preferred = encodeGrid(t, c, w, points, body)
			} else {
				original, _ = encodeGrid(t, c, w, points, body, preferred)
			}
			flat := append([]codec.Sample(nil), points...)
			regular := true
			for i := range flat {
				flat[i].At = int64(i)
				if i > 1 && points[i].At-points[i-1].At != points[1].At-points[0].At {
					regular = false
				}
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
			var clock []byte
			if !regular {
				clock = encodeClock(w, s.Times[start:min(start+240, len(s.Times))])
			}
			clocks[sid] = append(clocks[sid], clock)
			packed := packFlatTrick(t, values)
			candidate := binary.AppendUvarint([]byte{249}, uint64(len(clock)))
			candidate = append(candidate, clock...)
			candidate = append(candidate, packed...)
			candidate = binary.LittleEndian.AppendUint32(candidate, modelChecksum(h, candidate))
			if len(original) < len(candidate) {
				candidate = original
			}
			bound := append(append([]byte(nil), clock...), packed...)
			sharedBody := binary.LittleEndian.AppendUint32(append([]byte(nil), packed...), modelChecksum(h, bound))
			old := make([]sample, len(points))
			a := decodeClockBlock(t, c, r, h, candidate)
			b := decodeSharedTrick(t, c, r, h, sharedBody, clock)
			for i, p := range points {
				if a[i].At != p.At || b[i].At != p.At || math.Float64bits(a[i].Value) != math.Float64bits(p.Value) || math.Float64bits(b[i].Value) != math.Float64bits(p.Value) {
					t.Fatal("candidate changed sample")
				}
				old[i] = sample{at: p.At, value: p.Value}
			}
			block := modelBlock{head: h, summary: summarise(old), body: candidate}
			inline[sid] = append(inline[sid], block)
			block.body = sharedBody
			shared[sid] = append(shared[sid], block)
			total += len(points)
		}
	}
	for _, run := range []struct {
		name           string
		blocks         [][]modelBlock
		scatter, share bool
	}{
		{"compact-inline", inline, false, false}, {"separate-payloads", inline, true, false}, {"shared-clocks", shared, false, true}, {"shared-separated", shared, true, true},
	} {
		t.Run(run.name, func(t *testing.T) {
			t.Parallel()
			c, w, r := trickResources(t)
			for _, width := range []int{8, 16, 32, 64} {
				trickFile(t, c, w, r, series, run.blocks, clocks, width, total, run.scatter, run.share)
			}
		})
	}
}

func trickFile(t *testing.T, c *codec.Codec, w *zstd.Encoder, r *zstd.Decoder, series []corpusSeries, all [][]modelBlock, clocks [][][]byte, width, total int, scatter, share bool) {
	t.Helper()
	schema := squeezedSchema("tricks", true, true, false)
	schema.create[1] = strings.Replace(schema.create[1], "payload_id integer not null,", "payload_id integer not null, directory blob, clock_id integer not null,", 1)
	schema.create = append(schema.create, `create table registry(id integer primary key,labels text not null) strict`)
	schema.indexes = append(schema.indexes, `create unique index registry_labels on registry(labels)`)
	if share {
		schema.create = append(schema.create, `create table clocks(id integer primary key,digest blob not null unique,refs integer not null,body blob not null) strict`)
	}
	db := openNight(t, filepath.Join(t.TempDir(), "trick.db"), schema)
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
			dir, body := compactClockDirectory(w, part)
			clockID := int64(0)
			if share {
				bundle := clockBundle(clocks[sid][start : start+len(part)])
				if len(bundle) > 0 {
					digest := sha256.Sum256(bundle)
					var saved []byte
					lookupErr := tx.QueryRow(`select id,body from clocks where digest=?`, digest[:]).Scan(&clockID, &saved)
					switch lookupErr {
					case sql.ErrNoRows:
						result, err := tx.Exec(`insert into clocks(digest,refs,body) values(?,1,?)`, digest[:], bundle)
						if err != nil {
							t.Fatal(err)
						}
						clockID, err = result.LastInsertId()
						if err != nil {
							t.Fatal(err)
						}
					case nil:
						if !bytes.Equal(saved, bundle) {
							t.Fatal("clock digest collision")
						}
						if _, err := tx.Exec(`update clocks set refs=refs+1 where id=?`, clockID); err != nil {
							t.Fatal(err)
						}
					default:
						t.Fatal(lookupErr)
					}
				}
			}
			firstID := id + 1
			if scatter {
				for _, b := range part {
					id++
					if _, err := tx.Exec(`insert into payloads values(?,?)`, id, b.body); err != nil {
						t.Fatal(err)
					}
				}
			} else {
				id++
				if _, err := tx.Exec(`insert into payloads values(?,?)`, id, body); err != nil {
					t.Fatal(err)
				}
			}
			count, low, high, sum := 0, first.summary.min, first.summary.max, 0.0
			for _, b := range part {
				count += b.head.Count
				low = min(low, b.summary.min)
				high = max(high, b.summary.max)
				sum += b.summary.sum
			}
			if _, err := tx.Exec(`insert into blocks values(?,?,?,?,?,?,?,?,?,?,?,?,?,?)`, sid+1, first.head.Start, last.head.End-first.head.Start, count, low, high, sum, first.head.First, last.summary.last, nil, nil, firstID, dir, clockID); err != nil {
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
	type group struct {
		sid, id, clock int
		dir            []byte
	}
	var groups []group
	func() {
		rows, err := db.Query(`select series_id,payload_id,directory,clock_id from blocks order by series_id,start_ts`)
		if err != nil {
			t.Fatal(err)
		}
		defer rows.Close()
		for rows.Next() {
			var g group
			if err := rows.Scan(&g.sid, &g.id, &g.dir, &g.clock); err != nil {
				t.Fatal(err)
			}
			groups = append(groups, g)
		}
		if err := rows.Err(); err != nil {
			t.Fatal(err)
		}
	}()
	positions := make([]int, len(series))
	for _, g := range groups {
		n := int(g.dir[0])
		var body, clock []byte
		if scatter {
			func() {
				rows, err := db.Query(`select body from payloads where id>=? and id<? order by id`, g.id, g.id+n)
				if err != nil {
					t.Fatal(err)
				}
				defer rows.Close()
				seen := 0
				for rows.Next() {
					var b []byte
					if err := rows.Scan(&b); err != nil {
						t.Fatal(err)
					}
					body = append(body, b...)
					seen++
				}
				if err := rows.Err(); err != nil {
					t.Fatal(err)
				}
				if seen != n {
					t.Fatal("missing payload")
				}
			}()
		} else {
			if err := db.QueryRow(`select body from payloads where id=?`, g.id).Scan(&body); err != nil {
				t.Fatal(err)
			}
		}
		if share && g.clock != 0 {
			if err := db.QueryRow(`select body from clocks where id=?`, g.clock).Scan(&clock); err != nil {
				t.Fatal(err)
			}
		}
		part := readCompactClockDirectory(t, r, g.dir, body)
		clockParts := readClockBundle(t, clock, n)
		for j, b := range part {
			expectedBlock := all[g.sid-1][positions[g.sid-1]/240]
			got := []float64{b.summary.min, b.summary.max, b.summary.sum, b.summary.last}
			want := []float64{expectedBlock.summary.min, expectedBlock.summary.max, expectedBlock.summary.sum, expectedBlock.summary.last}
			for k := range got {
				if math.Float64bits(got[k]) != math.Float64bits(want[k]) {
					t.Fatal("summary changed")
				}
			}
			var points []codec.Sample
			if share {
				points = decodeSharedTrick(t, c, r, b.head, b.body, clockParts[j])
			} else {
				points = decodeClockBlock(t, c, r, b.head, b.body)
			}
			for _, p := range points {
				i := positions[g.sid-1]
				s := series[g.sid-1]
				if i >= len(s.Values) || p.At != s.Times[i] || math.Float64bits(p.Value) != math.Float64bits(s.Values[i]) {
					t.Fatal("SQLite sample changed")
				}
				positions[g.sid-1]++
			}
		}
	}
	for sid, n := range positions {
		if n != len(series[sid].Values) {
			t.Fatal("samples lost")
		}
	}
	shares, size := nightShares(t, db, "payloads")
	var physical, free int64
	if err := db.QueryRow(`select (select page_count from pragma_page_count)*(select page_size from pragma_page_size),(select freelist_count from pragma_freelist_count)`).Scan(&physical, &free); err != nil {
		t.Fatal(err)
	}
	if physical != size || free != 0 {
		t.Fatalf("dbstat excluded file bytes: file=%d used=%d freelist=%d", physical, size, free)
	}
	t.Logf("RESULT group=%d file=%d B/sample=%.6f scatter=%v shared=%v", width, size, float64(size)/float64(total), scatter, share)
	for _, s := range shares {
		t.Logf("OBJECT %s %.6f", s.name, float64(s.bytes)/float64(total))
	}
	if share {
		checkRefs := func() {
			var n int
			err := db.QueryRow(`select count(*) from clocks c left join (select clock_id,count(*) n from blocks where clock_id!=0 group by clock_id) b on c.id=b.clock_id where b.n is null or c.refs!=b.n or c.refs<=0`).Scan(&n)
			if err != nil || n != 0 {
				t.Fatal("clock ownership", n, err)
			}
			if err := db.QueryRow(`select count(*) from blocks b left join clocks c on b.clock_id=c.id where b.clock_id!=0 and c.id is null`).Scan(&n); err != nil || n != 0 {
				t.Fatal("missing shared clock", n, err)
			}
		}
		checkRefs()
		tx, err := db.Begin()
		if err != nil {
			t.Fatal(err)
		}
		for _, g := range groups {
			if g.sid != 1 {
				continue
			}
			n := 1
			if scatter {
				n = int(g.dir[0])
			}
			if _, err := tx.Exec(`delete from payloads where id>=? and id<?`, g.id, g.id+n); err != nil {
				t.Fatal(err)
			}
			if g.clock != 0 {
				if _, err := tx.Exec(`update clocks set refs=refs-1 where id=?`, g.clock); err != nil {
					t.Fatal(err)
				}
			}
		}
		if _, err := tx.Exec(`delete from blocks where series_id=1`); err != nil {
			t.Fatal(err)
		}
		if _, err := tx.Exec(`delete from clocks where refs=0`); err != nil {
			t.Fatal(err)
		}
		if err := tx.Commit(); err != nil {
			t.Fatal(err)
		}
		checkRefs()
		remaining := 0
		for _, g := range groups {
			if g.sid != 1 {
				if scatter {
					remaining += int(g.dir[0])
				} else {
					remaining++
				}
			}
		}
		var payloads int
		if err := db.QueryRow(`select count(*) from payloads`).Scan(&payloads); err != nil || payloads != remaining {
			t.Fatal("payload lifecycle", payloads, remaining, err)
		}
	}
}
