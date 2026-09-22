package spike

import (
	"database/sql"
	"encoding/binary"
	"math"
	"math/bits"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/klauspost/compress/zstd"

	"github.com/tinyshed/tinystore/codec"
)

// these candidates are independent blocks, with the same external head
func reviewTransform(samples []codec.Sample, order int, shuffled bool) []byte {
	n := len(samples) - 1
	out := make([]byte, 8*n)
	previous := math.Float64bits(samples[0].Value)
	var lastDelta uint64
	for i, s := range samples[1:] {
		current := math.Float64bits(s.Value)
		v := current
		if order > 0 {
			v = current - previous
			if order == 2 {
				v -= lastDelta
			}
			lastDelta = current - previous
			v = v<<1 ^ uint64(int64(v)>>63)
		}
		previous = current
		if shuffled {
			for lane := range 8 {
				out[lane*n+i] = byte(v >> (lane * 8))
			}
		} else {
			binary.LittleEndian.PutUint64(out[i*8:], v)
		}
	}
	return out
}

func reviewRestore(t *testing.T, raw []byte, samples []codec.Sample, order int, shuffled bool) {
	t.Helper()
	n := len(samples) - 1
	if len(raw) != 8*n {
		t.Fatal("wrong transformed length")
	}
	previous := math.Float64bits(samples[0].Value)
	var lastDelta uint64
	for i, s := range samples[1:] {
		var v uint64
		if shuffled {
			for lane := range 8 {
				v |= uint64(raw[lane*n+i]) << (lane * 8)
			}
		} else {
			v = binary.LittleEndian.Uint64(raw[i*8:])
		}
		if order > 0 {
			v = v>>1 ^ -(v & 1)
			if order == 2 {
				v += lastDelta
			}
			lastDelta = v
			v += previous
		}
		if v != math.Float64bits(s.Value) {
			t.Fatalf("bits changed at %d", i+1)
		}
		previous = v
	}
}

func TestReviewFloatTransforms(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1 to measure")
	}
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
	for _, kind := range []string{"noisy", "sine", "random bits", "temperature", "integers"} {
		var baseline int
		var totals [6]int
		for seed := range 256 {
			samples := adaptiveSamples(kind, 240, seed)
			_, body, err := c.Encode(samples)
			if err != nil {
				t.Fatal(err)
			}
			baseline += len(body)
			for order := range 3 {
				for shuffle := range 2 {
					raw := reviewTransform(samples, order, shuffle == 1)
					body := w.EncodeAll(raw, nil)
					back, err := r.DecodeAll(body, nil)
					if err != nil {
						t.Fatal(err)
					}
					reviewRestore(t, back, samples, order, shuffle == 1)
					totals[order*2+shuffle] += 8 + min(len(body), len(raw))
				}
			}
		}
		const count = 256 * 240.0
		t.Logf("%s baseline=%.4f raw=%.4f shuffle=%.4f delta=%.4f delta-shuffle=%.4f delta2=%.4f delta2-shuffle=%.4f B/sample (8-byte envelope charged)", kind, float64(baseline)/count,
			float64(totals[0])/count, float64(totals[1])/count, float64(totals[2])/count, float64(totals[3])/count, float64(totals[4])/count, float64(totals[5])/count)
	}
}

func TestReviewSQLiteFirstBits(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1 to measure")
	}
	db := openNight(t, filepath.Join(t.TempDir(), "bits.db"), denseSchema{create: []string{`create table heads (first real, exact integer not null) strict`}})
	defer db.Close()
	c, err := codec.New()
	if err != nil {
		t.Fatal(err)
	}
	defer c.Close()
	for _, bits := range []uint64{0x8000000000000000, 0x7ff8000000001234, 0x3ff0000000000000} {
		value := math.Float64frombits(bits)
		head, body, err := c.Encode([]codec.Sample{{At: 1, Value: value}, {At: 2, Value: value}})
		if err != nil {
			t.Fatal(err)
		}
		result, err := db.Exec(`insert into heads values (?, ?)`, value, int64(bits))
		if err != nil {
			t.Fatal(err)
		}
		id, err := result.LastInsertId()
		if err != nil {
			t.Fatal(err)
		}
		var number sql.NullFloat64
		var exact int64
		if err := db.QueryRow(`select first, exact from heads where rowid=?`, id).Scan(&number, &exact); err != nil {
			t.Fatal(err)
		}
		head.First = number.Float64
		_, decodeErr := c.Decode(head, body)
		t.Logf("in=%016x REAL valid=%v out=%016x decode=%v", bits, number.Valid, math.Float64bits(number.Float64), decodeErr)
		head.First = math.Float64frombits(uint64(exact))
		verifyAdaptivePayload(t, c, head, body, []codec.Sample{{At: 1, Value: value}, {At: 2, Value: value}})
	}
}

func TestReviewActualMixedFile(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1 to measure")
	}
	c, err := codec.New()
	if err != nil {
		t.Fatal(err)
	}
	defer c.Close()
	const perClass = 600000
	var corpus [4][]codec.Sample
	for k, kind := range denseClasses {
		corpus[k] = adaptiveSamples(kind, perClass, k)
	}
	for _, adaptive := range []bool{false, true} {
		schema := squeezedSchema("mixed", true, true, false)
		counter := squeezedSchema("counter", true, true, true)
		db := openNight(t, filepath.Join(t.TempDir(), "mixed.db"), schema)
		var remaining [4]int
		for i := range remaining {
			remaining[i] = perClass
		}
		tx, err := db.Begin()
		if err != nil {
			t.Fatal(err)
		}
		var payload int64
		id := 0
		for round := 0; ; round++ {
			active := false
			for k, kind := range denseClasses {
				if remaining[k] == 0 {
					continue
				}
				active = true
				n := 240
				if adaptive && kind == "noisy" {
					n = 200
				}
				n = min(n, remaining[k])
				offset := (round%250)*(perClass/250) + (round/250)*n
				samples := corpus[k][offset : offset+n]
				_, body, err := c.Encode(samples)
				if err != nil {
					t.Fatal(err)
				}
				old := make([]sample, n)
				for i, s := range samples {
					old[i] = sample{at: s.At, value: s.Value}
				}
				insert := schema.insert
				if kind == "counter" {
					insert = counter.insert
				}
				if err := insert(tx, id, int64(k*250+round%250+1), summarise(old), body); err != nil {
					t.Fatal(err)
				}
				id++
				remaining[k] -= n
				payload += int64(len(body))
			}
			if !active {
				break
			}
		}
		if err := tx.Commit(); err != nil {
			t.Fatal(err)
		}
		checkpoint(t, db)
		shares, allocated := nightShares(t, db, "payloads")
		var pages, free, size int64
		if err := db.QueryRow(`select (select page_count from pragma_page_count), (select freelist_count from pragma_freelist_count), (select page_size from pragma_page_size)`).Scan(&pages, &free, &size); err != nil {
			t.Fatal(err)
		}
		t.Logf("adaptive=%v actual mixed file=%.4f B/sample payload=%.4f dbstat=%d file=%d freelist=%d", adaptive, float64(pages*size)/(4*perClass), float64(payload)/(4*perClass), allocated, pages*size, free)
		for _, s := range shares {
			t.Logf("%s %.4f B/sample unused=%.1f%%", s.name, float64(s.bytes)/(4*perClass), 100*float64(s.unused)/float64(s.bytes))
		}
		if err := db.Close(); err != nil {
			t.Fatal(err)
		}
	}
}

func reviewPack(raw []byte, group int) []byte {
	var out []byte
	for len(raw) > 0 {
		n := min(group, len(raw)/8)
		var union uint64
		for i := range n {
			union |= binary.LittleEndian.Uint64(raw[i*8:])
		}
		trailing := bits.TrailingZeros64(union)
		width := bits.Len64(union) - trailing
		if union == 0 {
			width, trailing = 0, 0
		}
		out = append(out, byte(width), byte(trailing))
		start := len(out)
		out = append(out, make([]byte, (width*n+7)/8)...)
		for i := range n {
			v := binary.LittleEndian.Uint64(raw[i*8:]) >> trailing
			for bit := range width {
				pos := i*width + bit
				out[start+pos/8] |= byte((v>>bit)&1) << (pos % 8)
			}
		}
		raw = raw[n*8:]
	}
	return out
}

func reviewUnpack(t *testing.T, packed []byte, count, group int) []byte {
	t.Helper()
	out := make([]byte, 0, count*8)
	for count > 0 {
		n := min(group, count)
		width, trailing := int(packed[0]), int(packed[1])
		packed = packed[2:]
		for i := range n {
			var v uint64
			for bit := range width {
				pos := i*width + bit
				v |= uint64((packed[pos/8]>>(pos%8))&1) << bit
			}
			out = binary.LittleEndian.AppendUint64(out, v<<trailing)
		}
		packed = packed[(width*n+7)/8:]
		count -= n
	}
	if len(packed) != 0 {
		t.Fatal("trailing packed bytes")
	}
	return out
}

func TestReviewFloatBitPacking(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1 to measure")
	}
	c, err := codec.New()
	if err != nil {
		t.Fatal(err)
	}
	defer c.Close()
	for _, kind := range []string{"noisy", "sine", "random bits"} {
		for order := 1; order <= 2; order++ {
			for _, group := range []int{32, 239} {
				var baseline, candidate int
				for seed := range 256 {
					samples := adaptiveSamples(kind, 240, seed)
					_, body, err := c.Encode(samples)
					if err != nil {
						t.Fatal(err)
					}
					baseline += len(body)
					raw := reviewTransform(samples, order, false)
					packed := reviewPack(raw, group)
					reviewRestore(t, reviewUnpack(t, packed, 239, group), samples, order, false)
					candidate += 9 + len(packed)
				}
				t.Logf("%s order=%d group=%d baseline=%.4f candidate=%.4f B/sample (9-byte framing charged)", kind, order, group, float64(baseline)/(256*240), float64(candidate)/(256*240))
			}
		}
	}
}

func TestReviewRetentionLookup(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1 to measure")
	}
	for _, count := range []int{10, 1000, 10000} {
		db := openNight(t, filepath.Join(t.TempDir(), "gc.db"), plainSchema())
		tx, err := db.Begin()
		if err != nil {
			t.Fatal(err)
		}
		stmt, err := tx.Prepare(`insert into blocks values(1,?,?,240,1,1,240,1,1,0,0,?)`)
		if err != nil {
			t.Fatal(err)
		}
		defer stmt.Close()
		for i := range count {
			if _, err = stmt.Exec(i*240, i*240+239, i+1); err != nil {
				t.Fatal(err)
			}
		}
		if err = tx.Commit(); err != nil {
			t.Fatal(err)
		}
		for _, query := range []string{
			`select min(end_ts) from blocks where series_id=1`,
			`select end_ts from blocks where series_id=1 order by start_ts limit 1`,
		} {
			const runs = 100
			start := time.Now()
			for range runs {
				var end int64
				if err = db.QueryRow(query).Scan(&end); err != nil {
					t.Fatal(err)
				}
				if end != 239 {
					t.Fatal("wrong retention boundary")
				}
			}
			t.Logf("blocks=%d query=%s elapsed=%s", count, query, time.Since(start)/runs)
		}
		if _, err = db.Exec(`insert into series_state values(1,100,50,null)`); err != nil {
			t.Fatal(err)
		}
		tx, err = db.Begin()
		if err != nil {
			t.Fatal(err)
		}
		if err = upsertState(tx, 1, summary{startTS: 200, endTS: 439, count: 240}); err != nil {
			t.Fatal(err)
		}
		if err = tx.Commit(); err != nil {
			t.Fatal(err)
		}
		var due sql.NullInt64
		if err = db.QueryRow(`select next_gc_ts from series_state where series_id=1`).Scan(&due); err != nil {
			t.Fatal(err)
		}
		t.Logf("previously empty series, new block: next_gc_ts=%v", due)
		if err = db.Close(); err != nil {
			t.Fatal(err)
		}
	}
}

func TestReviewOneDecimalException(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1 to measure")
	}
	c, err := codec.New()
	if err != nil {
		t.Fatal(err)
	}
	defer c.Close()
	for _, exception := range []float64{math.Float64frombits(0x7ff8000000001234), math.Pi} {
		samples := adaptiveSamples("temperature", 240, 0)
		samples[120].Value = exception
		_, body, err := c.Encode(samples)
		if err != nil {
			t.Fatal(err)
		}
		patched := append([]codec.Sample(nil), samples...)
		patched[120].Value = patched[119].Value
		head, base, err := c.Encode(patched)
		if err != nil {
			t.Fatal(err)
		}
		it, err := c.Decode(head, base)
		if err != nil {
			t.Fatal(err)
		}
		i := 0
		for it.Next() {
			s := it.Sample()
			if i == 120 {
				s.Value = exception
			}
			if s.At != samples[i].At || math.Float64bits(s.Value) != math.Float64bits(samples[i].Value) {
				t.Fatal("exception changed sample")
			}
			i++
		}
		if err := it.Err(); err != nil {
			t.Fatal(err)
		}
		if i != len(samples) {
			t.Fatal("missing samples")
		}
		t.Logf("exception=%016x current=%d B, patched=%d B including 1-byte mode, 1-byte count, 1-byte position, 8-byte exact value", math.Float64bits(exception), len(body), len(base)+11)
	}
}
