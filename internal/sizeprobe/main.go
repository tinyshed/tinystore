// Command sizeprobe links the public API so task size can report what importing it costs.
package main

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"time"

	"github.com/tinyshed/tinystore/codec"
	"github.com/tinyshed/tinystore/metrics"
)

func main() {
	blocks, err := codec.New()
	if err != nil {
		panic(err)
	}
	defer func() { _ = blocks.Close() }()

	head, payload, err := blocks.Encode([]codec.Sample{{At: 1, Value: 1}})
	if err != nil {
		panic(err)
	}
	if _, err = blocks.Decode(head, payload); err != nil {
		panic(err)
	}
	fmt.Println(len(payload))
	directory, err := os.MkdirTemp("", "tinystore-size-")
	if err != nil {
		panic(err)
	}
	defer func() { _ = os.RemoveAll(directory) }()
	ctx := context.Background()
	store, err := metrics.Open(ctx, filepath.Join(directory, "metrics.db"), metrics.Options{})
	if err != nil {
		panic(err)
	}
	defer func() { _ = store.Close(ctx) }()
	at := time.Now().UnixMilli()
	series := metrics.Series{Labels: []metrics.Label{{Name: "__name__", Value: "probe"}}}
	batch := metrics.Batch{Series: series, Samples: []metrics.Sample{{At: at, Value: 1}}}
	if err = store.Ingest(ctx, []metrics.Batch{batch}); err != nil {
		panic(err)
	}
	if _, err = store.Maintain(ctx); err != nil {
		panic(err)
	}
	result, err := store.Read(ctx, metrics.Range{Matchers: series.Labels, From: at, To: at + 1})
	if err != nil {
		panic(err)
	}
	fmt.Println(len(result), store.Stats())
}
