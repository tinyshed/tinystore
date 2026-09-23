// Command repro compares saved TinyStore revisions on the same logical workload.
package main

import (
	"context"
	"flag"
	"fmt"
	"log"
	"math"
	"os"
	"path/filepath"
	"runtime"
	"slices"
	"strconv"
	"time"

	"github.com/tinyshed/tinystore/metrics"
)

func series(id int) metrics.Series {
	return metrics.Series{Kind: metrics.Gauge, Labels: []metrics.Label{
		{Name: "__name__", Value: "repro"},
		{Name: "host", Value: "host_" + strconv.Itoa(id)},
		{Name: "region", Value: "r" + strconv.Itoa(id%4)},
	}}
}

func sample(id, index int) metrics.Sample {
	return metrics.Sample{At: 1789500000000 + int64(index)*10000, Value: float64((id*17+index*13)%97) / 3}
}

func store(ctx context.Context, path string, seriesCount int) *metrics.Store {
	s, err := metrics.Open(ctx, path, metrics.Options{
		Retention:       365 * 24 * time.Hour,
		MaxSeries:       seriesCount * 2,
		MaxHeadSamples:  8192,
		MaxBatchSamples: 20000,
		MaxBatchBytes:   64 << 20,
		MaxReaders:      2,
		Limits: metrics.Limits{
			Series: seriesCount * 2, Blocks: 1 << 18, PayloadBytes: 1 << 28,
			DecodedSamples: 1 << 22, OutputSamples: 1 << 22,
		},
	})
	if err != nil {
		log.Fatal(err)
	}
	return s
}

func write(ctx context.Context, s *metrics.Store, firstRound, lastRound, seriesCount, batchSize int, timings *[]time.Duration) int {
	written := 0
	for round := firstRound; round < lastRound; round++ {
		for first := 0; first < seriesCount; first += batchSize {
			batch := make([]metrics.Batch, 0, min(batchSize, seriesCount-first))
			for id := first; id < min(first+batchSize, seriesCount); id++ {
				batch = append(batch, metrics.Batch{Series: series(id), Samples: []metrics.Sample{sample(id, round)}})
			}
			start := time.Now()
			if err := s.Ingest(ctx, batch); err != nil {
				log.Fatal(err)
			}
			if timings != nil {
				*timings = append(*timings, time.Since(start))
			}
			written += len(batch)
		}
	}
	return written
}

func quantile(values []time.Duration, percent int) time.Duration {
	if len(values) == 0 {
		return 0
	}
	slices.Sort(values)
	return values[(len(values)*percent+99)/100-1]
}

func main() {
	dir := flag.String("dir", ".", "new database directory")
	stage := flag.String("stage", "append", "register, append, seal or read")
	seriesCount := flag.Int("series", 1000, "series count")
	rounds := flag.Int("rounds", 100, "timed append rounds")
	batchSize := flag.Int("batch", 100, "series per Ingest call")
	readSeconds := flag.Int("seconds", 3, "read stage duration")
	flag.Parse()
	if *seriesCount < 1 || *batchSize < 1 || *rounds < 1 || *readSeconds < 1 {
		log.Fatal("positive series, batch, rounds and seconds required")
	}
	if err := os.MkdirAll(*dir, 0o755); err != nil {
		log.Fatal(err)
	}
	path := filepath.Join(*dir, *stage+".db")
	for _, suffix := range []string{"", "-wal", "-shm"} {
		if err := os.Remove(path + suffix); err != nil && !os.IsNotExist(err) {
			log.Fatal(err)
		}
	}
	ctx := context.Background()
	s := store(ctx, path, *seriesCount)
	defer s.Close(ctx)
	var timings []time.Duration
	var before, after runtime.MemStats
	switch *stage {
	case "register":
		runtime.GC()
		runtime.ReadMemStats(&before)
		start := time.Now()
		n := write(ctx, s, 0, 1, *seriesCount, *batchSize, &timings)
		elapsed := time.Since(start)
		runtime.ReadMemStats(&after)
		report(*stage, n, elapsed, timings, before, after)
	case "append":
		write(ctx, s, 0, 1, *seriesCount, *batchSize, nil)
		runtime.GC()
		runtime.ReadMemStats(&before)
		start := time.Now()
		n := write(ctx, s, 1, *rounds+1, *seriesCount, *batchSize, &timings)
		elapsed := time.Since(start)
		runtime.ReadMemStats(&after)
		report(*stage, n, elapsed, timings, before, after)
	case "seal":
		for first := 0; first < *seriesCount; first += 32 {
			var batch []metrics.Batch
			for id := first; id < min(first+32, *seriesCount); id++ {
				points := make([]metrics.Sample, 241)
				for i := range points {
					points[i] = sample(id, i)
				}
				batch = append(batch, metrics.Batch{Series: series(id), Samples: points})
			}
			if err := s.Ingest(ctx, batch); err != nil {
				log.Fatal(err)
			}
		}
		start := time.Now()
		blocks := 0
		for {
			at := time.Now()
			result, err := s.Maintain(ctx)
			if err != nil {
				log.Fatal(err)
			}
			timings = append(timings, time.Since(at))
			blocks += result.SealedBlocks
			if result.SealedBlocks == 0 {
				break
			}
		}
		elapsed := time.Since(start)
		fmt.Printf("stage=seal series=%d blocks=%d elapsed_s=%.6f blocks_per_second=%.1f calls=%d call_p50_us=%.1f call_p99_us=%.1f\n", *seriesCount, blocks, elapsed.Seconds(), float64(blocks)/elapsed.Seconds(), len(timings), float64(quantile(timings, 50).Microseconds()), float64(quantile(timings, 99).Microseconds()))
	case "read":
		for first := 0; first < *seriesCount; first += 32 {
			var batch []metrics.Batch
			for id := first; id < min(first+32, *seriesCount); id++ {
				points := make([]metrics.Sample, 500)
				for i := range points {
					points[i] = sample(id, i)
				}
				batch = append(batch, metrics.Batch{Series: series(id), Samples: points})
			}
			if err := s.Ingest(ctx, batch); err != nil {
				log.Fatal(err)
			}
		}
		for {
			result, err := s.Maintain(ctx)
			if err != nil {
				log.Fatal(err)
			}
			if result.SealedBlocks == 0 {
				break
			}
		}
		end := time.Now().Add(time.Duration(*readSeconds) * time.Second)
		queries, returned := 0, 0
		start := time.Now()
		for time.Now().Before(end) {
			id := (queries * 7919) % *seriesCount
			from := sample(id, (queries*17)%140).At
			at := time.Now()
			result, err := s.Read(ctx, metrics.Range{Matchers: series(id).Labels, From: from, To: from + 3600000})
			if err != nil {
				log.Fatal(err)
			}
			timings = append(timings, time.Since(at))
			for _, got := range result {
				returned += len(got.Samples)
			}
			queries++
		}
		elapsed := time.Since(start)
		fmt.Printf("stage=read series=%d queries=%d returned=%d elapsed_s=%.6f qps=%.1f samples_per_second=%.1f p50_us=%.1f p99_us=%.1f\n", *seriesCount, queries, returned, elapsed.Seconds(), float64(queries)/elapsed.Seconds(), float64(returned)/elapsed.Seconds(), float64(quantile(timings, 50).Microseconds()), float64(quantile(timings, 99).Microseconds()))
	default:
		log.Fatal("unknown stage")
	}
	if err := s.Close(ctx); err != nil {
		log.Fatal(err)
	}
	info, err := os.Stat(path)
	if err != nil {
		log.Fatal(err)
	}
	fmt.Printf("stage=%s file_bytes=%d\n", *stage, info.Size())
}

func report(stage string, samples int, elapsed time.Duration, timings []time.Duration, before, after runtime.MemStats) {
	allocs := float64(after.Mallocs-before.Mallocs) / float64(samples)
	bytes := float64(after.TotalAlloc-before.TotalAlloc) / float64(samples)
	if math.IsNaN(allocs) || math.IsNaN(bytes) {
		log.Fatal("invalid allocation count")
	}
	fmt.Printf("stage=%s samples=%d calls=%d elapsed_s=%.6f samples_per_second=%.1f call_p50_us=%.1f call_p99_us=%.1f allocs_per_sample=%.2f bytes_alloc_per_sample=%.1f\n", stage, samples, len(timings), elapsed.Seconds(), float64(samples)/elapsed.Seconds(), float64(quantile(timings, 50).Microseconds()), float64(quantile(timings, 99).Microseconds()), allocs, bytes)
}
