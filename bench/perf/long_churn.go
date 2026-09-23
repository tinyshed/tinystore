package main

import (
	"bufio"
	"context"
	"database/sql"
	"encoding/json"
	"log"
	"os"
	"path/filepath"
	"strconv"
	"time"

	_ "modernc.org/sqlite"

	"github.com/tinyshed/tinystore/metrics"
)

type churnIdentity struct {
	series metrics.Series
	value  float64
}

func corpusChurnIdentities(path string, count int) []churnIdentity {
	file, err := os.Open(path)
	if err != nil {
		log.Fatal(err)
	}
	scanner := bufio.NewScanner(file)
	scanner.Buffer(make([]byte, 1<<20), 1<<28)
	total := 0
	for scanner.Scan() {
		total++
	}
	if err := scanner.Err(); err != nil {
		log.Fatal(err)
	}
	if err := file.Close(); err != nil {
		log.Fatal(err)
	}
	if count > total {
		log.Fatal("corpus has fewer series than the requested churn cohort")
	}
	file, err = os.Open(path)
	if err != nil {
		log.Fatal(err)
	}
	defer file.Close()
	scanner = bufio.NewScanner(file)
	scanner.Buffer(make([]byte, 1<<20), 1<<28)
	identities := make([]churnIdentity, 0, count)
	for index := 0; scanner.Scan(); index++ {
		if (index+1)*count/total == index*count/total {
			continue
		}
		var row struct {
			Metric map[string]string `json:"metric"`
			Values []float64         `json:"values"`
		}
		if err := json.Unmarshal(scanner.Bytes(), &row); err != nil || len(row.Values) == 0 {
			log.Fatalf("decode churn corpus: %v", err)
		}
		identity := churnIdentity{series: metrics.Series{Kind: metrics.Gauge}, value: row.Values[0]}
		for name, value := range row.Metric {
			identity.series.Labels = append(identity.series.Labels, metrics.Label{Name: name, Value: value})
		}
		identities = append(identities, identity)
	}
	if err := scanner.Err(); err != nil {
		log.Fatal(err)
	}
	if len(identities) != count {
		log.Fatal("corpus sampling changed cohort size")
	}
	return identities
}

func longChurn(ctx context.Context, dir string, seriesCount, epochs int, corpus string) {
	if seriesCount < 1 || seriesCount > 10000 || epochs < 2 {
		log.Fatal("long churn requires 1..10000 series and at least two epochs")
	}
	path := filepath.Join(dir, "long-churn.db")
	for _, suffix := range []string{"", "-wal", "-shm"} {
		if err := os.Remove(path + suffix); err != nil && !os.IsNotExist(err) {
			log.Fatal(err)
		}
	}
	store := churnStore(ctx, path, seriesCount)
	var realIdentities []churnIdentity
	if corpus != "" {
		realIdentities = corpusChurnIdentities(corpus, seriesCount)
	}
	ingestTimes, expireTimes := &latencies{}, &latencies{}
	var ingestDurations, expireDurations []time.Duration
	maxOpenBytes := int64(0)
	for epoch := range epochs {
		series := buildSeries((epoch + 1) * seriesCount)[epoch*seriesCount:]
		stamp := time.Now().UnixMilli()
		batches := make([]metrics.Batch, len(series))
		for id := range batches {
			value := float64(epoch*seriesCount + id)
			if corpus != "" {
				original := realIdentities[id]
				series[id] = metrics.Series{Kind: original.series.Kind, Labels: append([]metrics.Label(nil), original.series.Labels...)}
				value = original.value
				foundHost := false
				for index := range series[id].Labels {
					if series[id].Labels[index].Name == "host" {
						series[id].Labels[index].Value += "-epoch-" + strconv.Itoa(epoch)
						foundHost = true
						break
					}
				}
				if !foundHost {
					series[id].Labels = append(series[id].Labels, metrics.Label{Name: "churn_epoch", Value: strconv.Itoa(epoch)})
				}
			}
			batches[id] = metrics.Batch{Series: series[id], Samples: []metrics.Sample{{At: stamp, Value: value}}}
		}
		start := time.Now()
		if err := store.Ingest(ctx, batches); err != nil {
			log.Fatalf("epoch %d ingest: %v", epoch, err)
		}
		ingestElapsed := time.Since(start)
		ingestTimes.add(ingestElapsed)
		ingestDurations = append(ingestDurations, ingestElapsed)
		maxOpenBytes = max(maxOpenBytes, fileBytes(path))
		time.Sleep(1200 * time.Millisecond)
		start = time.Now()
		reclaimed := 0
		for pass := 0; pass < (seriesCount+63)/64+4 && reclaimed < seriesCount; pass++ {
			result, err := store.Maintain(ctx)
			if err != nil {
				log.Fatalf("epoch %d retention: %v", epoch, err)
			}
			reclaimed += result.ReclaimedSeries
		}
		if reclaimed != seriesCount {
			log.Fatalf("epoch %d reclaimed %d of %d series", epoch, reclaimed, seriesCount)
		}
		expireElapsed := time.Since(start)
		expireTimes.add(expireElapsed)
		expireDurations = append(expireDurations, expireElapsed)
		maxOpenBytes = max(maxOpenBytes, fileBytes(path))
	}
	if err := store.Close(ctx); err != nil {
		log.Fatal(err)
	}
	i50, _, i99, _ := ingestTimes.quantiles()
	e50, _, e99, _ := expireTimes.quantiles()
	report("long_churn", "corpus", corpus != "", "epochs", epochs, "series_per_epoch", seriesCount,
		"registered_and_reclaimed", epochs*seriesCount, "max_file_bytes_open", maxOpenBytes,
		"file_bytes_closed", fileBytes(path),
		"ingest_p50_us", micros(i50), "ingest_p99_us", micros(i99),
		"reclaim_p50_us", micros(e50), "reclaim_p99_us", micros(e99),
		"first5_ingest_avg_us", micros(meanDuration(ingestDurations[:min(5, epochs)])),
		"last5_ingest_avg_us", micros(meanDuration(ingestDurations[max(0, epochs-5):])),
		"first5_reclaim_avg_us", micros(meanDuration(expireDurations[:min(5, epochs)])),
		"last5_reclaim_avg_us", micros(meanDuration(expireDurations[max(0, epochs-5):])))
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
		var bytes int64
		if err := rows.Scan(&name, &bytes); err != nil {
			log.Fatal(err)
		}
		report("long_churn_object", "name", name, "bytes", bytes)
	}
	if err := rows.Err(); err != nil {
		log.Fatal(err)
	}
	var free, pages int64
	if err := db.QueryRowContext(ctx, `select (select freelist_count from pragma_freelist_count),(select page_count from pragma_page_count)`).Scan(&free, &pages); err != nil {
		log.Fatal(err)
	}
	report("long_churn_pages", "free_pages", free, "total_pages", pages)
}

func meanDuration(values []time.Duration) time.Duration {
	var total time.Duration
	for _, value := range values {
		total += value
	}
	return total / time.Duration(len(values))
}
