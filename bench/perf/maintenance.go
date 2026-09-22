package main

import (
	"context"
	"log"
	"os"
	"path/filepath"
	"strconv"
	"time"

	"github.com/tinyshed/tinystore/metrics"
)

func maintenanceBatch(ctx context.Context, dir string, seriesCount, samples int) {
	if seriesCount < 1 || seriesCount > 64 || samples < 241 || samples > 8192 {
		log.Fatal("maintenance batch requires 1..64 series and 241..8192 samples")
	}
	path := filepath.Join(dir, "maintenance.db")
	for _, suffix := range []string{"", "-wal", "-shm"} {
		if err := os.Remove(path + suffix); err != nil && !os.IsNotExist(err) {
			log.Fatal(err)
		}
	}
	store := openStore(ctx, path, seriesCount, 2)
	all := buildSeries(seriesCount)
	start := time.Now().UnixMilli() - int64(samples)*10000
	batches := make([]metrics.Batch, seriesCount)
	for id := range batches {
		points := make([]metrics.Sample, samples)
		for i := range points {
			points[i] = metrics.Sample{At: start + int64(i)*10000, Value: float64((id*17 + i*13) % 97)}
		}
		batches[id] = metrics.Batch{Series: all[id], Samples: points}
	}
	if err := store.Ingest(ctx, batches); err != nil {
		log.Fatal(err)
	}
	w := watch(path)
	begin := time.Now()
	result, err := store.Maintain(ctx)
	elapsed := time.Since(begin)
	if err != nil {
		log.Fatal(err)
	}
	w.finish()
	if err := store.Close(ctx); err != nil {
		log.Fatal(err)
	}
	report("maintenance_batch",
		"series", seriesCount, "samples_per_series", samples,
		"sealed_blocks", result.SealedBlocks,
		"elapsed_us", micros(elapsed),
		"seconds", strconv.FormatFloat(elapsed.Seconds(), 'f', 3, 64),
		"observed_wal_growth_bytes", w.walGrowth,
		"wal_peak_bytes", w.walPeak,
		"file_bytes_closed", fileBytes(path),
	)
}
