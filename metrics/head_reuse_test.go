package metrics

import (
	"bytes"
	"math"
	"testing"
)

func TestUnchangedHeadChunksKeepTheirEncodedBytes(t *testing.T) {
	s, _ := openTestStore(t, Options{})
	old := testSamples(480)
	packed, err := s.encodeHead(t.Context(), 1, old)
	if err != nil {
		t.Fatal(err)
	}
	chunks, err := s.parseHead(snapshotOf(1, old, packed))
	if err != nil {
		t.Fatal(err)
	}
	for _, test := range []struct {
		name     string
		incoming []Sample
		want     int
	}{
		{name: "append", incoming: []Sample{{At: testEpoch + 480, Value: math.Float64frombits(0x7ff8000000004321)}}, want: 480},
		{name: "late replacement", incoming: []Sample{{At: testEpoch + 300, Value: math.Float64frombits(0x7ff8000000004321)}}, want: 240},
	} {
		t.Run(test.name, func(t *testing.T) {
			merged, err := mergeHead(old, test.incoming, s.opts.MaxHeadSamples)
			if err != nil {
				t.Fatal(err)
			}
			kept := reusableChunks(chunks, test.incoming[0].At)
			if countChunkSamples(kept) != test.want {
				t.Fatalf("reusable prefix: %d", countChunkSamples(kept))
			}
			updated, err := s.encodeHeadAfter(t.Context(), 1, kept, merged[test.want:])
			if err != nil {
				t.Fatal(err)
			}
			after, err := s.parseHead(snapshotOf(1, merged, updated))
			if err != nil {
				t.Fatal(err)
			}
			if !bytes.Equal(storedBytes(reusableChunks(after, test.incoming[0].At)), storedBytes(kept)) {
				t.Fatal("unchanged chunk bytes moved")
			}
			decoded, err := s.decodeHead(t.Context(), snapshotOf(1, merged, updated))
			if err != nil {
				t.Fatal(err)
			}
			for i := range merged {
				if decoded[i].At != merged[i].At || math.Float64bits(decoded[i].Value) != math.Float64bits(merged[i].Value) {
					t.Fatalf("sample %d changed", i)
				}
			}
		})
	}
}

func snapshotOf(id int64, points []Sample, packed []byte) headSnapshot {
	return headSnapshot{seriesID: id, count: len(points), start: points[0].At, end: points[len(points)-1].At, packed: packed}
}

func storedBytes(chunks []headChunk) []byte {
	var out []byte
	for _, chunk := range chunks {
		out = append(out, chunk.stored...)
	}
	return out
}
