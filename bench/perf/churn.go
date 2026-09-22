package main

import (
	"context"
	"log"
	"os"
	"path/filepath"
	"time"

	"github.com/tinyshed/tinystore/metrics"
)

func churnStore(ctx context.Context, path string, seriesCount int) *metrics.Store {
	store, err := metrics.Open(ctx, path, metrics.Options{
		Retention:       time.Second,
		MaxSeries:       seriesCount,
		MaxBatchSamples: seriesCount,
		MaxBatchBytes:   4 << 20,
	})
	if err != nil {
		log.Fatal(err)
	}
	return store
}

func churnPrepare(ctx context.Context, dir string, seriesCount int) {
	path := filepath.Join(dir, "churn.db")
	for _, suffix := range []string{"", "-wal", "-shm"} {
		if err := os.Remove(path + suffix); err != nil && !os.IsNotExist(err) {
			log.Fatal(err)
		}
	}
	store := churnStore(ctx, path, seriesCount)
	series := buildSeries(seriesCount)
	stamp := time.Now().UnixMilli()
	batches := make([]metrics.Batch, seriesCount)
	for i := range batches {
		batches[i] = metrics.Batch{Series: series[i], Samples: []metrics.Sample{{At: stamp, Value: float64(i)}}}
	}
	if err := store.Ingest(ctx, batches); err != nil {
		log.Fatal(err)
	}
	if err := store.Close(ctx); err != nil {
		log.Fatal(err)
	}
	report("churn_prepare", "series", seriesCount, "sample_at_ms", stamp, "file_bytes", fileBytes(path))
}

func churnExpire(ctx context.Context, dir string, seriesCount int) {
	path := filepath.Join(dir, "churn.db")
	store := churnStore(ctx, path, seriesCount)
	var expired, reclaimed int
	begin := time.Now()
	for range (seriesCount+63)/64 + 1 {
		result, err := store.Maintain(ctx)
		if err != nil {
			log.Fatal(err)
		}
		expired += result.ExpiredSamples
		reclaimed += result.ReclaimedSeries
	}
	elapsed := time.Since(begin)
	if err := store.Close(ctx); err != nil {
		log.Fatal(err)
	}
	report("churn_expire", "series", seriesCount, "expired_samples", expired, "reclaimed_series", reclaimed, "elapsed_us", micros(elapsed), "file_bytes", fileBytes(path))
}
