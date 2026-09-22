package spike

import (
	"math"
	"testing"

	"github.com/tinyshed/tinystore/codec"
)

// TestTimestampAndValueShare splits the payload into what the timestamps cost
// and what the values cost, by encoding the same timestamps twice: once with
// the real values and once with a constant, which the value codec stores in no
// bytes at all. The difference is the value stream.
func TestTimestampAndValueShare(t *testing.T) {
	series := readJSONLCorpus(t)
	c, err := codec.New()
	if err != nil {
		t.Fatal(err)
	}
	defer c.Close()

	var samples, payload, timestamps int
	var blocks, regular int
	step := map[int64]int{}
	for _, s := range series {
		for start := 0; start < len(s.Values); start += 240 {
			points := corpusSamples(s, start, min(start+240, len(s.Values)))
			_, body, err := c.Encode(points)
			if err != nil {
				t.Fatal(err)
			}
			flat := make([]codec.Sample, len(points))
			for i := range flat {
				flat[i] = codec.Sample{At: points[i].At, Value: 1}
			}
			_, flatBody, err := c.Encode(flat)
			if err != nil {
				t.Fatal(err)
			}
			samples += len(points)
			payload += len(body)
			timestamps += len(flatBody)
			blocks++
			even := true
			for i := 2; i < len(points); i++ {
				if points[i].At-points[i-1].At != points[1].At-points[0].At {
					even = false
					break
				}
			}
			if even {
				regular++
			}
			if len(points) > 1 {
				step[points[1].At-points[0].At]++
			}
		}
	}

	per := func(n int) float64 { return float64(n) / float64(samples) }
	t.Logf("SAMPLES %d in %d blocks, %d of them on a single timestamp step (%.1f%%)",
		samples, blocks, regular, 100*float64(regular)/float64(blocks))
	t.Logf("PAYLOAD          %.4f B/sample", per(payload))
	t.Logf("  timestamps and framing %.4f B/sample (%.1f%%)",
		per(timestamps), 100*float64(timestamps)/float64(payload))
	t.Logf("  values                 %.4f B/sample (%.1f%%)",
		per(payload-timestamps), 100*float64(payload-timestamps)/float64(payload))

	best := int64(0)
	for value, count := range step {
		if count > step[best] {
			best = value
		}
	}
	t.Logf("most common first step %d ms in %d blocks of %d", best, step[best], blocks)
	if math.Abs(float64(len(step))) > 0 {
		t.Logf("distinct first steps across blocks: %d", len(step))
	}
}
