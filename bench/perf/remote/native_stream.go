package main

import (
	"bufio"
	"bytes"
	"encoding/binary"
	"encoding/json"
	"errors"
	"fmt"
	"hash/crc32"
	"io"
	"log"
	"net/http"
	"net/url"
	"sort"
	"strings"
	"sync"
	"time"

	"github.com/klauspost/compress/s2"
)

func nativeStreamQuery(endpoint, engine string, workers, seconds int) {
	if endpoint == "" || engine != "vm" && engine != "prom" || workers < 1 || seconds < 1 {
		log.Fatal("native stream needs endpoint, vm/prom engine, positive workers and seconds")
	}
	client := &http.Client{Timeout: 2 * time.Minute}
	var mu sync.Mutex
	var calls, units, bytesRead int64
	var durations []time.Duration
	deadline := time.Now().Add(time.Duration(seconds) * time.Second)
	start := time.Now()
	var group sync.WaitGroup
	for range workers {
		group.Go(func() {
			for time.Now().Before(deadline) {
				at := time.Now()
				request := streamRequest(endpoint, engine)
				response, err := client.Do(request)
				if err != nil {
					log.Fatal(err)
				}
				if response.StatusCode/100 != 2 {
					body, _ := io.ReadAll(io.LimitReader(response.Body, 4096))
					response.Body.Close()
					log.Fatalf("native stream %s: %s", response.Status, body)
				}
				var count, size int64
				if engine == "vm" {
					count, size, err = readVMLines(response.Body)
				} else {
					if content := response.Header.Get("Content-Type"); !strings.HasPrefix(content, "application/x-streamed-protobuf") {
						log.Fatalf("Prometheus returned %q instead of streamed chunks", content)
					}
					count, size, err = readPromFrames(response.Body)
				}
				response.Body.Close()
				if err != nil || count == 0 {
					log.Fatalf("native stream rows %d: %v", count, err)
				}
				mu.Lock()
				calls++
				units += count
				bytesRead += size
				durations = append(durations, time.Since(at))
				mu.Unlock()
			}
		})
	}
	group.Wait()
	elapsed := time.Since(start)
	sort.Slice(durations, func(i, j int) bool { return durations[i] < durations[j] })
	p50, p99 := durations[(len(durations)*50+99)/100-1], durations[(len(durations)*99+99)/100-1]
	fmt.Printf("stage=native_stream engine=%s workers=%d calls=%d lines_or_chunked_series=%d response_bytes=%d elapsed_s=%.3f qps=%.1f p50_ms=%.1f p99_ms=%.1f\n", engine, workers, calls, units, bytesRead, elapsed.Seconds(), float64(calls)/elapsed.Seconds(), float64(p50.Milliseconds()), float64(p99.Milliseconds()))
}

func streamRequest(endpoint, engine string) *http.Request {
	base := strings.TrimRight(endpoint, "/")
	if engine == "vm" {
		query := url.Values{
			"match[]": {`{region="eu-west-1"}`},
			"start":   {"1790119200"}, "end": {"1790120774.375"},
			"reduce_mem_usage": {"1"}, "max_rows_per_line": {"1000"},
		}
		request, err := http.NewRequest(http.MethodGet, base+"/api/v1/export?"+query.Encode(), nil)
		if err != nil {
			log.Fatal(err)
		}
		return request
	}
	matcher := stringField(nil, 2, "region")
	matcher = stringField(matcher, 3, "eu-west-1")
	query := uvarint(nil, 8)
	query = uvarint(query, 1790119200000)
	query = uvarint(query, 16)
	query = uvarint(query, 1790120774375)
	query = messageField(query, 3, matcher)
	body := messageField(nil, 1, query)
	body = uvarint(body, 16)
	body = uvarint(body, 1)
	request, err := http.NewRequest(http.MethodPost, base+"/api/v1/read", bytes.NewReader(s2.EncodeSnappy(nil, body)))
	if err != nil {
		log.Fatal(err)
	}
	request.Header.Set("Content-Type", "application/x-protobuf")
	request.Header.Set("Content-Encoding", "snappy")
	request.Header.Set("Accept", "application/x-streamed-protobuf")
	request.Header.Set("X-Prometheus-Remote-Read-Version", "0.1.0")
	return request
}

func readVMLines(body io.Reader) (int64, int64, error) {
	scanner := bufio.NewScanner(body)
	scanner.Buffer(make([]byte, 1<<20), 10<<20)
	var lines, samples, size int64
	for scanner.Scan() {
		var row struct {
			Values []json.RawMessage `json:"values"`
		}
		if err := json.Unmarshal(scanner.Bytes(), &row); err != nil {
			return 0, 0, err
		}
		lines++
		samples += int64(len(row.Values))
		size += int64(len(scanner.Bytes()) + 1)
	}
	if err := scanner.Err(); err != nil {
		return 0, 0, err
	}
	if samples != 763560 {
		return 0, 0, fmt.Errorf("VM stream returned %d samples", samples)
	}
	return lines, size, nil
}

func readPromFrames(body io.Reader) (int64, int64, error) {
	reader := bufio.NewReader(body)
	table := crc32.MakeTable(crc32.Castagnoli)
	var series, samples, size int64
	for {
		length, err := binary.ReadUvarint(reader)
		if errors.Is(err, io.EOF) {
			if samples != 763560 {
				return 0, 0, fmt.Errorf("Prometheus stream returned %d samples", samples)
			}
			return series, size, nil
		}
		if err != nil {
			return 0, 0, err
		}
		if length == 0 || length > 2<<20 {
			return 0, 0, fmt.Errorf("invalid Prometheus frame size %d", length)
		}
		var checksum [4]byte
		if _, err := io.ReadFull(reader, checksum[:]); err != nil {
			return 0, 0, err
		}
		payload := make([]byte, length)
		if _, err := io.ReadFull(reader, payload); err != nil {
			return 0, 0, err
		}
		if crc32.Checksum(payload, table) != binary.BigEndian.Uint32(checksum[:]) {
			return 0, 0, errors.New("Prometheus streamed frame checksum")
		}
		count, points, err := countChunkedSeries(payload)
		if err != nil {
			return 0, 0, err
		}
		series += int64(count)
		samples += int64(points)
		size += int64(length) + 4 + int64(binary.MaxVarintLen64)
	}
}

func countChunkedSeries(payload []byte) (int, int, error) {
	count, samples := 0, 0
	for len(payload) > 0 {
		field, value, remaining, err := nextProtoField(payload)
		if err != nil {
			return 0, 0, err
		}
		if field == 1 {
			points, err := countSeriesChunkSamples(value)
			if err != nil {
				return 0, 0, err
			}
			count++
			samples += points
		}
		payload = remaining
	}
	return count, samples, nil
}

func countSeriesChunkSamples(payload []byte) (int, error) {
	samples := 0
	for len(payload) > 0 {
		field, value, remaining, err := nextProtoField(payload)
		if err != nil {
			return 0, err
		}
		if field == 2 {
			points, err := countChunkSamples(value)
			if err != nil {
				return 0, err
			}
			samples += points
		}
		payload = remaining
	}
	return samples, nil
}

func countChunkSamples(payload []byte) (int, error) {
	chunkType := uint64(0)
	var data []byte
	for len(payload) > 0 {
		field, value, remaining, err := nextProtoField(payload)
		if err != nil {
			return 0, err
		}
		switch field {
		case 3:
			chunkType, _ = binary.Uvarint(value)
		case 4:
			data = value
		}
		payload = remaining
	}
	if chunkType != 1 || len(data) < 2 {
		return 0, fmt.Errorf("unexpected streamed chunk type %d or size %d", chunkType, len(data))
	}
	return int(binary.BigEndian.Uint16(data[:2])), nil
}

func nextProtoField(payload []byte) (int, []byte, []byte, error) {
	tag, read := binary.Uvarint(payload)
	if read <= 0 {
		return 0, nil, nil, errors.New("Prometheus streamed protobuf tag")
	}
	payload = payload[read:]
	switch tag & 7 {
	case 0:
		_, read = binary.Uvarint(payload)
		if read <= 0 {
			return 0, nil, nil, errors.New("Prometheus streamed protobuf varint")
		}
		return int(tag >> 3), payload[:read], payload[read:], nil
	case 2:
		length, read := binary.Uvarint(payload)
		if read <= 0 || length > uint64(len(payload)-read) {
			return 0, nil, nil, errors.New("Prometheus streamed protobuf length")
		}
		return int(tag >> 3), payload[read : read+int(length)], payload[read+int(length):], nil
	default:
		return 0, nil, nil, fmt.Errorf("Prometheus streamed protobuf wire type %d", tag&7)
	}
}
