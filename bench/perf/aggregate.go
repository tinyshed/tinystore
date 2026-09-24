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

func aggregate(ctx context.Context, dir, shape string, seriesCount, readers, seconds int) {
	if shape != "hour" && shape != "region" || seriesCount < 1 || readers < 1 || seconds < 1 {
		log.Fatal("aggregate stage needs hour/region and positive capacity")
	}
	path, cleanup, err := copiedReadFixture(dir)
	if err != nil {
		log.Fatal(err)
	}
	defer cleanup()
	store := openStore(ctx, path, seriesCount, readers)
	defer closeMetrics(ctx, store)
	probe, err := store.Read(ctx, metrics.Range{Matchers: []metrics.Label{{Name: "__name__", Value: "metric_0"}, {Name: "host", Value: "host_0"}}, From: 0, To: 1 << 62})
	if err != nil || len(probe) != 1 {
		log.Fatalf("aggregate probe: %v", err)
	}
	from := probe[0].Samples[0].At
	matchers := []metrics.Label{{Name: "__name__", Value: "metric_0"}, {Name: "host", Value: "host_0"}}
	if shape == "region" {
		matchers = []metrics.Label{{Name: "region", Value: regions[0]}}
	}
	request := metrics.AggregateRequest{Range: metrics.Range{Matchers: matchers, From: from, To: from + 3600000}, Width: time.Hour, Op: metrics.AggregateSum}
	runtime.GC()
	w := watch("")
	deadline := time.Now().Add(time.Duration(seconds) * time.Second)
	start := time.Now()
	var queries, outputBuckets, inputSamples atomic.Int64
	latency := &latencies{}
	var group sync.WaitGroup
	for range readers {
		group.Go(func() {
			for time.Now().Before(deadline) {
				at := time.Now()
				result, err := store.Aggregate(ctx, request)
				if err != nil {
					log.Fatal(err)
				}
				latency.add(time.Since(at))
				queries.Add(1)
				for _, series := range result {
					outputBuckets.Add(int64(len(series.Buckets)))
					for _, bucket := range series.Buckets {
						inputSamples.Add(int64(bucket.Count))
					}
				}
			}
		})
	}
	group.Wait()
	elapsed := time.Since(start)
	w.finish()
	p50, _, p99, _ := latency.quantiles()
	report("aggregate", "shape", shape, "series", seriesCount, "readers", readers,
		"queries", queries.Load(), "buckets", outputBuckets.Load(), "input_samples", inputSamples.Load(),
		"queries_per_second", strconv.FormatFloat(float64(queries.Load())/elapsed.Seconds(), 'f', 1, 64),
		"p50_us", micros(p50), "p99_us", micros(p99),
		"peak_os_rss_mib", rssMiB(w))
}
