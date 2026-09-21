package spike

import (
	"os"
	"testing"

	"github.com/tinyshed/tinystore/codec"
)

// everything the night arrived at, in one run: the codec as it now is, no
// foreign key, retention that walks series, a summary shaped by kind, a span
// instead of an absolute end, and a block closed before it stops fitting a page
func TestWhereTheNightEnded(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1 to measure")
	}
	blocks, err := codec.New()
	if err != nil {
		t.Fatal(err)
	}
	defer blocks.Close()

	const samples = 2400000
	results := make([]denseResult, 0, len(denseClasses))
	for _, class := range denseClasses {
		perBlock := 240
		for perBlock > 30 {
			_, body, encodeErr := blocks.Encode(adaptiveSamples(class, perBlock, 1))
			if encodeErr != nil {
				t.Fatal(encodeErr)
			}
			if len(body) <= 1300 {
				break
			}
			perBlock -= 8
		}
		schema := squeezedSchema("the night's", true, true, class == "counter")
		result := measureDense(t, class, samples/perBlock, perBlock, schema, blocks.Encode)
		results = append(results, result)
		payload, metadata, indexes, waste := result.lines()
		t.Logf("%-12s %3d samples a block  total=%.3f  payload=%.3f  metadata=%.3f  indexes=%.3f  waste=%.3f",
			class, perBlock, result.perSample(result.file), payload, metadata, indexes, waste)
	}
	reportDense(t, results)
}
