package metrics_test

import (
	"context"
	"fmt"
	"os"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/metrics"
)

func ExampleStore() {
	ctx := context.Background()
	directory, err := os.MkdirTemp("", "tinystore-example-")
	if err != nil {
		panic(err)
	}
	defer os.RemoveAll(directory)

	// Manual: this example runs Maintain itself instead of waiting a minute
	store, err := tinystore.Open(ctx, directory, tinystore.Options{Manual: true})
	if err != nil {
		panic(err)
	}
	cpu, err := metrics.Open(ctx, store, metrics.Options{})
	if err != nil {
		panic(err)
	}

	series := metrics.Series{Labels: []metrics.Label{{Name: "__name__", Value: "cpu"}, {Name: "host", Value: "web-1"}}}
	start := time.Now().UnixMilli()
	points := make([]metrics.Sample, 241)
	for i := range points {
		points[i] = metrics.Sample{At: start + int64(i), Value: 42}
	}
	if err = cpu.Ingest(ctx, []metrics.Batch{{Series: series, Samples: points}}); err != nil {
		panic(err)
	}

	// 240 older samples can seal; the newest one remains replaceable in the head
	work, err := cpu.Maintain(ctx)
	if err != nil {
		panic(err)
	}
	if err = store.Close(ctx); err != nil {
		panic(err)
	}

	store, err = tinystore.Open(ctx, directory, tinystore.Options{Manual: true})
	if err != nil {
		panic(err)
	}
	defer store.Close(ctx)
	cpu, err = metrics.Open(ctx, store, metrics.Options{})
	if err != nil {
		panic(err)
	}
	result, err := cpu.Read(ctx, metrics.Range{Matchers: series.Labels, From: start + 237, To: start + 241})
	if err != nil {
		panic(err)
	}
	fmt.Println("sealed blocks:", work.SealedBlocks)
	fmt.Print("values:")
	for _, point := range result[0].Samples {
		fmt.Print(" ", point.Value)
	}
	fmt.Println()
	// Output:
	// sealed blocks: 1
	// values: 42 42 42 42
}
