package main

import (
	"context"
	"log"
	"runtime"
	"strconv"
	"sync"
	"sync/atomic"
	"time"

	"github.com/tinyshed/tinystore/metrics"
)

func multiStore(ctx context.Context, dir string, seriesCount, seconds int, sharedBytes int64) {
	if seriesCount < 1 || seconds < 1 || sharedBytes < 0 {
		log.Fatal("invalid multi-store parameters")
	}
	var shared *metrics.WorkBudget
	if sharedBytes > 0 {
		var err error
		shared, err = metrics.NewWorkBudget(sharedBytes)
		if err != nil {
			log.Fatal(err)
		}
	}
	limits := metrics.Limits{Series: 3000, Blocks: 100000, PayloadBytes: 64 << 20, DecodedSamples: 2000000, OutputSamples: 1000000}
	var stores [2]*metrics.Store
	for index := range stores {
		path, cleanup, err := copiedReadFixture(dir)
		if err != nil {
			log.Fatal(err)
		}
		defer cleanup()
		stores[index] = openMetrics(ctx, path, metrics.Options{
			Retention:          365 * 24 * time.Hour,
			MaxReaders:         4,
			MaxConcurrentReads: 4,
			MaxSeries:          2 * seriesCount,
			MaxHeadSamples:     8192,
			MaxBatchSamples:    200000,
			MaxBatchBytes:      64 << 20,
			Limits:             limits,
			SharedBudget:       shared,
		})
		defer closeMetrics(ctx, stores[index])
	}
	probe, err := stores[0].Read(ctx, metrics.Range{Matchers: []metrics.Label{{Name: "__name__", Value: "metric_0"}, {Name: "host", Value: "host_0"}}, From: 0, To: 1 << 62})
	if err != nil || len(probe) != 1 {
		log.Fatalf("multi-store probe: %v", err)
	}
	points := probe[0].Samples
	from := points[0].At
	request := metrics.Range{Matchers: []metrics.Label{{Name: "region", Value: regions[0]}}, From: from, To: from + 3600000}
	runtime.GC()
	w := watch("")
	var queries, returned atomic.Int64
	latency := &latencies{}
	deadline := time.Now().Add(time.Duration(seconds) * time.Second)
	start := time.Now()
	var workers sync.WaitGroup
	for worker := range 8 {
		workers.Go(func() {
			store := stores[worker/4]
			for time.Now().Before(deadline) {
				at := time.Now()
				result, err := store.Read(ctx, request)
				if err != nil {
					log.Fatal(err)
				}
				latency.add(time.Since(at))
				queries.Add(1)
				for _, series := range result {
					returned.Add(int64(len(series.Samples)))
				}
			}
		})
	}
	workers.Wait()
	elapsed := time.Since(start)
	w.finish()
	p50, _, p99, _ := latency.quantiles()
	peak := int64(0)
	if shared != nil {
		_, peak = shared.Usage()
	}
	report("multi_store", "series_per_store", seriesCount, "workers", 8,
		"shared_bytes", sharedBytes, "shared_peak_reserved", peak,
		"queries", queries.Load(), "returned_samples", returned.Load(),
		"queries_per_second", strconv.FormatFloat(float64(queries.Load())/elapsed.Seconds(), 'f', 1, 64),
		"p50_us", micros(p50), "p99_us", micros(p99),
		"peak_go_sys_mib", strconv.FormatFloat(float64(w.peakGoSys)/(1<<20), 'f', 1, 64),
		"peak_os_rss_mib", rssMiB(w))
}
