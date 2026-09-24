package metrics

import (
	"math"
	"math/rand/v2"
	"testing"

	"github.com/tinyshed/tinystore/codec"
)

func BenchmarkReadOrdinary(b *testing.B) {
	points := make([]Sample, 240)
	for index := range points {
		bits := uint64(index+1) * 0x9e3779b97f4a7c15
		points[index] = Sample{At: int64(index), Value: math.Float64frombits(0x3ff0000000000000 | bits&0x000fffffffffffff)}
	}
	encoder, err := codec.New()
	if err != nil {
		b.Fatal(err)
	}
	defer encoder.Close()
	decoder, err := codec.New()
	if err != nil {
		b.Fatal(err)
	}
	defer decoder.Close()
	store := &Store{encoder: encoder, decoder: decoder}
	body, err := store.ordinaryValues(points)
	if err != nil {
		b.Fatal(err)
	}
	head, payload, err := encoder.Encode(points)
	if err != nil {
		b.Fatal(err)
	}
	b.Run("value_stream", func(b *testing.B) {
		b.ReportAllocs()
		for range b.N {
			decoded, err := store.readOrdinary(head, body)
			if err != nil || len(decoded) != len(points) {
				b.Fatalf("ordinary decode: %v", err)
			}
		}
	})
	b.Run("whole_payload", func(b *testing.B) {
		b.ReportAllocs()
		for range b.N {
			iterator, err := decoder.Decode(head, payload)
			if err != nil {
				b.Fatal(err)
			}
			decoded := make([]Sample, 0, head.Count)
			for iterator.Next() {
				decoded = append(decoded, iterator.Sample())
			}
			if err := iterator.Err(); err != nil || len(decoded) != len(points) {
				b.Fatalf("whole payload decode: %v", err)
			}
		}
	})
}

type benchmarkBlock struct {
	name   string
	points []Sample
	encode func(*Store, []Sample) ([]byte, error)
}

// benchmarkBlocks is one block per reader; changes_many is the changes reader's worst case
func benchmarkBlocks() []benchmarkBlock {
	random := rand.New(rand.NewPCG(3, 5))
	block := func(value func(i int) float64) []Sample {
		points := make([]Sample, 240)
		for i := range points {
			points[i] = Sample{At: int64(i) * 10, Value: value(i)}
		}
		return points
	}
	changesMany := block(func(i int) float64 { return float64(i / 2 * 3) })
	gridResiduals := block(func(int) float64 {
		value := float64(random.IntN(10000)) / 100
		if random.IntN(4) == 0 {
			return math.Nextafter(value, math.Inf(1))
		}
		return value
	})
	ordinary := block(func(int) float64 {
		return math.Float64frombits(0x3ff0000000000000 | random.Uint64()&0x000fffffffffffff)
	})
	changes := func(_ *Store, points []Sample) ([]byte, error) { return changeValues(points), nil }
	grid := func(s *Store, points []Sample) ([]byte, error) { return s.gridValues(points, 2) }
	return []benchmarkBlock{
		{name: "changes_many", points: changesMany, encode: changes},
		{name: "grid_residuals", points: gridResiduals, encode: grid},
		{name: "ordinary", points: ordinary, encode: (*Store).ordinaryValues},
	}
}

func BenchmarkDecodeBlock(b *testing.B) {
	s := encodingStore(b)
	for _, test := range benchmarkBlocks() {
		body, err := test.encode(s, test.points)
		if err != nil || len(body) == 0 {
			b.Fatal(test.name, err)
		}
		points := test.points
		head := codec.Head{Start: points[0].At, End: points[len(points)-1].At, Count: len(points), First: points[0].Value}
		block := storedBlock{head: head, clock: encodeClockValues(points)}
		block.body = sealValueBody(block, body)
		b.Run(test.name, func(b *testing.B) {
			b.ReportAllocs()
			for range b.N {
				if _, err := s.decodeBlock(block); err != nil {
					b.Fatal(err)
				}
			}
		})
	}
}

func BenchmarkEncodeValues(b *testing.B) {
	s := encodingStore(b)
	for _, test := range benchmarkBlocks() {
		b.Run(test.name, func(b *testing.B) {
			b.ReportAllocs()
			for range b.N {
				if _, _, err := s.encodeValues(test.points, -2); err != nil {
					b.Fatal(err)
				}
			}
		})
	}
}
