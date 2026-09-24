package metrics

import (
	"bytes"
	"math"
	"math/rand/v2"
	"testing"

	"github.com/tinyshed/tinystore/codec"
)

func encodingStore(t testing.TB) *Store {
	t.Helper()
	encoder, err := codec.New()
	if err != nil {
		t.Fatal(err)
	}
	decoder, err := codec.New()
	if err != nil {
		t.Fatal(err)
	}
	metadata, err := newMetadataCodec()
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { encoder.Close(); decoder.Close(); metadata.close() })
	return &Store{encoder: encoder, decoder: decoder, metadata: metadata}
}

func encodedTestBlock(t testing.TB, s *Store, points []Sample) storedBlock {
	t.Helper()
	body, _, err := s.encodeValues(points, -2)
	if err != nil {
		t.Fatal(err)
	}
	b := storedBlock{head: codec.Head{Start: points[0].At, End: points[len(points)-1].At, Count: len(points), First: points[0].Value}, clock: encodeClockValues(points), summary: summarize(points, Gauge)}
	b.body = sealValueBody(b, body)
	b.bodyBytes = len(b.body)
	return b
}

func TestEveryValueRepresentationPreservesBits(t *testing.T) {
	s := encodingStore(t)
	random := rand.New(rand.NewPCG(1, 2))
	for _, shape := range []string{"constant", "changes", "decimal", "grid", "arbitrary"} {
		points := make([]Sample, 240)
		at := int64(-10000)
		for i := range points {
			at += 10 + int64(random.IntN(4))*10
			value := float64(i % 37)
			switch shape {
			case "constant":
				value = math.Float64frombits(0x7ff800000000abcd)
			case "changes":
				value = float64(i / 80)
			case "decimal":
				value = float64(i%37) / 100
			case "grid":
				value = math.Nextafter(float64(i%37)/100, math.Inf(1))
			case "arbitrary":
				value = math.Float64frombits(random.Uint64())
			}
			points[i] = Sample{At: at, Value: value}
		}
		block := encodedTestBlock(t, s, points)
		back, err := s.decodeBlock(block)
		if err != nil {
			t.Fatalf("%s: %v", shape, err)
		}
		assertSamples(t, back, points)
		if shape == "constant" && len(block.body) != 0 {
			t.Fatal("constant has payload")
		}
		if shape == "changes" && block.body[0] != valuesChanges {
			t.Fatal("changes not selected")
		}
		if shape == "grid" {
			body, err := s.gridValues(points, 2)
			if err != nil || body == nil {
				t.Fatal("grid candidate", err)
			}
			block.body = sealValueBody(block, body)
			back, err = s.decodeBlock(block)
			if err != nil {
				t.Fatal(err)
			}
			assertSamples(t, back, points)
		}
	}
}

func TestChangeValuesWriteWhereAndByHowMuchValuesChange(t *testing.T) {
	values := []float64{5, 5, 5, 7, 7, 9}
	points := make([]Sample, len(values))
	for i, value := range values {
		points[i] = Sample{At: int64(i), Value: value}
	}
	body := changeValues(points)
	if want := []byte{valuesChanges, 0, 3, 4, 2, 4}; !bytes.Equal(body, want) {
		t.Fatalf("body %x, want %x", body, want)
	}
	decoded, err := readChanges(codec.Head{End: 5, Count: len(values), First: values[0]}, body[1:])
	if err != nil || len(decoded) != len(values) {
		t.Fatalf("decoded %d: %v", len(decoded), err)
	}
	for i, point := range decoded {
		if math.Float64bits(point.Value) != math.Float64bits(values[i]) {
			t.Fatalf("value %d: %v", i, point.Value)
		}
	}
}
