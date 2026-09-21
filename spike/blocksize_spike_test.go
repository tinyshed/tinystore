package spike

import (
	"os"
	"testing"
	"time"

	"github.com/tinyshed/tinystore/codec"
)

// a narrow query still decodes whole blocks, so a bigger block buys disk with
// somebody else's latency
func TestWhatABlockSizeCostsAQuery(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1 to measure")
	}
	blocks, err := codec.New()
	if err != nil {
		t.Fatal(err)
	}
	defer blocks.Close()

	const totalSamples = 2400000
	const window = 60
	for _, class := range []string{"integers", "counter", "temperature"} {
		for _, perBlock := range []int{30, 60, 120, 240} {
			count := totalSamples / perBlock
			result := measureDense(t, class, count, perBlock, tonightsSchema(), blocks.Encode)

			samples := adaptiveSamples(class, perBlock, 1)
			head, body, encodeErr := blocks.Encode(samples)
			if encodeErr != nil {
				t.Fatal(encodeErr)
			}
			const runs = 500
			start := time.Now()
			for range runs {
				it, decodeErr := blocks.Decode(head, body)
				if decodeErr != nil {
					t.Fatal(decodeErr)
				}
				for it.Next() {
					_ = it.Sample()
				}
				if it.Err() != nil {
					t.Fatal(it.Err())
				}
			}
			perBlockDecode := time.Since(start) / runs
			touched := 2
			if perBlock < window {
				touched = window/perBlock + 1
			}
			payload, metadata, indexes, waste := result.lines()
			t.Logf("%-12s %4d samples a block  total=%.3f  payload=%.3f  metadata=%.3f  indexes=%.3f  waste=%.3f  "+
				"decode=%s a block  an hour's window decodes %dx what it answers",
				class, perBlock, result.perSample(result.file), payload, metadata, indexes, waste,
				perBlockDecode, touched*perBlock/window)
		}
	}
}
