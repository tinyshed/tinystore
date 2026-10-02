package metrics

import (
	"bytes"
	"encoding/binary"
	"errors"
	"math"
	"math/big"
	"testing"
)

func TestExactSummariesKeepFiniteUnitsAndCancelOverflow(t *testing.T) {
	for _, value := range []float64{
		0, math.Copysign(0, -1), math.SmallestNonzeroFloat64,
		-math.SmallestNonzeroFloat64, 0.1, -1.25, math.MaxFloat64, -math.MaxFloat64,
	} {
		var want, got big.Int
		if err := finiteUnits(value, &want); err != nil {
			t.Fatal(err)
		}
		encoded := encodeExact(&want)
		reader := binaryReader{data: encoded}
		if !bytes.Equal(readExact(&reader), encoded) || reader.finish() != nil {
			t.Fatal("canonical round trip")
		}
		if err := exactValue(encoded, &got); err != nil || want.Cmp(&got) != 0 {
			t.Fatalf("units: %v", err)
		}
	}
	points := []Sample{{Value: math.MaxFloat64}, {Value: math.MaxFloat64}, {Value: -math.MaxFloat64}}
	summary := exactSummarize(points, Gauge)
	var sum big.Int
	if err := exactValue(summary.exactSum, &sum); err != nil {
		t.Fatal(err)
	}
	value, overflow := roundedExact(&sum)
	if value != math.MaxFloat64 || overflow {
		t.Fatalf("cancellation: %g %v", value, overflow)
	}
}

func TestExactSummaryEncodingRefusesNoncanonicalOrUnboundedFields(t *testing.T) {
	tooLarge := appendCount(nil, 1)
	tooLarge = binary.AppendUvarint(tooLarge, exactSummaryBits<<1)
	tooLarge = append(tooLarge, 1)
	for _, data := range [][]byte{{0x80, 0}, {1, 0, 0}, {1, 0, 2}, {1, 0}, tooLarge, {0xff, 0xff, 0x7f}} {
		reader := binaryReader{data: data}
		readExact(&reader)
		if !errors.Is(reader.finish(), ErrCorrupt) {
			t.Fatalf("accepted %x", data)
		}
	}
}

func FuzzExactSummary(f *testing.F) {
	for _, value := range []float64{0, 0.1, math.MaxFloat64, math.SmallestNonzeroFloat64} {
		var units big.Int
		_ = finiteUnits(value, &units)
		f.Add(encodeExact(&units))
	}
	f.Fuzz(func(t *testing.T, data []byte) {
		reader := binaryReader{data: data}
		encoded := readExact(&reader)
		if reader.finish() == nil && !bytes.Equal(encoded, data) {
			t.Fatal("noncanonical accepted")
		}
	})
}
