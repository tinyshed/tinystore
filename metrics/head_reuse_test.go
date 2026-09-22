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
			prefix, count, err := reusableHeadPrefix(packed, test.incoming[0].At, s.opts.MaxHeadSamples)
			if err != nil || count != test.want {
				t.Fatalf("reusable prefix: %d, %v", count, err)
			}
			updated, err := s.encodeHeadPrefix(t.Context(), 1, merged, prefix, count)
			if err != nil {
				t.Fatal(err)
			}
			unchanged, _, err := reusableHeadPrefix(updated, test.incoming[0].At, s.opts.MaxHeadSamples)
			if err != nil || !bytes.Equal(unchanged, prefix) {
				t.Fatalf("unchanged chunk bytes moved: %v", err)
			}
			decoded, err := s.decodeHead(t.Context(), headSnapshot{seriesID: 1, count: len(merged), start: merged[0].At, end: merged[len(merged)-1].At, packed: updated})
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
