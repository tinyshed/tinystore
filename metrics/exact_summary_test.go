package metrics

import (
	"bytes"
	"encoding/binary"
	"encoding/hex"
	"errors"
	"math"
	"math/big"
	"testing"

	"github.com/tinyshed/tinystore/codec"
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

func TestVersionFourExactDirectoryGolden(t *testing.T) {
	const clockVector = "010100ef01f00100b9a118b1"
	const golden = "040101000000000000000000000000000000010000000000000000d0027f0002ee10013b00004566d681"
	s := encodingStore(t)
	clock, err := hex.DecodeString(clockVector)
	if err != nil {
		t.Fatal(err)
	}
	clocks, err := decodeClockGroup(clock)
	if err != nil {
		t.Fatal(err)
	}
	points := make([]Sample, 240)
	for i := range points {
		points[i] = Sample{At: int64(i), Value: 42}
	}
	block := clocks[0]
	block.head = codec.Head{Start: 0, End: 239, Count: 240, First: 42}
	block.summary = exactSummarize(points, Gauge)
	group := blockGroup{format: 4, seriesID: 7, start: 0, end: 239, clockID: 1, live: 1, blocks: []storedBlock{block}}
	encoded, err := s.writeDirectory(group)
	if err != nil {
		t.Fatal(err)
	}
	if hex.EncodeToString(encoded) != golden {
		t.Fatalf("golden: %s", hex.EncodeToString(encoded))
	}
	decoded, err := s.readDirectory(7, groupRow{start: 0, end: 239, clockID: 1, data: encoded}, clocks)
	if err != nil {
		t.Fatal(err)
	}
	if decoded.format != 4 || !bytes.Equal(decoded.blocks[0].summary.exactSum, block.summary.exactSum) {
		t.Fatal("exact summary changed")
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
