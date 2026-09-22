package main

import (
	"context"
	"log"
	"os"
	"path/filepath"
	"runtime"
	"strconv"
	"time"

	"github.com/tinyshed/tinystore/metrics"
)

func writePath(ctx context.Context, dir, stage string, seriesCount, samples, batch, seedSamples int) {
	if seriesCount < 1 || samples < 1 || batch < 1 || seedSamples < 1 || (stage == "append" && (seedSamples+samples > 8192 || seedSamples*batch > 200000)) {
		log.Fatal("write path exceeds its series, batch or mutable-head bounds")
	}
	path := filepath.Join(dir, stage+".db")
	for _, suffix := range []string{"", "-wal", "-shm"} {
		if err := os.Remove(path + suffix); err != nil && !os.IsNotExist(err) {
			log.Fatal(err)
		}
	}
	store := openStore(ctx, path, seriesCount, 2)
	series := buildSeries(seriesCount)
	start := time.Now().UnixMilli() - int64(samples+seedSamples)*10000
	if stage == "append" {
		seedHeads(ctx, store, series, batch, seedSamples, start)
	}
	var before, after runtime.MemStats
	runtime.GC()
	runtime.ReadMemStats(&before)
	w := watch(path)
	calls := &latencies{}
	begin := time.Now()
	first, last := 0, 1
	if stage == "append" {
		first, last = seedSamples, seedSamples+samples
	}
	written := writeSamples(ctx, store, series, batch, first, last, start, calls)
	elapsed := time.Since(begin)
	runtime.ReadMemStats(&after)
	w.finish()
	if err := store.Close(ctx); err != nil {
		log.Fatal(err)
	}
	p50, p95, p99, worst := calls.quantiles()
	report(stage,
		"series", seriesCount, "batch", batch, "seed_samples_per_series", seedSamples, "samples", written,
		"seconds", strconv.FormatFloat(elapsed.Seconds(), 'f', 2, 64),
		"samples_per_second", int64(float64(written)/elapsed.Seconds()),
		"call_p50_us", micros(p50), "call_p95_us", micros(p95), "call_p99_us", micros(p99), "call_max_us", micros(worst),
		"allocs_per_sample", strconv.FormatFloat(float64(after.Mallocs-before.Mallocs)/float64(written), 'f', 2, 64),
		"observed_wal_growth_bytes_per_sample", strconv.FormatFloat(float64(w.walGrowth)/float64(written), 'f', 2, 64),
		"wal_peak_bytes", w.walPeak,
		"peak_heap_mib", strconv.FormatFloat(float64(w.peakHeap)/(1<<20), 'f', 1, 64),
		"peak_go_sys_mib", strconv.FormatFloat(float64(w.peakGoSys)/(1<<20), 'f', 1, 64),
		"peak_os_rss_mib", rssMiB(w),
		"file_bytes_closed", fileBytes(path),
	)
}

func seedHeads(ctx context.Context, store *metrics.Store, series []metrics.Series, batch, samples int, start int64) {
	for first := 0; first < len(series); first += batch {
		last := min(first+batch, len(series))
		batches := make([]metrics.Batch, 0, last-first)
		for id := first; id < last; id++ {
			points := make([]metrics.Sample, samples)
			for round := range points {
				points[round] = metrics.Sample{At: start + int64(round)*10000, Value: float64((round*7 + id) % 97)}
			}
			batches = append(batches, metrics.Batch{Series: series[id], Samples: points})
		}
		if err := store.Ingest(ctx, batches); err != nil {
			log.Fatalf("seed series %d..%d: %v", first, last, err)
		}
	}
}

func writeSamples(ctx context.Context, store *metrics.Store, series []metrics.Series, batch, firstRound, lastRound int, start int64, calls *latencies) int {
	written := 0
	for round := firstRound; round < lastRound; round++ {
		for first := 0; first < len(series); first += batch {
			last := min(first+batch, len(series))
			batches := make([]metrics.Batch, 0, last-first)
			for id := first; id < last; id++ {
				batches = append(batches, metrics.Batch{
					Series:  series[id],
					Samples: []metrics.Sample{{At: start + int64(round)*10000, Value: float64((round*7 + id) % 97)}},
				})
			}
			at := time.Now()
			if err := store.Ingest(ctx, batches); err != nil {
				log.Fatalf("%s series %d..%d round %d: %v", "write", first, last, round, err)
			}
			if calls != nil {
				calls.add(time.Since(at))
			}
			written += len(batches)
		}
	}
	return written
}
