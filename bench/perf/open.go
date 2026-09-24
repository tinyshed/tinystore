package main

import (
	"context"
	"fmt"
	"log"
	"path/filepath"
	"sync"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/metrics"
)

// the store holding each open engine, so that closing one releases its directory
var runtimes sync.Map

// openMetrics opens a Manual store on the directory holding path, a metrics.db,
// and the engine in it; every stage runs Maintain itself, as it always did
func openMetrics(ctx context.Context, path string, options metrics.Options) *metrics.Store {
	if filepath.Base(path) != "metrics.db" {
		log.Fatalf("%s: the engine's file is named metrics.db", path)
	}
	runtime, err := tinystore.Open(ctx, filepath.Dir(path), tinystore.Options{Manual: true})
	if err != nil {
		log.Fatal(err)
	}
	store, err := metrics.Open(ctx, runtime, options)
	if err != nil {
		_ = runtime.Close(ctx)
		log.Fatal(err)
	}
	runtimes.Store(store, runtime)
	return store
}

func closeMetrics(ctx context.Context, store *metrics.Store) error {
	runtime, ok := runtimes.LoadAndDelete(store)
	if !ok {
		return fmt.Errorf("close metrics: not opened by openMetrics")
	}
	return runtime.(*tinystore.Store).Close(ctx)
}
