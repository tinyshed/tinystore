package metrics

import (
	"math"
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
