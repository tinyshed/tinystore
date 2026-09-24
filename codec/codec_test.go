package codec

import (
	"bytes"
	"encoding/binary"
	"errors"
	"hash/crc32"
	"math"
	"math/rand/v2"
	"slices"
	"testing"
)

func testCodec(t testing.TB) *Codec {
	t.Helper()
	c, err := New()
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := c.Close(); err != nil {
			t.Error(err)
		}
	})
	return c
}

func assertRoundTrip(t testing.TB, c *Codec, samples []Sample) (Head, []byte) {
	t.Helper()
	head, encoded, err := c.Encode(samples)
	if err != nil {
		t.Fatal(err)
	}
	it, err := c.Decode(head, encoded)
	if err != nil {
		t.Fatal(err)
	}
	i := 0
	for it.Next() {
		got := it.Sample()
		if i >= len(samples) || got.At != samples[i].At || math.Float64bits(got.Value) != math.Float64bits(samples[i].Value) {
			t.Fatalf("sample %d: got %+v", i, got)
		}
		i++
	}
	if it.Err() != nil || i != len(samples) {
		t.Fatalf("read %d/%d: %v", i, len(samples), it.Err())
	}
	return head, encoded
}

func TestExactBitsAndTimestampExtremes(t *testing.T) {
	c := testCodec(t)
	values := []uint64{
		0, 1 << 63, 0x7ff0000000000000, 0xfff0000000000000,
		0x7ff8000000000001, 0x7ff8000000000002, 1, 0x8000000000000001,
		math.Float64bits(-0x1p63), math.Float64bits(0x1p63), math.Float64bits(0x1p53 + 2),
	}
	for _, value := range values {
		assertRoundTrip(t, c, []Sample{{At: math.MinInt64, Value: math.Float64frombits(value)}})
		assertRoundTrip(t, c, []Sample{
			{At: math.MinInt64, Value: math.Float64frombits(value)},
			{At: math.MaxInt64, Value: math.Float64frombits(value)},
		})
	}
	for _, times := range [][]int64{
		{math.MinInt64, -1, math.MaxInt64},
		{math.MinInt64, 0, 1, math.MaxInt64},
		{-3, -2, -1, 0, 1},
		{0, 15, 30, 46, 61},
		{math.MaxInt64 - 1, math.MaxInt64},
	} {
		samples := make([]Sample, len(times))
		for i, at := range times {
			samples[i] = Sample{At: at, Value: math.Float64frombits(values[i%len(values)])}
		}
		assertRoundTrip(t, c, samples)
	}
	for offset := range values {
		samples := make([]Sample, MaxSamples)
		for i := range samples {
			samples[i] = Sample{At: int64(i), Value: math.Float64frombits(values[(i+offset)%len(values)])}
		}
		assertRoundTrip(t, c, samples)
	}
}

func TestWorkloadRoundTrips(t *testing.T) {
	c := testCodec(t)
	r := rand.New(rand.NewPCG(42, 17))
	for run := range 800 {
		n := 1 + r.IntN(MaxSamples)
		samples := make([]Sample, n)
		at := int64(-1000000)
		integer := int64(1000)
		for i := range samples {
			at += 1000 + int64(r.IntN(15000))
			integer += int64(r.IntN(17) - 8)
			var value float64
			switch run % 5 {
			case 0:
				value = float64(integer)
			case 1:
				value = math.Float64frombits(r.Uint64())
			case 2:
				value = 17
			case 3:
				value = math.Sin(float64(i) / 10)
			case 4:
				value = float64(i)
			}
			samples[i] = Sample{At: at, Value: value}
		}
		assertRoundTrip(t, c, samples)
	}
}

func TestRejectsUnorderedAndOversizedInput(t *testing.T) {
	c := testCodec(t)
	for _, samples := range [][]Sample{nil, make([]Sample, MaxSamples+1), {{At: 1}, {At: 1}}, {{At: 2}, {At: 1}}} {
		if _, _, err := c.Encode(samples); err == nil {
			t.Fatal("accepted invalid samples")
		}
	}
}

func TestPayloadCorruptionIsRefused(t *testing.T) {
	c := testCodec(t)
	samples := make([]Sample, 240)
	for i := range samples {
		samples[i] = Sample{At: int64(i) * 15000, Value: float64(i % 9)}
	}
	head, payload := assertRoundTrip(t, c, samples)
	for i := range payload {
		if refused := readsBack(c, head, payload[:i]); refused == nil {
			t.Fatalf("truncation %d was read back", i)
		}
		broken := slices.Clone(payload)
		broken[i] ^= 0x80
		if refused := readsBack(c, head, broken); refused == nil {
			t.Fatalf("bit flip %d was read back", i)
		}
	}
	for _, change := range []func([]byte){
		func(p []byte) { p[0] = 99 }, func(p []byte) { p[1] ^= 1 },
		func(p []byte) { p[1] ^= 4 }, func(p []byte) { p[1] ^= 32 },
		func(p []byte) { binary.LittleEndian.PutUint16(p[2:], 65535) },
	} {
		broken := slices.Clone(payload)
		change(broken)
		checksum(head, broken)
		if refused := readsBack(c, head, broken); refused == nil {
			t.Fatal("a corrupted header was read back")
		}
	}
	// the head is no longer in the body, so the checksum has to cover it too
	for _, moved := range []Head{
		{Start: head.Start + 1, End: head.End, Count: head.Count, First: head.First},
		{Start: head.Start, End: head.End + 1, Count: head.Count, First: head.First},
		{Start: head.Start, End: head.End, Count: head.Count - 1, First: head.First},
		{Start: head.Start, End: head.End, Count: head.Count, First: head.First + 1},
	} {
		if refused := readsBack(c, moved, payload); refused == nil {
			t.Fatal("a head that does not belong to this body was accepted")
		}
	}
}

// readsBack returns the error a damaged block produces, from opening it or
// from walking it, and nil when it came back whole
func readsBack(c *Codec, head Head, payload []byte) error {
	it, err := c.Decode(head, payload)
	if err != nil {
		return err
	}
	read := 0
	for it.Next() {
		read++
	}
	if it.Err() != nil {
		return it.Err()
	}
	if read != head.Count {
		return ErrInvalid
	}
	return nil
}

func TestIteratorOwnsItsBytesAndOutlivesTheCodec(t *testing.T) {
	c := testCodec(t)
	samples := []Sample{{At: 1, Value: -1}, {At: 3, Value: 7}}
	head, payload := assertRoundTrip(t, c, samples)
	it, err := c.Decode(head, payload)
	if err != nil {
		t.Fatal(err)
	}
	clear(payload)
	if err = c.Close(); err != nil {
		t.Fatal(err)
	}
	for _, want := range samples {
		if !it.Next() || it.Sample() != want {
			t.Fatal("iterator lost its buffer")
		}
	}
	if it.Next() || it.Err() != nil {
		t.Fatal(it.Err())
	}
	if _, _, err = c.Encode(samples); err == nil {
		t.Fatal("used a closed codec")
	}
}

func TestSimple8bWordsAndRuns(t *testing.T) {
	for selector, n := range wordCounts {
		width := wordWidths[selector]
		values := make([]uint64, n)
		for i := range values {
			if width == 0 {
				values[i] = 1
			} else {
				values[i] = uint64(1)<<width - 1
			}
		}
		packed := packIntegers(nil, values)
		if len(packed) != 8 || int(binary.LittleEndian.Uint64(packed)>>60) != selector {
			t.Fatalf("selector %d did not get its own word", selector)
		}
	}
}

func TestConcurrentCodecCalls(t *testing.T) {
	c := testCodec(t)
	for range 8 {
		t.Run("worker", func(t *testing.T) {
			t.Parallel()
			for range 50 {
				assertRoundTrip(t, c, []Sample{{At: 1, Value: math.Copysign(0, -1)}, {At: 2, Value: math.NaN()}})
			}
		})
	}
}

func BenchmarkCodec(b *testing.B) {
	c := testCodec(b)
	for _, kind := range []string{"integer", "float"} {
		samples := make([]Sample, MaxSamples)
		for i := range samples {
			v := float64(1000 + i%17)
			if kind == "float" {
				v = math.Sin(float64(i) / 30)
			}
			samples[i] = Sample{At: int64(i) * 15000, Value: v}
		}
		head, payload := assertRoundTrip(b, c, samples)
		b.Run(kind+"/encode", func(b *testing.B) {
			b.ReportAllocs()
			for b.Loop() {
				if _, _, err := c.Encode(samples); err != nil {
					b.Fatal(err)
				}
			}
		})
		b.Run(kind+"/decode", func(b *testing.B) {
			b.ReportAllocs()
			for b.Loop() {
				it, err := c.Decode(head, payload)
				if err != nil {
					b.Fatal(err)
				}
				for it.Next() {
					_ = it.Sample()
				}
				if it.Err() != nil {
					b.Fatal(it.Err())
				}
			}
		})
	}
}

func checksum(head Head, payload []byte) {
	end := len(payload) - checksumSize
	sum := crc32.Update(crc32.Checksum(head.bytes(), checksumTable), checksumTable, payload[:end])
	binary.LittleEndian.PutUint32(payload[end:], sum)
}

func FuzzDecode(f *testing.F) {
	c := testCodec(f)
	for _, n := range []int{1, 8, 16, 240} {
		samples := make([]Sample, n)
		for i := range samples {
			samples[i] = Sample{At: int64(i) * 15000, Value: float64(i % 5)}
		}
		head, body := assertRoundTrip(f, c, samples)
		f.Add(append(head.bytes(), body...))
	}
	f.Fuzz(func(t *testing.T, data []byte) {
		const headBytes = 26
		if len(data) < headBytes || len(data) > headBytes+maxBody+headerSize+checksumSize {
			return
		}
		head := Head{
			Start: int64(binary.LittleEndian.Uint64(data)),
			End:   int64(binary.LittleEndian.Uint64(data[8:])),
			Count: int(binary.LittleEndian.Uint16(data[16:])),
			First: math.Float64frombits(binary.LittleEndian.Uint64(data[18:])),
		}
		payload := slices.Clone(data[headBytes:])
		if len(payload) >= headerSize+checksumSize {
			checksum(head, payload)
		}
		it, err := c.Decode(head, payload)
		if err != nil {
			return
		}
		var samples []Sample
		for it.Next() {
			samples = append(samples, it.Sample())
			if len(samples) > MaxSamples {
				t.Fatal("unbounded iterator")
			}
		}
		if it.Err() == nil {
			assertRoundTrip(t, c, samples)
		}
	})
}

// valueSets are one of each representation, and the values a round trip must not bend
func valueSets() [][]float64 {
	random := rand.New(rand.NewPCG(7, 11))
	sets := [][]float64{{42}, {math.Copysign(0, -1), math.Inf(1), math.Float64frombits(0x7ff8000000001234)}}
	for _, n := range []int{2, 8, 33, MaxSamples} {
		integers, decimals, smooth, noise := make([]float64, n), make([]float64, n), make([]float64, n), make([]float64, n)
		for i := range n {
			integers[i] = float64(i*7 - 40)
			decimals[i] = float64(200+i%17) / 10
			smooth[i] = math.Sin(float64(i) / 9)
			noise[i] = math.Float64frombits(random.Uint64())
		}
		sets = append(sets, integers, decimals, smooth, noise)
	}
	return sets
}

func decodeValues(t testing.TB, c *Codec, first float64, count int, stream []byte) ([]float64, error) {
	t.Helper()
	it, err := c.DecodeValues(first, count, stream)
	if err != nil {
		return nil, err
	}
	var values []float64
	for it.Next() {
		if it.Sample().At != int64(len(values)) {
			t.Fatalf("value %d carries timestamp %d", len(values), it.Sample().At)
		}
		values = append(values, it.Sample().Value)
	}
	return values, it.Err()
}

func TestValueStreamIsEncodeWithoutItsEnvelope(t *testing.T) {
	c := testCodec(t)
	for _, values := range valueSets() {
		stream, err := c.EncodeValues(values)
		if err != nil {
			t.Fatal(err)
		}
		samples := make([]Sample, len(values))
		for i, value := range values {
			samples[i] = Sample{At: int64(i), Value: value}
		}
		_, body, err := c.Encode(samples)
		if err != nil {
			t.Fatal(err)
		}
		if want := append([]byte{body[1]}, body[headerSize:len(body)-checksumSize]...); !bytes.Equal(stream, want) {
			t.Fatalf("%d values: stream %x, want %x", len(values), stream, want)
		}

		decoded, err := decodeValues(t, c, values[0], len(values), stream)
		if err != nil || len(decoded) != len(values) {
			t.Fatalf("%d values: decoded %d, %v", len(values), len(decoded), err)
		}
		for i := range values {
			if math.Float64bits(decoded[i]) != math.Float64bits(values[i]) {
				t.Fatalf("value %d of %d changed", i, len(values))
			}
		}
	}
}

func TestValueStreamCorruptionIsRefused(t *testing.T) {
	c := testCodec(t)
	values := valueSets()[len(valueSets())-2]
	stream, err := c.EncodeValues(values)
	if err != nil {
		t.Fatal(err)
	}
	for name, corrupt := range map[string]func() (float64, int, []byte){
		"empty":           func() (float64, int, []byte) { return values[0], len(values), nil },
		"no values":       func() (float64, int, []byte) { return values[0], 0, stream },
		"too many values": func() (float64, int, []byte) { return values[0], MaxSamples + 1, stream },
		"one value more":  func() (float64, int, []byte) { return values[0], len(values) + 1, stream },
		"timestamps": func() (float64, int, []byte) {
			return values[0], len(values), append([]byte{stream[0] | timeDelta}, stream[1:]...)
		},
		"truncated":      func() (float64, int, []byte) { return values[0], len(values), stream[:len(stream)-1] },
		"trailing bytes": func() (float64, int, []byte) { return values[0], len(values), append(slices.Clone(stream), 0) },
	} {
		first, count, bytes := corrupt()
		if _, err := decodeValues(t, c, first, count, bytes); !errors.Is(err, ErrInvalid) {
			t.Errorf("%s: %v", name, err)
		}
	}
}

func FuzzDecodeValues(f *testing.F) {
	c := testCodec(f)
	for _, values := range valueSets() {
		stream, err := c.EncodeValues(values)
		if err != nil {
			f.Fatal(err)
		}
		f.Add(math.Float64bits(values[0]), uint8(len(values)), stream)
	}
	f.Fuzz(func(t *testing.T, first uint64, count uint8, stream []byte) {
		values, err := decodeValues(t, c, math.Float64frombits(first), int(count), stream)
		if err == nil && len(values) != int(count) {
			t.Fatalf("decoded %d of %d values", len(values), count)
		}
	})
}
