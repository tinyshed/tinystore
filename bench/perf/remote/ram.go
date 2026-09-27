package main

import (
	"fmt"
	"io"
	"log"
	"net/http"
	"net/url"
	"slices"
	"strings"
	"sync"
	"time"
)

func ramQuery(endpoint, shape string, workers, seconds int) {
	if endpoint == "" || workers < 1 || seconds < 1 || shape != "light" && shape != "wide" {
		log.Fatal("RAM query needs endpoint, light/wide shape, positive workers and seconds")
	}
	values := url.Values{}
	path := "/api/v1/query_range"
	if shape == "light" {
		values.Set("query", `cpu_usage_guest{hostname="host_0"}`)
		values.Set("start", "1790119200")
		values.Set("end", "1790119424.375")
		values.Set("step", "0.625")
	} else {
		values.Set("query", `{region="eu-west-1"}`)
		values.Set("start", "1790119200")
		values.Set("end", "1790120774.375")
		values.Set("step", "0.625")
	}
	target := strings.TrimRight(endpoint, "/") + path + "?" + values.Encode()
	client := &http.Client{Timeout: 2 * time.Minute}
	var mu sync.Mutex
	var calls, bytes int64
	var durations []time.Duration
	deadline := time.Now().Add(time.Duration(seconds) * time.Second)
	start := time.Now()
	var group sync.WaitGroup
	for range workers {
		group.Go(func() {
			for time.Now().Before(deadline) {
				at := time.Now()
				response, err := client.Get(target)
				if err != nil {
					log.Fatal(err)
				}
				n, err := io.Copy(io.Discard, response.Body)
				response.Body.Close()
				if err != nil || response.StatusCode/100 != 2 || n == 0 {
					log.Fatalf("RAM query %s: bytes=%d error=%v", response.Status, n, err)
				}
				mu.Lock()
				calls++
				bytes += n
				durations = append(durations, time.Since(at))
				mu.Unlock()
			}
		})
	}
	group.Wait()
	elapsed := time.Since(start)
	slices.Sort(durations)
	p50, p99 := durations[(len(durations)*50+99)/100-1], durations[(len(durations)*99+99)/100-1]
	fmt.Printf("stage=ram_query shape=%s workers=%d calls=%d response_bytes=%d elapsed_s=%.3f qps=%.1f p50_ms=%.1f p99_ms=%.1f\n", shape, workers, calls, bytes, elapsed.Seconds(), float64(calls)/elapsed.Seconds(), float64(p50.Milliseconds()), float64(p99.Milliseconds()))
}
