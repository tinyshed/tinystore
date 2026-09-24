package main

import (
	"bufio"
	"context"
	"encoding/json"
	"log"
	"os"
	"path/filepath"
	"runtime"
	"strconv"
	"sync"
	"sync/atomic"
	"time"

	"github.com/tinyshed/tinystore/metrics"
)

func tsbsIngestRSS(ctx context.Context, dir, corpus string, concurrentMaintenance bool) {
	if corpus == "" {
		log.Fatal("TSBS RSS ingestion requires -corpus")
	}
	stage := "tsbs_ingest_rss"
	if concurrentMaintenance {
		stage = "tsbs_ingest_maint_rss"
	}
	path := filepath.Join(dir, stage, "metrics.db")
	if _, err := os.Stat(path); !os.IsNotExist(err) {
		log.Fatal("TSBS RSS database path must be new")
	}
	store := openMetrics(ctx, path, metrics.Options{
		Retention:       100 * 365 * 24 * time.Hour,
		MaxSeries:       4040,
		MaxHeadSamples:  8192,
		MaxBatchSamples: 8192,
		MaxBatchBytes:   64 << 20,
	})
	defer closeMetrics(ctx, store)
	file, err := os.Open(corpus)
	if err != nil {
		log.Fatal(err)
	}
	defer file.Close()
	scanner := bufio.NewScanner(file)
	scanner.Buffer(make([]byte, 1<<20), 1<<28)
	runtime.GC()
	baseline := processRSS()
	w := watch(path)
	start := time.Now()
	seriesCount, sampleCount := 0, 0
	var blocks, maintenanceCalls atomic.Int64
	finished := make(chan struct{})
	var maintainer sync.WaitGroup
	if concurrentMaintenance {
		maintainer.Go(func() {
			for {
				select {
				case <-finished:
					return
				default:
				}
				result, maintainErr := store.Maintain(ctx)
				if maintainErr != nil {
					log.Fatal(maintainErr)
				}
				blocks.Add(int64(result.SealedBlocks))
				maintenanceCalls.Add(1)
				if result.SealedBlocks == 0 {
					time.Sleep(10 * time.Millisecond)
				}
			}
		})
	}
	for scanner.Scan() {
		var row struct {
			Metric map[string]string `json:"metric"`
			Values []float64         `json:"values"`
			Times  []int64           `json:"timestamps"`
		}
		if err := json.Unmarshal(scanner.Bytes(), &row); err != nil {
			log.Fatal(err)
		}
		if len(row.Values) != len(row.Times) || len(row.Values) == 0 {
			log.Fatal("invalid normalized corpus row")
		}
		series := metrics.Series{Kind: metrics.Gauge}
		for name, value := range row.Metric {
			series.Labels = append(series.Labels, metrics.Label{Name: name, Value: value})
		}
		points := make([]metrics.Sample, len(row.Values))
		for index, value := range row.Values {
			points[index] = metrics.Sample{At: row.Times[index], Value: value}
		}
		if err := store.Ingest(ctx, []metrics.Batch{{Series: series, Samples: points}}); err != nil {
			log.Fatal(err)
		}
		if !concurrentMaintenance {
			result, maintainErr := store.Maintain(ctx)
			if maintainErr != nil {
				log.Fatal(maintainErr)
			}
			blocks.Add(int64(result.SealedBlocks))
			maintenanceCalls.Add(1)
		}
		seriesCount++
		sampleCount += len(points)
	}
	if err := scanner.Err(); err != nil {
		log.Fatal(err)
	}
	close(finished)
	maintainer.Wait()
	elapsed := time.Since(start)
	w.finish()
	report(stage, "series", seriesCount, "samples", sampleCount,
		"sealed_blocks", blocks.Load(), "maintenance_calls", maintenanceCalls.Load(),
		"elapsed_s", strconv.FormatFloat(elapsed.Seconds(), 'f', 3, 64),
		"samples_per_second", strconv.FormatFloat(float64(sampleCount)/elapsed.Seconds(), 'f', 1, 64),
		"rss_before_timing_mib", strconv.FormatFloat(float64(baseline)/(1<<20), 'f', 1, 64),
		"peak_os_rss_mib", rssMiB(w),
		"peak_heap_mib", strconv.FormatFloat(float64(w.peakHeap)/(1<<20), 'f', 1, 64))
}

func idleRSS(ctx context.Context, dir string, seriesCount, readers, seconds int) {
	path, cleanup, err := copiedReadFixture(dir)
	if err != nil {
		log.Fatal(err)
	}
	defer cleanup()
	store := openStore(ctx, path, seriesCount, readers)
	defer closeMetrics(ctx, store)
	runtime.GC()
	baseline := processRSS()
	w := watch(path)
	time.Sleep(time.Duration(seconds) * time.Second)
	w.finish()
	report("idle_rss", "series", seriesCount, "reader_pool", readers, "seconds", seconds,
		"rss_after_open_mib", strconv.FormatFloat(float64(baseline)/(1<<20), 'f', 1, 64),
		"peak_os_rss_mib", rssMiB(w),
		"peak_heap_mib", strconv.FormatFloat(float64(w.peakHeap)/(1<<20), 'f', 1, 64))
}

func maintenanceIngestRSS(ctx context.Context, dir string, seriesCount, seconds int) {
	if seriesCount < 1 || seriesCount > 10000 || seconds < 1 {
		log.Fatal("maintenance ingest RSS needs 1..10000 series and positive seconds")
	}
	path := filepath.Join(dir, "maintenance-ingest-rss", "metrics.db")
	for _, suffix := range []string{"", "-wal", "-shm"} {
		if err := os.Remove(path + suffix); err != nil && !os.IsNotExist(err) {
			log.Fatal(err)
		}
	}
	store := openStore(ctx, path, seriesCount, 2)
	defer closeMetrics(ctx, store)
	series := buildSeries(seriesCount)
	start := time.Now().UnixMilli() - 3600000
	for first := 0; first < seriesCount; first += 64 {
		batch := make([]metrics.Batch, 0, min(64, seriesCount-first))
		for id := first; id < min(first+64, seriesCount); id++ {
			points := make([]metrics.Sample, 241)
			for index := range points {
				points[index] = metrics.Sample{At: start + int64(index)*10000, Value: float64((id + index) % 97)}
			}
			batch = append(batch, metrics.Batch{Series: series[id], Samples: points})
		}
		if err := store.Ingest(ctx, batch); err != nil {
			log.Fatal(err)
		}
	}
	runtime.GC()
	baseline := processRSS()
	w := watch(path)
	deadline := time.Now().Add(time.Duration(seconds) * time.Second)
	var ingested, sealed, maintenanceCalls atomic.Int64
	var workers sync.WaitGroup
	workers.Go(func() {
		round := int64(241)
		for time.Now().Before(deadline) {
			for first := 0; first < seriesCount && time.Now().Before(deadline); first += 100 {
				batch := make([]metrics.Batch, 0, min(100, seriesCount-first))
				for id := first; id < min(first+100, seriesCount); id++ {
					batch = append(batch, metrics.Batch{Series: series[id], Samples: []metrics.Sample{{At: start + round*10000, Value: float64((id + int(round)) % 97)}}})
				}
				if err := store.Ingest(ctx, batch); err != nil {
					log.Fatal(err)
				}
				ingested.Add(int64(len(batch)))
			}
			round++
		}
	})
	workers.Go(func() {
		for time.Now().Before(deadline) {
			result, err := store.Maintain(ctx)
			if err != nil {
				log.Fatal(err)
			}
			maintenanceCalls.Add(1)
			sealed.Add(int64(result.SealedBlocks))
			if result.SealedBlocks == 0 {
				time.Sleep(10 * time.Millisecond)
			}
		}
	})
	workers.Wait()
	w.finish()
	report("maintenance_ingest_rss", "series", seriesCount, "seconds", seconds,
		"ingested_samples", ingested.Load(), "sealed_blocks", sealed.Load(),
		"maintenance_calls", maintenanceCalls.Load(),
		"rss_before_timing_mib", strconv.FormatFloat(float64(baseline)/(1<<20), 'f', 1, 64),
		"peak_os_rss_mib", rssMiB(w),
		"peak_heap_mib", strconv.FormatFloat(float64(w.peakHeap)/(1<<20), 'f', 1, 64))
}

func streamRegionHour(ctx context.Context, store *metrics.Store, seriesCount, readers, seconds int, first, last int64) {
	request := metrics.Range{Matchers: []metrics.Label{{Name: "region", Value: regions[0]}}, From: first + 10000, To: min(first+3600000+10000, last+1)}
	runtime.GC()
	baseline := processRSS()
	w := watch("")
	var queries, returned atomic.Int64
	latency := &latencies{}
	deadline := time.Now().Add(time.Duration(seconds) * time.Second)
	start := time.Now()
	var workers sync.WaitGroup
	for range readers {
		workers.Go(func() {
			for time.Now().Before(deadline) {
				at := time.Now()
				count := 0
				if err := store.Stream(ctx, request, func(result metrics.Result) error {
					count += len(result.Samples)
					return nil
				}); err != nil {
					log.Fatal(err)
				}
				latency.add(time.Since(at))
				queries.Add(1)
				returned.Add(int64(count))
			}
		})
	}
	workers.Wait()
	elapsed := time.Since(start)
	w.finish()
	p50, _, p99, _ := latency.quantiles()
	report("stream_region_hour", "series", seriesCount, "readers", readers, "seconds", seconds,
		"queries", queries.Load(), "samples_returned", returned.Load(),
		"queries_per_second", strconv.FormatFloat(float64(queries.Load())/elapsed.Seconds(), 'f', 1, 64),
		"p50_us", micros(p50), "p99_us", micros(p99),
		"rss_before_timing_mib", strconv.FormatFloat(float64(baseline)/(1<<20), 'f', 1, 64),
		"peak_os_rss_mib", rssMiB(w),
		"peak_heap_mib", strconv.FormatFloat(float64(w.peakHeap)/(1<<20), 'f', 1, 64))
}

func tsbsRSS(ctx context.Context, dir, shape string, workers, seconds int) {
	if workers < 1 || seconds < 1 || shape != "idle" && shape != "light" && shape != "wide" && shape != "wide_materialized" {
		log.Fatal("TSBS RSS needs idle/light/wide/wide_materialized shape and positive workers/seconds")
	}
	path, cleanup, err := copiedReadFixture(dir)
	if err != nil {
		log.Fatal(err)
	}
	defer cleanup()
	store := openMetrics(ctx, path, metrics.Options{
		Retention:          100 * 365 * 24 * time.Hour,
		MaxReaders:         workers,
		MaxConcurrentReads: workers,
		MaxSeries:          4040,
		MaxHeadSamples:     8192,
		Limits: metrics.Limits{
			Series: 3000, Blocks: 1 << 20, PayloadBytes: 64 << 20,
			DecodedSamples: 4 << 20, OutputSamples: 4 << 20,
		},
	})
	defer closeMetrics(ctx, store)
	const first = int64(1767225600000)
	const last = int64(1767250790000)
	request := metrics.Range{}
	switch shape {
	case "light":
		request = metrics.Range{Matchers: []metrics.Label{{Name: "__name__", Value: "cpu_usage_guest"}, {Name: "hostname", Value: "host_0"}}, From: first, To: first + 3600000}
	case "wide", "wide_materialized":
		request = metrics.Range{Matchers: []metrics.Label{{Name: "region", Value: "eu-west-1"}}, From: first, To: last + 1}
	}
	runtime.GC()
	baseline := processRSS()
	w := watch(path)
	var queries, returned atomic.Int64
	latency := &latencies{}
	deadline := time.Now().Add(time.Duration(seconds) * time.Second)
	start := time.Now()
	var group sync.WaitGroup
	if shape == "idle" {
		time.Sleep(time.Duration(seconds) * time.Second)
	} else {
		for range workers {
			group.Go(func() {
				for time.Now().Before(deadline) {
					at := time.Now()
					count := 0
					if shape == "light" || shape == "wide_materialized" {
						result, readErr := store.Read(ctx, request)
						if readErr != nil {
							log.Fatal(readErr)
						}
						for _, series := range result {
							count += len(series.Samples)
						}
					} else {
						if readErr := store.Stream(ctx, request, func(result metrics.Result) error {
							count += len(result.Samples)
							return nil
						}); readErr != nil {
							log.Fatal(readErr)
						}
					}
					queries.Add(1)
					returned.Add(int64(count))
					latency.add(time.Since(at))
				}
			})
		}
		group.Wait()
	}
	elapsed := time.Since(start)
	w.finish()
	p50, _, p99, _ := latency.quantiles()
	report("tsbs_rss", "shape", shape, "workers", workers, "seconds", seconds,
		"queries", queries.Load(), "returned_samples", returned.Load(),
		"elapsed_s", strconv.FormatFloat(elapsed.Seconds(), 'f', 3, 64),
		"p50_us", micros(p50), "p99_us", micros(p99),
		"rss_before_timing_mib", strconv.FormatFloat(float64(baseline)/(1<<20), 'f', 1, 64),
		"peak_os_rss_mib", rssMiB(w),
		"peak_heap_mib", strconv.FormatFloat(float64(w.peakHeap)/(1<<20), 'f', 1, 64))
}
