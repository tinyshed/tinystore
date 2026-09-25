package records

import (
	"errors"
	"math"
	"math/rand/v2"
	"slices"
	"testing"

	"github.com/klauspost/compress/zstd"

	"github.com/tinyshed/tinystore"
)

func testCoders(t testing.TB) (*encoder, *decoder) {
	t.Helper()
	blobs, unpack, err := newBlobCoders()
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		_ = blobs.Close()
		unpack.Close()
	})
	return newEncoder(blobs), newDecoder(unpack)
}

func roundTripInts(t *testing.T, e *encoder, d *decoder, values []int64) []byte {
	t.Helper()
	encoded := e.appendInts(nil, values)
	c := cursor{data: encoded}
	decoded := d.ints(&c, len(values))
	if err := c.finish(); err != nil {
		t.Fatalf("%v: %v", values, err)
	}
	if !slices.Equal(decoded, values) && (len(decoded) != 0 || len(values) != 0) {
		t.Fatalf("decoded %v, want %v", decoded, values)
	}
	return encoded
}

func TestIntegerColumnsRoundTripInEveryLayout(t *testing.T) {
	e, d := testCoders(t)
	random := rand.New(rand.NewPCG(1, 2))
	sorted := make([]int64, 1000)
	for i := range sorted {
		sorted[i] = fixtureBase + int64(i)*1_000_000 + random.Int64N(1_000_000)
	}
	cases := map[string][]int64{
		"empty":      {},
		"one":        {-7},
		"constant":   slices.Repeat([]int64{42}, 100),
		"sorted":     sorted,
		"trend":      trendValues(100, 1_000_003, 5),
		"extremes":   {math.MinInt64, math.MaxInt64, 0, -1, 1, math.MinInt64, math.MaxInt64},
		"small":      randomValues(random, 500, 3),
		"alphabet":   randomValues(random, 500, 200),
		"wide":       randomValues(random, 500, math.MaxInt64),
		"dominant":   dominantValues(500),
		"dictionary": {1 << 40, 7, 1 << 40, 7, 1 << 40, -9, 7, 7, 1 << 40, -9, 7, 1 << 40},
	}
	for name, values := range cases {
		t.Run(name, func(t *testing.T) { roundTripInts(t, e, d, values) })
	}
}

func trendValues(n int, step, jitter int64) []int64 {
	values := make([]int64, n)
	for i := range values {
		values[i] = 1_000 + int64(i)*step + int64(i)%jitter
	}
	return values
}

func randomValues(random *rand.Rand, n int, below int64) []int64 {
	values := make([]int64, n)
	for i := range values {
		values[i] = random.Int64N(below)
	}
	return values
}

func dominantValues(n int) []int64 {
	values := slices.Repeat([]int64{200}, n)
	for i := 0; i < n; i += 37 {
		values[i] = 500
	}
	return values
}

// the example in the comment on the integer layouts
func TestADeltaColumnKeepsResidualsOverTheCommonDivisor(t *testing.T) {
	e, _ := testCoders(t)
	values := []int64{1000, 1250, 1250, 1900}
	plan := e.planTransform(values, intPlan{transform: transformDelta, first: values[0]})
	residuals := []uint64{plan.residual(values, 0), plan.residual(values, 1), plan.residual(values, 2)}
	if plan.base != 0 || plan.divisor != 50 || !slices.Equal(residuals, []uint64{5, 0, 13}) ||
		plan.packer != packWidth || plan.width != 4 {
		t.Fatalf("plan %+v, residuals %v; want base 0, gcd 50, 5 0 13 in 4 bits", plan, residuals)
	}
}

// the examples in the comments on radixCost and the rice code
func TestRadixWordsAndRiceCodes(t *testing.T) {
	if group, width, cost := radixCost(5, 3); group != 3 || width != 7 || cost != 7 {
		t.Errorf("three values below 5: %d a word of %d bits, %d bits; want 3 of 7, 7", group, width, cost)
	}
	writer := bitWriter{}
	for _, value := range []uint64{5, 0, 13} {
		writeRice(&writer, value, 2)
	}
	if bits := riceBits(5, 2) + riceBits(0, 2) + riceBits(13, 2); bits != 4+3+6 {
		t.Errorf("rice k=2 of 5, 0, 13 takes %d bits, want 13", bits)
	}
	reader := bitReader{data: writer.finish()}
	for _, want := range []uint64{5, 0, 13} {
		if got := readRice(&reader, 2); got != want {
			t.Errorf("read %d, want %d", got, want)
		}
	}
	escaped := bitWriter{}
	writeRice(&escaped, math.MaxUint64, 0)
	if got := readRice(&bitReader{data: escaped.finish()}, 0); got != math.MaxUint64 {
		t.Errorf("an escaped value read back as %d", got)
	}
}

// the rice parameter is the cheapest of all 64, the example in its comment
// among them, and its cost is what writing the code takes
func TestTheRiceParameterIsTheCheapestOfAll(t *testing.T) {
	example := slices.Concat(make([]int64, 14), []int64{1000000, 1000000})
	plan := intPlan{divisor: 1}
	if k, cost := riceCost(example, &plan, len(example)); k != 0 || cost != 206 {
		t.Errorf("the example: k %d at %d bits, want 0 at 206", k, cost)
	}
	random := rand.New(rand.NewPCG(5, 6))
	for round := range 200 {
		values := randomValues(random, 1+random.IntN(300), int64(1)<<random.IntN(63))
		for i := range values {
			if random.IntN(8) == 0 {
				values[i] = random.Int64()
			}
		}
		k, cost := riceCost(values, &intPlan{divisor: 1}, len(values))
		for candidate := range uint(64) {
			total := uint64(0)
			for _, value := range values {
				total += riceBits(uint64(value), candidate)
			}
			if total < cost || (candidate == k && total != cost) {
				t.Fatalf("round %d: k %d costs %d, but k %d costs %d", round, k, cost, candidate, total)
			}
		}
	}
}

func TestACorruptIntegerColumnIsRefused(t *testing.T) {
	e, d := testCoders(t)
	encoded := e.appendInts(nil, randomValues(rand.New(rand.NewPCG(3, 4)), 300, 1000))
	for cut := range len(encoded) {
		c := cursor{data: encoded[:cut]}
		d.ints(&c, 300)
		if err := c.finish(); !errors.Is(err, tinystore.ErrCorrupt) {
			t.Fatalf("a column cut at %d of %d bytes: %v", cut, len(encoded), err)
		}
	}
	for _, layout := range [][]byte{{9}, {layoutDirect, 7}, {layoutDirect, transformNone, 0, 0}} {
		c := cursor{data: layout}
		d.ints(&c, 3)
		if !errors.Is(c.finish(), tinystore.ErrCorrupt) {
			t.Errorf("layout %v accepted", layout)
		}
	}
}

func BenchmarkIntegerColumn(b *testing.B) {
	blobs, unpack, err := newBlobCoders()
	if err != nil {
		b.Fatal(err)
	}
	defer unpack.Close()
	defer func(blobs *zstd.Encoder) { _ = blobs.Close() }(blobs)
	e, d := newEncoder(blobs), newDecoder(unpack)
	values := make([]int64, 1024)
	for i, record := range frontendRecords(1024) {
		values[i] = record.At.UnixNano()
	}
	slices.Sort(values)
	encoded := e.appendInts(nil, values)
	b.Run("encode", func(b *testing.B) {
		for b.Loop() {
			e.appendInts(nil, values)
		}
	})
	b.Run("decode", func(b *testing.B) {
		for b.Loop() {
			c := cursor{data: encoded}
			d.ints(&c, len(values))
		}
	})
}
