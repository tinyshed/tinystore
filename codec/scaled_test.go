package codec

import (
	"encoding/hex"
	"math"
	"math/rand/v2"
	"testing"
)

func TestDecimalsTravelAsTheIntegersTheyWereWrittenAs(t *testing.T) {
	c := testCodec(t)
	for _, workload := range []struct {
		name  string
		value func(i int) float64
	}{
		{"tenths", func(i int) float64 { return math.Round((20+3*math.Sin(float64(i)/30))*10) / 10 }},
		{"hundredths", func(i int) float64 { return math.Round(float64(9900+i*13)) / 100 }},
		{"money", func(i int) float64 { return math.Round(float64(120000+i*37)) / 100 }},
		{"a percentage", func(i int) float64 { return math.Round(float64(i%1000)) / 10 }},
		{"thousandths that walk", func(i int) float64 { return math.Round(float64(500000+i*3)) / 1000 }},
	} {
		samples := make([]Sample, MaxSamples)
		for i := range samples {
			samples[i] = Sample{At: 1220227200000 + int64(i)*15000, Value: workload.value(i)}
		}
		_, encoded := assertRoundTrip(t, c, samples)
		if encoded[1]>>2&7 != valueScaled {
			t.Errorf("%s chose encoding %d, not the scaled one", workload.name, encoded[4])
		}
		t.Logf("%-22s %.3f B/sample", workload.name, float64(len(encoded))/float64(len(samples)))
	}
}

func TestAValueNoScaleReproducesIsRefusedRatherThanRounded(t *testing.T) {
	for _, value := range []float64{
		math.Copysign(0, -1),
		math.NaN(),
		math.Inf(1),
		math.Inf(-1),
		math.Float64frombits(0x7FF8000000000001),
		0.1 + 0.2,
		math.Pi,
		1 << 62,
		math.MaxFloat64,
		math.SmallestNonzeroFloat64,
	} {
		if scale := exactScale(value); scale >= 0 {
			back := float64(int64(math.Round(value*pow10[scale]))) / pow10[scale]
			if math.Float64bits(back) != math.Float64bits(value) {
				t.Fatalf("%v claimed scale %d and came back as %v", value, scale, back)
			}
		}
	}
	// a block holding one of them must not be encoded as decimals at all
	c := testCodec(t)
	samples := make([]Sample, 64)
	for i := range samples {
		samples[i] = Sample{At: 1220227200000 + int64(i)*15000, Value: math.Round(float64(200+i)) / 10}
	}
	samples[7].Value = math.Copysign(0, -1)
	_, encoded := assertRoundTrip(t, c, samples)
	if encoded[1]>>2&7 == valueScaled {
		t.Error("negative zero survived as a scaled decimal")
	}
}

func TestEveryScaleSurvivesTheRoundTrip(t *testing.T) {
	c := testCodec(t)
	r := rand.New(rand.NewPCG(7, 11))
	for scale := range maxScale + 1 {
		samples := make([]Sample, 32)
		for i := range samples {
			whole := int64(r.IntN(2001) - 1000)
			samples[i] = Sample{At: 1220227200000 + int64(i)*15000, Value: float64(whole) / pow10[scale]}
		}
		assertRoundTrip(t, c, samples)
	}
}

// the decoder is the promise: these bytes were written once and have to keep
// reading the same, on every architecture and in every later version
func TestPayloadsWrittenBeforeStillRead(t *testing.T) {
	c := testCodec(t)
	at := func(i int) int64 { return 1220227200000 + int64(i)*15000 }
	raw := []uint64{
		0x3EDB10AB06962388, 0x46A39F8161C20E58, 0x2354CA478D72AA83, 0xE1165860E7C5F89A,
		0xCAB273D6FA451B6C, 0x02B1CA2441FD740C, 0xEFF7FC068416BF9A, 0xED6ED47DFE1A4F73,
		0xEF05B830D078FA2D, 0x40247CFABB2F23B5, 0x51ED94124939E517, 0x4EF6B2EBAEF5B6D8,
	}
	for _, golden := range []struct {
		name    string
		mode    byte
		count   int
		payload string
		value   func(i int) float64
	}{
		{
			"one repeated value", valueConst, 4,
			"01040000c4a73278",
			func(int) float64 { return 1 },
		},
		{
			"whole numbers", valueInteger, 12,
			"010800000104f00de65e42087318c09f",
			func(i int) float64 { return float64(1000 + i*3 - i%5) },
		},
		{
			"tenths", valueScaled, 12,
			"011000000001b66ddbb601000040355bbf46",
			func(i int) float64 { return math.Round(float64(1000+i*3)) / 10 },
		},
		{
			"neighbouring bit patterns", valueXOR, 12,
			"010c0000ff080000000600000001c0000000180000000f00000000600000001c0000000180000001f00000000600000001c000000010469fefef",
			func(i int) float64 { return 1e20 + float64(i)*16384 },
		},
		{
			"values with nothing in common", valueRaw, 12,
			"01000000580ec261819fa34683aa728d47ca54239af8c5e7605816e16c1b45fad673b2ca0c74fd4124cab1029abf168406fcf7ef734f1afe7dd46eed2dfa78d030b805efb5232fbbfa7c244017e539491294ed51d8b6f5aeebb2f64e2233bd40",
			func(i int) float64 { return math.Float64frombits(raw[i]) },
		},
	} {
		payload, err := hex.DecodeString(golden.payload)
		if err != nil {
			t.Fatal(err)
		}
		if payload[1]>>2&7 != golden.mode {
			t.Errorf("%s: the frozen payload says encoding %d, not %d", golden.name, payload[1]>>2&7, golden.mode)
		}
		head := Head{
			Start: at(0), End: at(golden.count - 1),
			Count: golden.count, First: golden.value(0),
		}
		it, err := c.Decode(head, payload)
		if err != nil {
			t.Fatalf("%s: %v", golden.name, err)
		}
		read := 0
		for it.Next() {
			got := it.Sample()
			want := Sample{At: at(read), Value: golden.value(read)}
			if got.At != want.At || math.Float64bits(got.Value) != math.Float64bits(want.Value) {
				t.Errorf("%s: sample %d read %v at %d, wanted %v at %d",
					golden.name, read, got.Value, got.At, want.Value, want.At)
			}
			read++
		}
		if it.Err() != nil {
			t.Fatalf("%s: %v", golden.name, it.Err())
		}
		if read != golden.count {
			t.Errorf("%s: read %d samples, wanted %d", golden.name, read, golden.count)
		}
	}
}

// 573 of the 9999 two-place decimals have a product that lands just under the
// integer they were written as, so truncating instead of rounding would refuse
// very nearly every block of them
func TestADecimalWhoseProductFallsShortIsStillFound(t *testing.T) {
	c := testCodec(t)
	for _, value := range []float64{0.29, 0.57, 1.15, 2.01, 4.35} {
		product := value * 100
		if int64(product) == int64(math.Round(product)) {
			t.Fatalf("%v no longer exercises the difference between truncating and rounding", value)
		}
		if scale := exactScale(value); scale != 2 {
			t.Errorf("%v was written with two places and got scale %d", value, scale)
		}
	}
	samples := make([]Sample, 64)
	for i := range samples {
		samples[i] = Sample{At: 1220227200000 + int64(i)*15000, Value: math.Round(float64(2900+i*7)) / 100}
	}
	if _, encoded := assertRoundTrip(t, c, samples); encoded[1]>>2&7 != valueScaled {
		t.Errorf("a block of two-place decimals chose encoding %d", encoded[4])
	}
}
