package codec

import (
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

func assertRoundTrip(t testing.TB, c *Codec, samples []Sample) []byte {
	t.Helper()
	encoded, err := c.Encode(samples)
	if err != nil {
		t.Fatal(err)
	}
	it, err := c.Decode(encoded)
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
	return encoded
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
		if _, err := c.Encode(samples); err == nil {
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
	payload := assertRoundTrip(t, c, samples)
	for i := range payload {
		if _, err := c.Decode(payload[:i]); !errors.Is(err, ErrInvalid) {
			t.Fatalf("truncation %d: %v", i, err)
		}
		broken := slices.Clone(payload)
		broken[i] ^= 0x80
		if _, err := c.Decode(broken); !errors.Is(err, ErrInvalid) {
			t.Fatalf("bit flip %d: %v", i, err)
		}
	}
	for _, change := range []func([]byte){
		func(p []byte) { p[2] = 99 }, func(p []byte) { p[3] = 99 },
		func(p []byte) { p[4] = 99 }, func(p []byte) { p[5] = 99 },
		func(p []byte) { binary.LittleEndian.PutUint16(p[6:], 65535) },
		func(p []byte) { binary.LittleEndian.PutUint16(p[8:], 65535) },
	} {
		broken := slices.Clone(payload)
		change(broken)
		checksum(broken)
		if _, err := c.Decode(broken); !errors.Is(err, ErrInvalid) {
			t.Fatalf("header corruption: %v", err)
		}
	}
}

func TestIteratorOwnsItsBytesAndOutlivesTheCodec(t *testing.T) {
	c := testCodec(t)
	samples := []Sample{{At: 1, Value: -1}, {At: 3, Value: 7}}
	payload := assertRoundTrip(t, c, samples)
	it, err := c.Decode(payload)
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
	if _, err = c.Encode(samples); err == nil {
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
		payload := assertRoundTrip(b, c, samples)
		b.Run(kind+"/encode", func(b *testing.B) {
			b.ReportAllocs()
			for b.Loop() {
				if _, err := c.Encode(samples); err != nil {
					b.Fatal(err)
				}
			}
		})
		b.Run(kind+"/decode", func(b *testing.B) {
			b.ReportAllocs()
			for b.Loop() {
				it, err := c.Decode(payload)
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

func checksum(payload []byte) {
	end := len(payload) - checksumSize
	binary.LittleEndian.PutUint32(payload[end:], crc32.Checksum(payload[:end], checksumTable))
}

func FuzzDecode(f *testing.F) {
	c := testCodec(f)
	for _, n := range []int{1, 8, 16, 240} {
		samples := make([]Sample, n)
		for i := range samples {
			samples[i] = Sample{At: int64(i) * 15000, Value: float64(i % 5)}
		}
		f.Add(assertRoundTrip(f, c, samples))
	}
	f.Fuzz(func(t *testing.T, data []byte) {
		if len(data) > maxBody+headerSize+checksumSize {
			return
		}
		payload := slices.Clone(data)
		if len(payload) >= headerSize+checksumSize {
			checksum(payload)
		}
		it, err := c.Decode(payload)
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
