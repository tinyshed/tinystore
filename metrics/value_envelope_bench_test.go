package metrics

import (
	"encoding/binary"
	"hash/crc32"
	"math"
	"testing"

	"github.com/tinyshed/tinystore/codec"
)

func BenchmarkOrdinaryEnvelope(b *testing.B) {
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
	head := codec.Head{Start: 0, End: 239, Count: 240, First: points[0].Value}
	encoded := append([]byte{1, body[1], 0, 0}, body[2:]...)
	key := binary.LittleEndian.AppendUint64(nil, 0)
	key = binary.LittleEndian.AppendUint64(key, uint64(head.End))
	key = binary.LittleEndian.AppendUint16(key, uint16(head.Count))
	key = binary.LittleEndian.AppendUint64(key, math.Float64bits(head.First))
	table := crc32.MakeTable(crc32.Castagnoli)
	sum := crc32.Update(crc32.Checksum(key, table), table, encoded)
	encoded = binary.LittleEndian.AppendUint32(encoded, sum)
	b.Run("rebuild_envelope", func(b *testing.B) {
		b.ReportAllocs()
		for range b.N {
			decoded, err := store.readOrdinary(head, body)
			if err != nil || len(decoded) != len(points) {
				b.Fatalf("ordinary decode: %v", err)
			}
		}
	})
	b.Run("prepared_envelope", func(b *testing.B) {
		b.ReportAllocs()
		for range b.N {
			iterator, err := decoder.Decode(head, encoded)
			if err != nil {
				b.Fatal(err)
			}
			decoded := make([]Sample, 0, head.Count)
			for iterator.Next() {
				decoded = append(decoded, iterator.Sample())
			}
			if err := iterator.Err(); err != nil || len(decoded) != len(points) {
				b.Fatalf("prepared decode: %v", err)
			}
		}
	})
}
