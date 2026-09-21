package spike

// what a query pays for a block size, which is the thing the disk stopped
// arguing about: a block is decompressed whole, so one point costs all of it

import (
	"os"
	"runtime"
	"testing"
	"time"
)

func TestWhatAQueryPaysForABlockSize(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1 to measure")
	}

	writer, reader := baselineCodec(t)

	// one hour of a fifteen-second series is the window a panel usually asks for
	const window = 240

	for _, perBlock := range []int{60, 120, 240, 480, 960, 1920} {
		samples := make([]sample, 0, perBlock)
		for len(samples) < perBlock {
			samples = append(samples, series240("noisy gauge", int64(len(samples))*240*15_000)...)
		}
		samples = samples[:perBlock]
		packed := encode(samples, writer)

		const runs = 200
		runtime.GC()
		var before, after runtime.MemStats
		runtime.ReadMemStats(&before)

		start := time.Now()
		for range runs {
			back, err := decode(packed, reader)
			if err != nil || len(back) != perBlock {
				t.Fatalf("decoding %d samples: %v", perBlock, err)
			}
		}
		each := time.Since(start) / runs
		runtime.ReadMemStats(&after)
		held := float64(after.TotalAlloc-before.TotalAlloc) / runs

		// a window needs every block it touches, whole
		blocks := (window + perBlock - 1) / perBlock
		if perBlock > window {
			blocks = 1
		}
		decoded := blocks * perBlock

		t.Logf("%5d per block: payload %6d B, decode %8s (%5.1f ns/sample), "+
			"holds %7.1f KiB, an hour costs %d blocks and %d samples decoded for %d wanted",
			perBlock, len(packed), each.Round(time.Nanosecond),
			float64(each.Nanoseconds())/float64(perBlock), held/1024, blocks, decoded, window)
	}
}

// TestWhatASampleCostsOnEachKindOfData answers the question the codec table left
// open: the cost is a property of the numbers, not of the format
func TestWhatASampleCostsOnEachKindOfData(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1 to measure")
	}

	writer, _ := baselineCodec(t)
	for _, kind := range []string{"stable gauge", "counter", "whole numbers", "noisy gauge"} {
		total := 0
		const blocks = 32
		for block := range blocks {
			total += len(encode(shaped(kind, int64(block)*240*15_000), writer))
		}
		t.Logf("%-14s %5.2f B per sample", kind, float64(total)/(blocks*240))
	}
}

// shaped is series240 plus the shape the codec table never named: a gauge whose
// values are whole numbers, which is what bytes, counts and percentages really are
func shaped(kind string, start int64) []sample {
	if kind != "whole numbers" {
		return series240(kind, start)
	}

	samples := series240("noisy gauge", start)
	for i := range samples {
		samples[i].value = float64(int64(samples[i].value))
	}
	return samples
}
