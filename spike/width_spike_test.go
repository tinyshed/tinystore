package spike

import (
	"encoding/binary"
	"math"
	"math/bits"
	"os"
	"testing"

	"github.com/klauspost/compress/huff0"
	"github.com/klauspost/compress/zstd"
)

var ladderCounts = [...]int{240, 120, 60, 30, 20, 15, 12, 10, 8, 7, 6, 5, 4, 3, 2, 1}

var ladderWidths = [...]uint8{0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 10, 12, 15, 20, 30, 60}

// simple8bBits is what the real packer writes: one word per group, at the width
// of the widest value in it
func simple8bBits(values []uint64) int {
	total := 0
	for len(values) > 0 {
		for selector, capacity := range ladderCounts {
			width := ladderWidths[selector]
			n := min(capacity, len(values))
			fits := width != 0 || len(values) >= capacity
			for _, v := range values[:n] {
				if width == 0 {
					fits = fits && v == 1
				} else {
					fits = fits && v < uint64(1)<<width
				}
			}
			if !fits {
				continue
			}
			total += 64
			values = values[n:]
			break
		}
	}
	return total
}

func TestWhereTheValueStreamStillHasMeat(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1 to measure")
	}
	for _, kind := range []string{"integers", "counter", "temperature", "jitter integers"} {
		var packed, fixed, deltas int
		frequency := map[uint64]int{}
		for block := range 64 {
			samples := adaptiveSamples(kind, 240, block)
			scale := 1.0
			if kind == "temperature" {
				scale = 10
			}
			zigzags := make([]uint64, 0, len(samples)-1)
			previous := int64(math.Round(samples[0].Value * scale))
			widest := 0
			for _, s := range samples[1:] {
				current := int64(math.Round(s.Value * scale))
				delta := current - previous
				zigzag := uint64(delta)<<1 ^ uint64(delta>>63)
				zigzags = append(zigzags, zigzag)
				frequency[zigzag]++
				widest = max(widest, bits.Len64(zigzag))
				previous = current
			}
			packed += simple8bBits(zigzags)
			fixed += widest * len(zigzags)
			deltas += len(zigzags)
		}
		writer, err := zstd.NewWriter(nil, zstd.WithEncoderConcurrency(1))
		if err != nil {
			t.Fatal(err)
		}
		// one frame per block, because that is what the codec writes
		symbolBytes, varintBytes, huffBytes, fseBytes := 0, 0, 0, 0
		for block := range 64 {
			samples := adaptiveSamples(kind, 240, block)
			scale := 1.0
			if kind == "temperature" {
				scale = 10
			}
			symbols := make([]byte, 0, len(samples))
			varints := make([]byte, 0, len(samples))
			previous := int64(math.Round(samples[0].Value * scale))
			for _, s := range samples[1:] {
				current := int64(math.Round(s.Value * scale))
				delta := current - previous
				zigzag := uint64(delta)<<1 ^ uint64(delta>>63)
				symbols = append(symbols, byte(zigzag))
				varints = binary.AppendUvarint(varints, zigzag)
				previous = current
			}
			symbolBytes += len(writer.EncodeAll(symbols, nil))
			varintBytes += len(writer.EncodeAll(varints, nil))

			var scratch huff0.Scratch
			if out, _, err := huff0.Compress1X(symbols, &scratch); err == nil {
				huffBytes += len(out)
			} else {
				huffBytes += len(symbols)
			}
			var wide huff0.Scratch
			if out, _, err := huff0.Compress1X(varints, &wide); err == nil {
				fseBytes += len(out)
			} else {
				fseBytes += len(varints)
			}
		}
		writer.Close()
		symbolBits := 8 * float64(symbolBytes) / float64(deltas)
		varintBits := 8 * float64(varintBytes) / float64(deltas)
		huffBits := 8 * float64(huffBytes) / float64(deltas)
		fseBits := 8 * float64(fseBytes) / float64(deltas)
		_ = varintBits

		var entropy float64
		for _, count := range frequency {
			p := float64(count) / float64(deltas)
			entropy -= p * math.Log2(p)
		}
		t.Logf("%-16s simple8b=%.2f  fixed=%.2f  zstd=%.2f  huff0=%.2f  huff0 varint=%.2f  entropy=%.2f  distinct=%d",
			kind, float64(packed)/float64(deltas), float64(fixed)/float64(deltas),
			symbolBits, huffBits, fseBits, entropy, len(frequency))
	}
}
