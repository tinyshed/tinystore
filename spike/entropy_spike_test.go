package spike

import (
	"math"
	"math/bits"
	"os"
	"testing"
)

// how much of a float block is shared structure and how much is noise, which
// bounds what any XOR-family or dictionary codec can take off it
func TestHowMuchOfAFloatBlockIsActuallyShared(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1 to measure")
	}
	for _, kind := range []string{"noisy", "sine", "random bits", "temperature"} {
		var sharedPrefix, sharedAny, neighbourWindow float64
		const blocks = 64
		for block := range blocks {
			samples := adaptiveSamples(kind, 240, block)
			common := math.Float64bits(samples[0].Value)
			var union uint64
			previous := math.Float64bits(samples[0].Value)
			window := 0
			for _, s := range samples[1:] {
				current := math.Float64bits(s.Value)
				common &= ^(current ^ math.Float64bits(samples[0].Value))
				union |= current ^ math.Float64bits(samples[0].Value)
				difference := current ^ previous
				if difference != 0 {
					window += 64 - bits.LeadingZeros64(difference) - bits.TrailingZeros64(difference)
				}
				previous = current
			}
			sharedPrefix += float64(bits.LeadingZeros64(union))
			sharedAny += float64(bits.OnesCount64(^union))
			neighbourWindow += float64(window) / float64(len(samples)-1)
		}
		t.Logf("%-12s leading bits shared by the whole block=%.1f  bits never differing=%.1f  "+
			"neighbour XOR window=%.1f bits  floor>=%.3f B/sample",
			kind, sharedPrefix/blocks, sharedAny/blocks, neighbourWindow/blocks,
			(64-sharedAny/blocks)/8)
	}
}
