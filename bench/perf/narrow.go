package main

import (
	"context"
	"log"
	"os"
	"path/filepath"
	"time"

	"github.com/tinyshed/tinystore/metrics"
)

func narrowPopulate(ctx context.Context, dir string, seriesCount, samples int) {
	if seriesCount < 1 || samples < 241 || samples > 8192 {
		log.Fatal("narrow fixture requires positive series and 241..8192 head samples")
	}
	path := filepath.Join(dir, "read.db")
	for _, suffix := range []string{"", "-wal", "-shm"} {
		if err := os.Remove(path + suffix); err != nil && !os.IsNotExist(err) {
			log.Fatal(err)
		}
	}
	store := openStore(ctx, path, seriesCount, 2)
	series := buildSeries(seriesCount)
	start := time.Now().UnixMilli() - int64(samples)*10000
	for first := 0; first < seriesCount; first += 100 {
		last := min(first+100, seriesCount)
		batches := make([]metrics.Batch, 0, last-first)
		for id := first; id < last; id++ {
			points := make([]metrics.Sample, samples)
			for i := range points {
				points[i] = metrics.Sample{At: start + int64(i)*10000, Value: float64(id + i%97)}
			}
			batches = append(batches, metrics.Batch{Series: series[id], Samples: points})
		}
		if err := store.Ingest(ctx, batches); err != nil {
			log.Fatal(err)
		}
	}
	if err := store.Close(ctx); err != nil {
		log.Fatal(err)
	}
	report("narrow_populate", "series", seriesCount, "head_samples_per_series", samples, "file_bytes", fileBytes(path))
}
