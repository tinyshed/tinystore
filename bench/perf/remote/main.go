// Command remote replays one normalized corpus through the same remote-write requests.
package main

import (
	"bufio"
	"bytes"
	"encoding/binary"
	"encoding/json"
	"flag"
	"fmt"
	"io"
	"log"
	"math"
	"net/http"
	"net/url"
	"os"
	"slices"
	"sort"
	"strconv"
	"strings"
	"time"

	"github.com/klauspost/compress/s2"
)

type corpusRow struct {
	Metric map[string]string `json:"metric"`
	Values []float64         `json:"values"`
	Times  []int64           `json:"timestamps"`
}

type request struct {
	body    []byte
	samples int
}

func uvarint(dst []byte, value uint64) []byte {
	for value >= 128 {
		dst = append(dst, byte(value)|128)
		value >>= 7
	}
	return append(dst, byte(value))
}

func stringField(dst []byte, field int, value string) []byte {
	dst = uvarint(dst, uint64(field<<3|2))
	dst = uvarint(dst, uint64(len(value)))
	return append(dst, value...)
}

func messageField(dst []byte, field int, value []byte) []byte {
	dst = uvarint(dst, uint64(field<<3|2))
	dst = uvarint(dst, uint64(len(value)))
	return append(dst, value...)
}

func sampleMessage(value float64, at int64) []byte {
	var message [20]byte
	message[0] = 9
	binary.LittleEndian.PutUint64(message[1:9], math.Float64bits(value))
	message[9] = 16
	return uvarint(message[:10], uint64(at))
}

func encodeSeries(row corpusRow, first, last int) []byte {
	names := make([]string, 0, len(row.Metric))
	for name := range row.Metric {
		names = append(names, name)
	}
	sort.Strings(names)
	var series []byte
	for _, name := range names {
		var label []byte
		label = stringField(label, 1, name)
		label = stringField(label, 2, row.Metric[name])
		series = messageField(series, 1, label)
	}
	for index := first; index < last; index++ {
		series = messageField(series, 2, sampleMessage(row.Values[index], row.Times[index]))
	}
	return series
}

func encode(row corpusRow, first, last int) []byte {
	return s2.EncodeSnappy(nil, messageField(nil, 1, encodeSeries(row, first, last)))
}

func minimumTimestamp(path string) int64 {
	file, err := os.Open(path)
	if err != nil {
		log.Fatal(err)
	}
	defer file.Close()
	scanner := bufio.NewScanner(file)
	scanner.Buffer(make([]byte, 1<<20), 1<<28)
	minTime := int64(math.MaxInt64)
	for scanner.Scan() {
		var row struct {
			Times []int64 `json:"timestamps"`
		}
		if err := json.Unmarshal(scanner.Bytes(), &row); err != nil {
			log.Fatal(err)
		}
		for _, at := range row.Times {
			minTime = min(minTime, at)
		}
	}
	if err := scanner.Err(); err != nil {
		log.Fatal(err)
	}
	return minTime
}

func readRequests(path string, batch, maxSamples int, anchor int64, divisor int64) ([]request, []corpusRow, int) {
	minTime := int64(0)
	if anchor != 0 {
		minTime = minimumTimestamp(path)
	}
	file, err := os.Open(path)
	if err != nil {
		log.Fatal(err)
	}
	defer file.Close()
	scanner := bufio.NewScanner(file)
	scanner.Buffer(make([]byte, 1<<20), 1<<28)
	var requests []request
	var probes []corpusRow
	total := 0
	for scanner.Scan() {
		var row corpusRow
		if err := json.Unmarshal(scanner.Bytes(), &row); err != nil {
			log.Fatal(err)
		}
		if len(row.Values) != len(row.Times) || len(row.Values) == 0 {
			log.Fatal("corpus row lengths")
		}
		if anchor != 0 {
			for index := range row.Times {
				row.Times[index] = anchor + (row.Times[index]-minTime)/divisor
			}
		}
		if len(probes) < 128 {
			probes = append(probes, corpusRow{Metric: row.Metric, Values: []float64{row.Values[len(row.Values)/2]}, Times: []int64{row.Times[len(row.Times)/2]}})
		}
		for first := 0; first < len(row.Values); first += batch {
			last := min(first+batch, len(row.Values))
			if maxSamples > 0 {
				last = min(last, first+maxSamples-total)
				if last <= first {
					break
				}
			}
			requests = append(requests, request{body: encode(row, first, last), samples: last - first})
			total += last - first
		}
		if maxSamples > 0 && total >= maxSamples {
			break
		}
	}
	if err := scanner.Err(); err != nil {
		log.Fatal(err)
	}
	return requests, probes, total
}

func quantile(values []time.Duration, percent int) time.Duration {
	if len(values) == 0 {
		return 0
	}
	slices.Sort(values)
	return values[(len(values)*percent+99)/100-1]
}

func selector(metric map[string]string) string {
	names := make([]string, 0, len(metric))
	for name := range metric {
		names = append(names, name)
	}
	sort.Strings(names)
	parts := make([]string, 0, len(names))
	for _, name := range names {
		parts = append(parts, name+"="+strconv.Quote(metric[name]))
	}
	return "{" + strings.Join(parts, ",") + "}"
}

func legacyMetricName(name string) bool {
	if name == "" {
		return false
	}
	for index := range len(name) {
		letter := name[index] >= 'a' && name[index] <= 'z' || name[index] >= 'A' && name[index] <= 'Z'
		digit := name[index] >= '0' && name[index] <= '9'
		if !letter && name[index] != '_' && name[index] != ':' && (index == 0 || !digit) {
			return false
		}
	}
	return true
}

func writeOpenMetrics(corpus, output string) {
	file, err := os.Open(corpus)
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
		if len(row.Values) != len(row.Times) {
			log.Fatal("corpus row lengths")
		}
		rows = append(rows, row)
	}
	if err := scanner.Err(); err != nil {
		log.Fatal(err)
	}
	sort.Slice(rows, func(i, j int) bool {
		return rows[i].Metric["__name__"] < rows[j].Metric["__name__"]
	})
	out, err := os.Create(output)
	if err != nil {
		log.Fatal(err)
	}
	defer out.Close()
	buffer := bufio.NewWriterSize(out, 1<<20)
	lastName, total := "", 0
	for _, row := range rows {
		name := row.Metric["__name__"]
		if name != lastName {
			typeName := name
			if !legacyMetricName(name) {
				typeName = strconv.Quote(name)
			}
			if _, err := fmt.Fprintf(buffer, "# TYPE %s gauge\n", typeName); err != nil {
				log.Fatal(err)
			}
			lastName = name
		}
		names := make([]string, 0, len(row.Metric))
		for label := range row.Metric {
			if label != "__name__" {
				names = append(names, label)
			}
		}
		sort.Strings(names)
		var labels []string
		for _, label := range names {
			labels = append(labels, strconv.Quote(label)+"="+strconv.Quote(row.Metric[label]))
		}
		seriesName := name
		if !legacyMetricName(name) {
			seriesName = "{" + strconv.Quote(name)
			if len(labels) > 0 {
				seriesName += "," + strings.Join(labels, ",")
			}
			seriesName += "}"
		} else if len(labels) > 0 {
			seriesName += "{" + strings.Join(labels, ",") + "}"
		}
		for index, value := range row.Values {
			if _, err := fmt.Fprintf(buffer, "%s %s %s\n", seriesName, strconv.FormatFloat(value, 'g', -1, 64), strconv.FormatFloat(float64(row.Times[index])/1000, 'f', 3, 64)); err != nil {
				log.Fatal(err)
			}
			total++
		}
	}
	if _, err := buffer.WriteString("# EOF\n"); err != nil {
		log.Fatal(err)
	}
	if err := buffer.Flush(); err != nil {
		log.Fatal(err)
	}
	fmt.Printf("stage=openmetrics series=%d samples=%d output=%s\n", len(rows), total, output)
}

func censusLabels(path string) {
	file, err := os.Open(path)
	if err != nil {
		log.Fatal(err)
	}
	defer file.Close()
	scanner := bufio.NewScanner(file)
	scanner.Buffer(make([]byte, 1<<20), 1<<28)
	series := map[int]int{}
	samples := map[int]int{}
	for scanner.Scan() {
		var row corpusRow
		if err := json.Unmarshal(scanner.Bytes(), &row); err != nil {
			log.Fatal(err)
		}
		count := len(row.Metric)
		series[count]++
		samples[count] += len(row.Values)
	}
	if err := scanner.Err(); err != nil {
		log.Fatal(err)
	}
	counts := make([]int, 0, len(series))
	for count := range series {
		counts = append(counts, count)
	}
	sort.Ints(counts)
	for _, count := range counts {
		fmt.Printf("stage=label_census labels=%d series=%d samples=%d\n", count, series[count], samples[count])
	}
}

func main() {
	corpus := flag.String("corpus", "", "normalized JSONL corpus")
	endpoint := flag.String("endpoint", "", "Prometheus remote-write base URL")
	stage := flag.String("stage", "write", "write, chronological_write, query, ram_query, native_stream, openmetrics or census")
	output := flag.String("output", "", "OpenMetrics output file")
	batch := flag.Int("batch", 1000, "samples per request")
	maxSamples := flag.Int("max-samples", 0, "optional smoke limit")
	queries := flag.Int("queries", 2000, "point queries")
	workers := flag.Int("workers", 1, "concurrent RAM query workers")
	seconds := flag.Int("seconds", 5, "light RAM query duration")
	shape := flag.String("shape", "light", "light or wide RAM query")
	engine := flag.String("engine", "", "vm or prom for native stream")
	anchor := flag.Int64("time-anchor-ms", 0, "map earliest sample to this timestamp, zero keeps corpus time")
	divisor := flag.Int64("time-divisor", 1, "divide timestamp offsets after anchoring")
	flag.Parse()
	if *stage == "native_stream" {
		nativeStreamQuery(*endpoint, *engine, *workers, *seconds)
		return
	}
	if *stage == "chronological_write" {
		if *corpus == "" || *endpoint == "" || *anchor == 0 || *divisor < 1 {
			log.Fatal("chronological write needs corpus, endpoint, anchor and positive divisor")
		}
		requests, total := chronologicalRequests(*corpus, *anchor, *divisor)
		replayChronological(*endpoint, requests, total, *anchor, *divisor)
		return
	}
	if *stage == "ram_query" {
		ramQuery(*endpoint, *shape, *workers, *seconds)
		return
	}
	if *stage == "census" {
		if *corpus == "" {
			log.Fatal("corpus required")
		}
		censusLabels(*corpus)
		return
	}
	if *stage == "openmetrics" {
		if *corpus == "" || *output == "" {
			log.Fatal("corpus and output required")
		}
		writeOpenMetrics(*corpus, *output)
		return
	}
	if *corpus == "" || *endpoint == "" || *batch < 1 || *queries < 1 || *divisor < 1 {
		log.Fatal("corpus, endpoint, positive batch and queries required")
	}
	requests, probes, total := readRequests(*corpus, *batch, *maxSamples, *anchor, *divisor)
	client := &http.Client{Timeout: 30 * time.Second}
	var timings []time.Duration
	start := time.Now()
	switch *stage {
	case "write":
		for _, item := range requests {
			req, err := http.NewRequest(http.MethodPost, strings.TrimRight(*endpoint, "/")+"/api/v1/write", bytes.NewReader(item.body))
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
				log.Fatalf("write response %s: %s: %v", response.Status, body, err)
			}
			timings = append(timings, time.Since(at))
		}
		elapsed := time.Since(start)
		fmt.Printf("stage=remote_write samples=%d requests=%d batch=%d time_anchor_ms=%d time_divisor=%d elapsed_s=%.6f samples_per_second=%.1f requests_per_second=%.1f p50_us=%.1f p99_us=%.1f\n", total, len(requests), *batch, *anchor, *divisor, elapsed.Seconds(), float64(total)/elapsed.Seconds(), float64(len(requests))/elapsed.Seconds(), float64(quantile(timings, 50).Microseconds()), float64(quantile(timings, 99).Microseconds()))
	case "query":
		if len(probes) == 0 {
			log.Fatal("no query probes")
		}
		found, mismatched := 0, 0
		for index := 0; index < *queries; index++ {
			probe := probes[(index*7919)%len(probes)]
			query := url.Values{"query": {selector(probe.Metric)}, "time": {strconv.FormatFloat(float64(probe.Times[0])/1000, 'f', 3, 64)}}
			at := time.Now()
			response, err := client.Get(strings.TrimRight(*endpoint, "/") + "/api/v1/query?" + query.Encode())
			if err != nil {
				log.Fatal(err)
			}
			body, err := io.ReadAll(io.LimitReader(response.Body, 1<<20))
			response.Body.Close()
			if err != nil || response.StatusCode/100 != 2 {
				log.Fatalf("query response %s: %s: %v", response.Status, body, err)
			}
			var result struct {
				Status string `json:"status"`
				Data   struct {
					Result []struct {
						Value []json.RawMessage `json:"value"`
					} `json:"result"`
				} `json:"data"`
			}
			if err := json.Unmarshal(body, &result); err != nil || result.Status != "success" {
				log.Fatalf("query decode: %v: %s", err, body)
			}
			if len(result.Data.Result) > 0 {
				found++
				if len(result.Data.Result[0].Value) == 2 {
					var printed string
					if err := json.Unmarshal(result.Data.Result[0].Value[1], &printed); err != nil {
						log.Fatal(err)
					}
					value, err := strconv.ParseFloat(printed, 64)
					if err != nil {
						log.Fatal(err)
					}
					if math.Float64bits(value) != math.Float64bits(probe.Values[0]) {
						mismatched++
					}
				}
			}
			timings = append(timings, time.Since(at))
		}
		elapsed := time.Since(start)
		fmt.Printf("stage=remote_query queries=%d found=%d bit_mismatches=%d time_anchor_ms=%d time_divisor=%d elapsed_s=%.6f qps=%.1f p50_us=%.1f p99_us=%.1f\n", *queries, found, mismatched, *anchor, *divisor, elapsed.Seconds(), float64(*queries)/elapsed.Seconds(), float64(quantile(timings, 50).Microseconds()), float64(quantile(timings, 99).Microseconds()))
	default:
		log.Fatal("unknown stage")
	}
}
