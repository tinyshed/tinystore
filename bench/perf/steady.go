package main

import (
	"context"
	"database/sql"
	"fmt"
	"log"
	"math"
	"path/filepath"
	"time"

	"github.com/tinyshed/tinystore/metrics"
)

func steadySample(series, index int) metrics.Sample {
	value := float64(series)
	switch series % 4 {
	case 1:
		value = float64(index / 80)
	case 2:
		value = float64(index*3 + index%7)
	case 3:
		bits := uint64(index+series) * 0x9e3779b97f4a7c15
		bits = (bits ^ bits>>30) * 0xbf58476d1ce4e5b9
		bits = (bits ^ bits>>27) * 0x94d049bb133111eb
		value = math.Float64frombits(0x3ff0000000000000 | (bits^bits>>31)&0x000fffffffffffff)
	}
	return metrics.Sample{At: 1789500000000 + int64(index)*10000, Value: value}
}

func steady(ctx context.Context, dir, label string, seriesCount, samples int) {
	path := filepath.Join(dir, "read", "metrics.db")
	store := openStore(ctx, path, seriesCount, 2)
	all := buildSeries(seriesCount)
	maintenance := &latencies{}
	w := watch(path)
	begin := time.Now()
	for offset := 0; offset < samples; {
		end := min(offset+240, samples)
		if offset == 0 {
			end = min(241, samples)
		}
		for first := 0; first < seriesCount; first += 128 {
			var batches []metrics.Batch
			for id := first; id < min(first+128, seriesCount); id++ {
				points := make([]metrics.Sample, end-offset)
				for i := range points {
					points[i] = steadySample(id, offset+i)
				}
				batches = append(batches, metrics.Batch{Series: all[id], Samples: points})
			}
			if err := store.Ingest(ctx, batches); err != nil {
				log.Fatal(err)
			}
		}
		for {
			at := time.Now()
			work, err := store.Maintain(ctx)
			maintenance.add(time.Since(at))
			if err != nil {
				log.Fatal(err)
			}
			if work.SealedBlocks == 0 {
				break
			}
		}
		offset = end
	}
	elapsed := time.Since(begin)
	w.finish()
	if err := closeMetrics(ctx, store); err != nil {
		log.Fatal(err)
	}
	p50, _, p99, _ := maintenance.quantiles()
	report("steady", "label", label, "series", seriesCount, "samples", samples*seriesCount,
		"samples_per_second", int(float64(samples*seriesCount)/elapsed.Seconds()),
		"maintain_p50_us", micros(p50), "maintain_p99_us", micros(p99),
		"file_bytes", fileBytes(path), "bytes_per_sample", fmt.Sprintf("%.6f", float64(fileBytes(path))/float64(samples*seriesCount)),
		"wal_peak_bytes", w.walPeak)
	store = openStore(ctx, path, seriesCount, 2)
	for id, series := range all {
		result, err := store.Read(ctx, metrics.Range{Matchers: series.Labels, From: steadySample(id, 0).At, To: steadySample(id, samples-1).At + 1})
		if err != nil || len(result) != 1 || len(result[0].Samples) != samples {
			log.Fatalf("steady readback series %d: %v", id, err)
		}
		for index, point := range result[0].Samples {
			want := steadySample(id, index)
			if point.At != want.At || math.Float64bits(point.Value) != math.Float64bits(want.Value) {
				log.Fatalf("steady readback series %d sample %d changed", id, index)
			}
		}
	}
	if err := closeMetrics(ctx, store); err != nil {
		log.Fatal(err)
	}
	report("steady_readback", "label", label, "verified_samples", samples*seriesCount)
	db, err := sql.Open("sqlite", path)
	if err != nil {
		log.Fatal(err)
	}
	defer db.Close()
	rows, err := db.QueryContext(ctx, `select name,sum(pgsize) from dbstat group by name order by name`)
	if err != nil {
		log.Fatal(err)
	}
	defer rows.Close()
	for rows.Next() {
		var name string
		var size int64
		if err = rows.Scan(&name, &size); err != nil {
			log.Fatal(err)
		}
		report("object", "label", label, "name", name, "bytes", size)
	}
	if err = rows.Err(); err != nil {
		log.Fatal(err)
	}
	var groups, clocks, free int
	if err = db.QueryRowContext(ctx, `select (select count(*) from groups),(select count(*) from clocks),(select freelist_count from pragma_freelist_count)`).Scan(&groups, &clocks, &free); err != nil {
		log.Fatal(err)
	}
	report("layout", "label", label, "groups", groups, "clocks", clocks, "free_pages", free)
}
