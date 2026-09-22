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

func readyChurn(ctx context.Context, dir string, rounds int) {
	if rounds < 1 || rounds > 100 {
		log.Fatal("ready churn requires 1..100 rounds")
	}
	path := filepath.Join(dir, "ready-churn.db")
	for _, suffix := range []string{"", "-wal", "-shm"} {
		if err := os.Remove(path + suffix); err != nil && !os.IsNotExist(err) {
			log.Fatal(err)
		}
	}
	store, err := metrics.Open(ctx, path, metrics.Options{
		Retention:  365 * 24 * time.Hour,
		Lateness:   time.Hour,
		MaxSeries:  1,
		MaxReaders: 1,
	})
	if err != nil {
		log.Fatal(err)
	}
	series := metrics.Series{Labels: []metrics.Label{{Name: "__name__", Value: "churn"}}}
	start := time.Now().UnixMilli() - int64(240+rounds)*10000
	initial := make([]metrics.Sample, 240)
	for i := range initial {
		initial[i] = metrics.Sample{At: start + int64(i)*10000, Value: float64(i)}
	}
	if err := store.Ingest(ctx, []metrics.Batch{{Series: series, Samples: initial}}); err != nil {
		log.Fatal(err)
	}
	maintenance := &latencies{}
	w := watch(path)
	begin := time.Now()
	for i := 240; i < 240+rounds; i++ {
		point := metrics.Sample{At: start + int64(i)*10000, Value: float64(i)}
		if err := store.Ingest(ctx, []metrics.Batch{{Series: series, Samples: []metrics.Sample{point}}}); err != nil {
			log.Fatal(err)
		}
		at := time.Now()
		result, err := store.Maintain(ctx)
		if err != nil || result.SealedBlocks != 0 {
			log.Fatalf("unsafe maintenance: %+v, %v", result, err)
		}
		maintenance.add(time.Since(at))
	}
	elapsed := time.Since(begin)
	w.finish()
	if err := store.Close(ctx); err != nil {
		log.Fatal(err)
	}
	p50, p95, p99, worst := maintenance.quantiles()
	report("ready_churn",
		"rounds", rounds,
		"seconds", strconv.FormatFloat(elapsed.Seconds(), 'f', 2, 64),
		"maintain_p50_us", micros(p50), "maintain_p95_us", micros(p95), "maintain_p99_us", micros(p99), "maintain_max_us", micros(worst),
		"observed_wal_growth_bytes", w.walGrowth,
		"wal_peak_bytes", w.walPeak,
		"file_bytes_closed", fileBytes(path),
	)
}
