package spike

import (
	"bufio"
	"encoding/json"
	"fmt"
	"maps"
	"os"
	"path/filepath"
	"regexp"
	"slices"
	"strings"
	"testing"
	"time"
)

// TestKVProductionRequestRates reads container logs that
// bench/fetch-docker-logs.sh collected, through TINYSTORE_KV_DOCKER=<corpus>,
// and prints aggregates only, never a name: the requests a second that the
// services logging their requests saw, the busiest service alone and each
// host's together. A request is a line naming an HTTP method with a path or a
// URL and then an HTTP version or a status, in text or as JSON fields; a
// service that logs fewer than a hundred of them is not counted
func TestKVProductionRequestRates(t *testing.T) {
	corpus := os.Getenv("TINYSTORE_KV_DOCKER")
	if corpus == "" {
		t.Skip("set TINYSTORE_KV_DOCKER=<corpus> to read container logs")
	}
	hosts, err := filepath.Glob(filepath.Join(corpus, "host-*"))
	if err != nil || len(hosts) == 0 {
		t.Fatalf("no host-* under %s: %v", corpus, err)
	}
	var busiest kvRates
	for _, host := range hosts {
		containers, err := filepath.Glob(filepath.Join(host, "raw", "*"))
		if err != nil {
			t.Fatal(err)
		}
		together := map[int64]int{}
		services := 0
		for _, container := range containers {
			seconds := kvRequestSeconds(t, container)
			rates := kvRatesOf(seconds)
			if rates.requests < 100 {
				continue
			}
			services++
			t.Logf("  a service: %s", rates)
			if rates.peak > busiest.peak {
				busiest = rates
			}
			for second, count := range seconds {
				together[second] += count
			}
		}
		t.Logf("a host with %d services logging requests, together: %s", services, kvRatesOf(together))
	}
	t.Logf("the busiest service alone: %s", busiest)
}

// kvRequestLine is a request in text: a method and a path or URL, then an HTTP
// version or a status
var kvRequestLine = regexp.MustCompile(
	`\b(GET|POST|PUT|PATCH|DELETE|HEAD|OPTIONS) (/|https?://)\S*(\s+HTTP/\d|["\s].*?\b[1-5]\d\d\b)`)

// kvRequestSeconds counts one container's requests by the second the daemon
// received their lines
func kvRequestSeconds(t *testing.T, container string) map[int64]int {
	t.Helper()
	files, err := filepath.Glob(filepath.Join(container, "*-json.log*"))
	if err != nil {
		t.Fatal(err)
	}
	seconds := map[int64]int{}
	for _, name := range files {
		kvCountRequests(t, name, seconds)
	}
	return seconds
}

func kvCountRequests(t *testing.T, name string, seconds map[int64]int) {
	t.Helper()
	file, err := os.Open(name)
	if err != nil {
		t.Fatal(err)
	}
	defer file.Close()
	lines := bufio.NewScanner(file)
	lines.Buffer(make([]byte, 1<<20), 64<<20)
	for lines.Scan() {
		var line struct {
			Log  string    `json:"log"`
			Time time.Time `json:"time"`
		}
		if json.Unmarshal(lines.Bytes(), &line) != nil {
			continue
		}
		if kvIsRequest(line.Log) {
			seconds[line.Time.Unix()]++
		}
	}
	if err := lines.Err(); err != nil {
		t.Fatalf("%s: %v", filepath.Base(name), err)
	}
}

func kvIsRequest(log string) bool {
	log = strings.TrimSpace(log)
	if !strings.HasPrefix(log, "{") {
		return kvRequestLine.MatchString(log)
	}
	var fields map[string]any
	if json.Unmarshal([]byte(log), &fields) != nil {
		return kvRequestLine.MatchString(log)
	}
	method, _ := kvField(fields, "method", "http.method", "http_method", "req.method", "request.method", "http.request.method").(string)
	switch strings.ToUpper(method) {
	case "GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS":
	default:
		return false
	}
	return kvField(fields, "status", "statusCode", "status_code", "http.status_code", "res.statusCode",
		"response.status", "path", "url", "uri", "req.url", "request.uri", "request.path") != nil
}

// kvField is the first of the names a JSON object has, a dotted name either as
// a key of its own or as nested objects
func kvField(fields map[string]any, names ...string) any {
	for _, name := range names {
		if value, ok := fields[name]; ok {
			return value
		}
		parent, child, nested := strings.Cut(name, ".")
		if !nested {
			continue
		}
		if inner, ok := fields[parent].(map[string]any); ok {
			if value := kvField(inner, child); value != nil {
				return value
			}
		}
	}
	return nil
}

// kvRates is a count of requests by second, summed: every second from the first
// request to the last is counted, the quiet ones too
type kvRates struct {
	requests, peak int
	days, mean     float64
	p99, p999      int
	peakMinute     float64
}

func kvRatesOf(seconds map[int64]int) kvRates {
	if len(seconds) == 0 {
		return kvRates{}
	}
	keys := slices.Sorted(maps.Keys(seconds))
	first, last := keys[0], keys[len(keys)-1]
	counts := make([]int, last-first+1)
	minutes := map[int64]int{}
	rates := kvRates{}
	for second, count := range seconds {
		counts[second-first] = count
		minutes[second/60] += count
		rates.requests += count
	}
	slices.Sort(counts)
	rates.peak = counts[len(counts)-1]
	rates.p99 = counts[len(counts)*99/100]
	rates.p999 = counts[len(counts)*999/1000]
	rates.days = float64(len(counts)) / 86_400
	rates.mean = float64(rates.requests) / float64(len(counts))
	rates.peakMinute = float64(slices.Max(slices.Collect(maps.Values(minutes)))) / 60
	return rates
}

func (r kvRates) String() string {
	return fmt.Sprintf("%d requests over %.1f days; a second: mean %.2f, p99 %d, p99.9 %d, peak %d; "+
		"the busiest minute %.1f a second", r.requests, r.days, r.mean, r.p99, r.p999, r.peak, r.peakMinute)
}
