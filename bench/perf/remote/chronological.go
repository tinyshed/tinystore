package main

import (
	"bufio"
	"bytes"
	"encoding/json"
	"fmt"
	"io"
	"log"
	"net/http"
	"os"
	"slices"
	"strings"
	"time"

	"github.com/klauspost/compress/s2"
)

func chronologicalRequests(path string, anchor, divisor int64) ([]request, int) {
	file, err := os.Open(path)
	if err != nil {
		log.Fatal(err)
	}
	defer file.Close()
	scanner := bufio.NewScanner(file)
	scanner.Buffer(make([]byte, 1<<20), 1<<28)
	var rows []corpusRow
	for scanner.Scan() {
		var row corpusRow
		if err := json.Unmarshal(scanner.Bytes(), &row); err != nil {
			log.Fatal(err)
		}
		if len(row.Values) != len(row.Times) || len(row.Values) == 0 {
			log.Fatal("corpus lengths")
		}
		rows = append(rows, row)
	}
	if err := scanner.Err(); err != nil {
		log.Fatal(err)
	}
	if len(rows) == 0 {
		log.Fatal("empty chronological corpus")
	}
	minTime := rows[0].Times[0]
	clock := slices.Clone(rows[0].Times)
	for _, row := range rows {
		if len(row.Values) != len(rows[0].Values) {
			log.Fatal("chronological replay requires shared clocks")
		}
		minTime = min(minTime, row.Times[0])
	}
	for id := range rows {
		for index, timestamp := range rows[id].Times {
			if timestamp != clock[index] {
				log.Fatal("chronological replay requires identical clocks")
			}
			rows[id].Times[index] = anchor + (timestamp-minTime)/divisor
		}
	}
	const roundsPerRequest = 10
	const seriesPerRequest = 100
	var requests []request
	total := 0
	for firstRound := 0; firstRound < len(rows[0].Values); firstRound += roundsPerRequest {
		lastRound := min(firstRound+roundsPerRequest, len(rows[0].Values))
		for firstSeries := 0; firstSeries < len(rows); firstSeries += seriesPerRequest {
			lastSeries := min(firstSeries+seriesPerRequest, len(rows))
			var raw []byte
			for id := firstSeries; id < lastSeries; id++ {
				raw = messageField(raw, 1, encodeSeries(rows[id], firstRound, lastRound))
			}
			count := (lastSeries - firstSeries) * (lastRound - firstRound)
			requests = append(requests, request{body: s2.EncodeSnappy(nil, raw), samples: count})
			total += count
		}
	}
	return requests, total
}

func replayChronological(endpoint string, requests []request, total int, anchor, divisor int64) {
	client := &http.Client{Timeout: 30 * time.Second}
	var timings []time.Duration
	start := time.Now()
	for _, item := range requests {
		req, err := http.NewRequest(http.MethodPost, strings.TrimRight(endpoint, "/")+"/api/v1/write", bytes.NewReader(item.body))
		if err != nil {
			log.Fatal(err)
		}
		req.Header.Set("Content-Type", "application/x-protobuf")
		req.Header.Set("Content-Encoding", "snappy")
		req.Header.Set("X-Prometheus-Remote-Write-Version", "0.1.0")
		at := time.Now()
		response, err := client.Do(req)
		if err != nil {
			log.Fatal(err)
		}
		body, err := io.ReadAll(io.LimitReader(response.Body, 4096))
		response.Body.Close()
		if err != nil || response.StatusCode/100 != 2 {
			log.Fatalf("chronological write response %s: %s: %v", response.Status, body, err)
		}
		timings = append(timings, time.Since(at))
	}
	elapsed := time.Since(start)
	fmt.Printf("stage=chronological_write samples=%d requests=%d time_anchor_ms=%d time_divisor=%d elapsed_s=%.6f samples_per_second=%.1f p50_us=%.1f p99_us=%.1f\n", total, len(requests), anchor, divisor, elapsed.Seconds(), float64(total)/elapsed.Seconds(), float64(quantile(timings, 50).Microseconds()), float64(quantile(timings, 99).Microseconds()))
}
