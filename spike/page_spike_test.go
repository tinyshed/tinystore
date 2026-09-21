package spike

import (
	"os"
	"testing"

	"github.com/tinyshed/tinystore/codec"
)

// a page is bought whole, so what matters is how many block rows fit one
func TestWhatAPageSizeCostsEachClass(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1 to measure")
	}
	blocks, err := codec.New()
	if err != nil {
		t.Fatal(err)
	}
	defer blocks.Close()

	for _, class := range denseClasses {
		for _, pageSize := range []int{4096, 8192, 16384, 32768} {
			schema := squeezedSchema("both", true, true, class == "counter")
			schema.pageSize = pageSize
			result := measureDense(t, class, 10000, 240, schema, blocks.Encode)
			payload, metadata, indexes, waste := result.lines()
			t.Logf("%-12s page=%5d actual=%d  total=%.3f  payload=%.3f  metadata=%.3f  indexes=%.3f  waste=%.3f  scan=%s",
				class, pageSize, result.pageSize, result.perSample(result.file), payload, metadata, indexes, waste,
				result.summaryScan)
		}
	}
}
