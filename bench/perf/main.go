// Command perf measures what the metrics engine costs in time, memory and log
// bytes, rather than in stored bytes. Every stage prints one line of key=value
// pairs so a run can be pasted into a table without being reformatted.
//
// Timed stages are meant to run one at a time: several of them at once measure
// the operating system's scheduler instead of the engine.
package main

import (
	"context"
	"flag"
	"fmt"
	"log"
	"math/rand/v2"
	"os"
	"path/filepath"
	"runtime"
	"slices"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/tinyshed/tinystore/metrics"
)

var (
	regions = []string{"eu-west-1", "us-east-1", "ap-southeast-1", "eu-central-1"}
	systems = []string{"Ubuntu24.04LTS", "Ubuntu22.04LTS", "Debian12"}
	staging = []string{"production", "staging", "test"}
)

// buildSeries mixes one high cardinality label with several low ones, so a
// selector can ask for one series or for every series of a region
func buildSeries(n int) []metrics.Series {
	out := make([]metrics.Series, n)
	for i := range out {
		out[i] = metrics.Series{Kind: metrics.Gauge, Labels: []metrics.Label{
			{Name: "__name__", Value: "metric_" + strconv.Itoa(i%20)},
			{Name: "host", Value: "host_" + strconv.Itoa(i/20)},
			{Name: "job", Value: "bench"},
			{Name: "region", Value: regions[i%len(regions)]},
			{Name: "os", Value: systems[i%len(systems)]},
			{Name: "service", Value: "svc_" + strconv.Itoa(i%16)},
			{Name: "rack", Value: strconv.Itoa(i % 50)},
			{Name: "environment", Value: staging[i%len(staging)]},
		}}
	}
	return out
}

type watcher struct {
	stop, done          chan struct{}
	peakHeap, peakGoSys uint64
	peakRSS             uint64
	walGrowth, walPeak  int64
}

// watch observes Go memory, OS RSS when available, and WAL file growth
func watch(path string) *watcher {
	w := &watcher{stop: make(chan struct{}), done: make(chan struct{})}
	previous := int64(0)
	if info, err := os.Stat(path + "-wal"); err == nil {
		previous = info.Size()
		w.walPeak = previous
	}
	go func() {
		defer close(w.done)
		var stats runtime.MemStats
		for {
			runtime.ReadMemStats(&stats)
			w.peakHeap = max(w.peakHeap, stats.HeapAlloc)
			w.peakGoSys = max(w.peakGoSys, stats.Sys)
			w.peakRSS = max(w.peakRSS, processRSS())
			if info, err := os.Stat(path + "-wal"); err == nil {
				size := info.Size()
				if size > previous {
					w.walGrowth += size - previous
				}
				w.walPeak = max(w.walPeak, size)
				previous = size
			} else {
				previous = 0
			}
			select {
			case <-w.stop:
				return
			case <-time.After(20 * time.Millisecond):
			}
		}
	}()
	return w
}

func processRSS() uint64 {
	data, err := os.ReadFile("/proc/self/statm")
	if err != nil {
		return 0
	}
	fields := strings.Fields(string(data))
	if len(fields) < 2 {
		return 0
	}
	pages, err := strconv.ParseUint(fields[1], 10, 64)
	if err != nil {
		return 0
	}
	return pages * uint64(os.Getpagesize())
}

func rssMiB(w *watcher) string {
	if w.peakRSS == 0 {
		return "unavailable"
	}
	return strconv.FormatFloat(float64(w.peakRSS)/(1<<20), 'f', 1, 64)
}

func (w *watcher) finish() *watcher {
	close(w.stop)
	<-w.done
	return w
}

type latencies struct {
	mu     sync.Mutex
	values []time.Duration
}

func (l *latencies) add(d time.Duration) {
	l.mu.Lock()
	l.values = append(l.values, d)
	l.mu.Unlock()
}

func (l *latencies) quantiles() (p50, p95, p99, worst time.Duration) {
	l.mu.Lock()
	defer l.mu.Unlock()
	if len(l.values) == 0 {
		return 0, 0, 0, 0
	}
	sorted := append([]time.Duration(nil), l.values...)
	slices.Sort(sorted)
	at := func(q float64) time.Duration {
		index := int(q * float64(len(sorted)-1))
		return sorted[index]
	}
	return at(0.50), at(0.95), at(0.99), sorted[len(sorted)-1]
}

func report(stage string, fields ...any) {
	parts := make([]string, 0, len(fields)/2+1)
	parts = append(parts, "STAGE="+stage)
	for i := 0; i+1 < len(fields); i += 2 {
		parts = append(parts, fmt.Sprintf("%v=%v", fields[i], fields[i+1]))
	}
	fmt.Println(strings.Join(parts, " "))
}

func micros(d time.Duration) string { return strconv.FormatFloat(d.Seconds()*1e6, 'f', 1, 64) }

func openStore(ctx context.Context, path string, series, readers int) *metrics.Store {
	return openStoreWithAdmission(ctx, path, series, readers, 0)
}

func openStoreWithAdmission(ctx context.Context, path string, series, readers, activeReads int) *metrics.Store {
	store, err := metrics.Open(ctx, path, metrics.Options{
		Retention:          365 * 24 * time.Hour,
		MaxReaders:         readers,
		MaxConcurrentReads: activeReads,
		MaxSeries:          2 * series,
		MaxHeadSamples:     8192,
		MaxBatchSamples:    200000,
		MaxBatchBytes:      64 << 20,
		Limits: metrics.Limits{
			Series: 2 * series, Blocks: 1 << 20, PayloadBytes: 1 << 30,
			DecodedSamples: 1 << 24, OutputSamples: 1 << 24,
		},
	})
	if err != nil {
		log.Fatal(err)
	}
	return store
}

func fileBytes(path string) int64 {
	var total int64
	for _, suffix := range []string{"", "-wal", "-shm"} {
		if info, err := os.Stat(path + suffix); err == nil {
			total += info.Size()
		}
	}
	return total
}

// ingest fills a fresh store and reports what one Ingest call costs
func ingest(ctx context.Context, dir, label string, seriesCount, samples, batch, maintainEvery int) {
	path := filepath.Join(dir, "ingest.db")
	os.Remove(path)
	store := openStore(ctx, path, seriesCount, 2)
	all := buildSeries(seriesCount)
	values := make([]int64, seriesCount)
	for i := range values {
		values[i] = int64(rand.IntN(1000))
	}
	step := int64(10000)
	start := time.Now().UnixMilli() - int64(samples)*step

	var before, after runtime.MemStats
	runtime.GC()
	runtime.ReadMemStats(&before)
	w := watch(path)
	calls := &latencies{}
	maintenance := &latencies{}
	written, sealed, rounds := 0, 0, 0
	begin := time.Now()

	// one batch carries `batch` samples, filled series by series
	next, offset := 0, 0
	for written < seriesCount*samples {
		size := min(batch, seriesCount*samples-written)
		batches := make([]metrics.Batch, 0, size)
		batchSamples := 0
		for batchSamples < size {
			take := min(size-batchSamples, samples-offset)
			if take <= 0 {
				next, offset = next+1, 0
				continue
			}
			points := make([]metrics.Sample, take)
			for i := range points {
				values[next] += int64(rand.IntN(7)) - 3
				points[i] = metrics.Sample{At: start + int64(offset+i)*step, Value: float64(values[next])}
			}
			batches = append(batches, metrics.Batch{Series: all[next], Samples: points})
			batchSamples += take
			offset += take
			if offset == samples {
				next, offset = next+1, 0
			}
			if next == seriesCount {
				break
			}
		}
		if len(batches) == 0 {
			break
		}
		at := time.Now()
		if err := store.Ingest(ctx, batches); err != nil {
			log.Fatalf("ingest: %v", err)
		}
		calls.add(time.Since(at))
		written += batchSamples
		rounds++
		if maintainEvery > 0 && rounds%maintainEvery == 0 {
			at = time.Now()
			result, err := store.Maintain(ctx)
			if err != nil {
				log.Fatalf("maintain: %v", err)
			}
			maintenance.add(time.Since(at))
			sealed += result.SealedBlocks
		}
	}
	for range 64 {
		result, err := store.Maintain(ctx)
		if err != nil {
			log.Fatalf("drain maintain: %v", err)
		}
		sealed += result.SealedBlocks
		if result.SealedBlocks == 0 {
			break
		}
	}
	elapsed := time.Since(begin)
	runtime.ReadMemStats(&after)
	w.finish()
	live := fileBytes(path)
	if err := store.Close(ctx); err != nil {
		log.Fatal(err)
	}
	p50, p95, p99, worst := calls.quantiles()
	m50, _, m99, mworst := maintenance.quantiles()
	report("ingest",
		"label", label, "series", seriesCount, "batch", batch, "samples", written,
		"seconds", strconv.FormatFloat(elapsed.Seconds(), 'f', 2, 64),
		"samples_per_second", int64(float64(written)/elapsed.Seconds()),
		"call_p50_us", micros(p50), "call_p95_us", micros(p95), "call_p99_us", micros(p99), "call_max_us", micros(worst),
		"maintain_p50_us", micros(m50), "maintain_p99_us", micros(m99), "maintain_max_us", micros(mworst),
		"sealed_blocks", sealed,
		"allocs_per_sample", strconv.FormatFloat(float64(after.Mallocs-before.Mallocs)/float64(written), 'f', 2, 64),
		"bytes_alloc_per_sample", strconv.FormatFloat(float64(after.TotalAlloc-before.TotalAlloc)/float64(written), 'f', 1, 64),
		"observed_wal_growth_bytes_per_sample", strconv.FormatFloat(float64(w.walGrowth)/float64(written), 'f', 2, 64),
		"wal_peak_bytes", w.walPeak,
		"peak_heap_mib", strconv.FormatFloat(float64(w.peakHeap)/(1<<20), 'f', 1, 64),
		"peak_go_sys_mib", strconv.FormatFloat(float64(w.peakGoSys)/(1<<20), 'f', 1, 64),
		"peak_os_rss_mib", rssMiB(w),
		"file_bytes_open", live,
		"file_bytes_closed", fileBytes(path),
		"bytes_per_sample", strconv.FormatFloat(float64(fileBytes(path))/float64(written), 'f', 4, 64),
	)
}

type readShape struct {
	name     string
	matchers []metrics.Label
	span     time.Duration
	rotate   bool
}

// read replays one shape of query at a chosen reader concurrency
func read(ctx context.Context, dir, label, only string, seriesCount, readers, activeReads, seconds int) {
	path, cleanup, err := copiedReadFixture(dir)
	if err != nil {
		log.Fatal(err)
	}
	defer cleanup()
	store := openStoreWithAdmission(ctx, path, seriesCount, readers, activeReads)
	defer store.Close(ctx)
	step := int64(10000)
	var first, last int64
	results, err := store.Read(ctx, metrics.Range{
		Matchers: []metrics.Label{{Name: "__name__", Value: "metric_0"}, {Name: "host", Value: "host_0"}},
		From:     0, To: 1 << 62,
	})
	if err != nil || len(results) == 0 {
		log.Fatalf("probe read: %v (%d results)", err, len(results))
	}
	first = results[0].Samples[0].At
	last = results[0].Samples[len(results[0].Samples)-1].At
	if only == "stream_region_hour" {
		streamRegionHour(ctx, store, seriesCount, readers, seconds, first, last)
		return
	}

	shapes := []readShape{
		{"point", []metrics.Label{{Name: "__name__", Value: "metric_0"}, {Name: "host", Value: "host_0"}}, time.Duration(step) * time.Millisecond, false},
		{"hour", []metrics.Label{{Name: "__name__", Value: "metric_0"}, {Name: "host", Value: "host_0"}}, time.Hour, false},
		{"all_labels_hour", buildSeries(seriesCount)[0].Labels, time.Hour, false},
		{"four_labels_hour", buildSeries(seriesCount)[0].Labels[:4], time.Hour, false},
		{"rotating_hour", nil, time.Hour, true},
		{"day", []metrics.Label{{Name: "__name__", Value: "metric_0"}, {Name: "host", Value: "host_0"}}, 24 * time.Hour, false},
		{"series_full", []metrics.Label{{Name: "__name__", Value: "metric_0"}, {Name: "host", Value: "host_0"}}, 0, false},
		{"selector_low_cardinality", []metrics.Label{{Name: "region", Value: regions[0]}}, time.Hour, false},
		{"selector_high_cardinality", []metrics.Label{{Name: "host", Value: "host_1"}}, time.Hour, false},
		{"scan_all", []metrics.Label{{Name: "job", Value: "bench"}}, 0, false},
	}
	for _, shape := range shapes {
		if only != "" && shape.name != only {
			continue
		}
		runShape(ctx, store, label, seriesCount, readers, activeReads, seconds, shape, first, last)
	}
}

func runShape(ctx context.Context, store *metrics.Store, label string, seriesCount, readers, activeReads, seconds int, shape readShape, first, last int64) {
	var rotating [][]metrics.Label
	if shape.rotate {
		rotating = make([][]metrics.Label, seriesCount)
		for id := range rotating {
			rotating[id] = []metrics.Label{{Name: "__name__", Value: "metric_" + strconv.Itoa(id%20)}, {Name: "host", Value: "host_" + strconv.Itoa(id/20)}}
		}
	}
	calls := &latencies{}
	var queries, points atomic.Int64
	deadline := time.Now().Add(time.Duration(seconds) * time.Second)
	var wg sync.WaitGroup
	runtime.GC()
	var before, after runtime.MemStats
	runtime.ReadMemStats(&before)
	w := watch("")
	begin := time.Now()
	for worker := range readers {
		wg.Add(1)
		go func(seed int) {
			defer wg.Done()
			source := rand.New(rand.NewPCG(uint64(seed), 7))
			for time.Now().Before(deadline) {
				matchers := shape.matchers
				if shape.rotate {
					matchers = rotating[source.IntN(len(rotating))]
				}
				from, to := first, last+1
				if shape.span > 0 {
					width := shape.span.Milliseconds()
					if width < last-first {
						from = first + source.Int64N(last-first-width)
					}
					to = from + width
				}
				at := time.Now()
				result, err := store.Read(ctx, metrics.Range{Matchers: matchers, From: from, To: to})
				if err != nil {
					log.Fatalf("read %s: %v", shape.name, err)
				}
				calls.add(time.Since(at))
				queries.Add(1)
				for _, series := range result {
					points.Add(int64(len(series.Samples)))
				}
			}
		}(worker)
	}
	wg.Wait()
	elapsed := time.Since(begin)
	runtime.ReadMemStats(&after)
	w.finish()
	p50, p95, p99, worst := calls.quantiles()
	total := queries.Load()
	report("read",
		"label", label, "series", seriesCount, "shape", shape.name, "readers", readers, "active_read_limit", activeReads,
		"queries", total,
		"queries_per_second", strconv.FormatFloat(float64(total)/elapsed.Seconds(), 'f', 1, 64),
		"samples_returned", points.Load(),
		"samples_per_second", int64(float64(points.Load())/elapsed.Seconds()),
		"p50_us", micros(p50), "p95_us", micros(p95), "p99_us", micros(p99), "max_us", micros(worst),
		"allocs_per_query", strconv.FormatFloat(float64(after.Mallocs-before.Mallocs)/float64(max(total, 1)), 'f', 1, 64),
		"peak_heap_mib", strconv.FormatFloat(float64(w.peakHeap)/(1<<20), 'f', 1, 64),
		"peak_go_sys_mib", strconv.FormatFloat(float64(w.peakGoSys)/(1<<20), 'f', 1, 64),
		"peak_os_rss_mib", rssMiB(w),
	)
}

// mixed is the shape an installation actually has: one writer that never stops
// and readers behaving like panels
func mixed(ctx context.Context, dir, label string, seriesCount, readers, activeReads, seconds int) {
	path, cleanup, err := copiedReadFixture(dir)
	if err != nil {
		log.Fatal(err)
	}
	defer cleanup()
	store := openStoreWithAdmission(ctx, path, seriesCount, readers, activeReads)
	all := buildSeries(seriesCount)
	step := int64(10000)
	// a repeated run must start after what an earlier one already sealed
	at := time.Now().UnixMilli()
	probe, err := store.Read(ctx, metrics.Range{
		Matchers: []metrics.Label{{Name: "__name__", Value: "metric_0"}, {Name: "host", Value: "host_0"}},
		From:     0, To: 1 << 62,
	})
	if err != nil {
		log.Fatalf("mixed probe: %v", err)
	}
	if len(probe) > 0 {
		points := probe[0].Samples
		at = max(at, points[len(points)-1].At+step)
	}
	ingestCalls, readCalls := &latencies{}, &latencies{}
	var written, queries atomic.Int64
	deadline := time.Now().Add(time.Duration(seconds) * time.Second)
	w := watch(path)
	var wg sync.WaitGroup
	wg.Go(func() {
		round := int64(0)
		for time.Now().Before(deadline) {
			batches := make([]metrics.Batch, 0, 256)
			for i := range min(256, seriesCount) {
				batches = append(batches, metrics.Batch{Series: all[i], Samples: []metrics.Sample{
					{At: at + round*step, Value: float64(round % 97)},
				}})
			}
			begin := time.Now()
			if err := store.Ingest(ctx, batches); err != nil {
				log.Fatalf("mixed ingest: %v", err)
			}
			ingestCalls.add(time.Since(begin))
			written.Add(int64(len(batches)))
			round++
			if round%16 == 0 {
				if _, err := store.Maintain(ctx); err != nil {
					log.Fatalf("mixed maintain: %v", err)
				}
			}
		}
	})
	for worker := range readers {
		wg.Add(1)
		go func(seed int) {
			defer wg.Done()
			for time.Now().Before(deadline) {
				begin := time.Now()
				_, err := store.Read(ctx, metrics.Range{
					Matchers: []metrics.Label{{Name: "__name__", Value: "metric_" + strconv.Itoa(seed%20)}},
					From:     at - 3600000, To: at + 3600000,
				})
				if err != nil {
					log.Fatalf("mixed read: %v", err)
				}
				readCalls.add(time.Since(begin))
				queries.Add(1)
			}
		}(worker)
	}
	wg.Wait()
	w.finish()
	if err := store.Close(ctx); err != nil {
		log.Fatal(err)
	}
	i50, i95, i99, imax := ingestCalls.quantiles()
	r50, r95, r99, rmax := readCalls.quantiles()
	report("mixed",
		"label", label, "series", seriesCount, "readers", readers, "active_read_limit", activeReads, "seconds", seconds,
		"ingested_samples", written.Load(), "queries", queries.Load(),
		"ingest_p50_us", micros(i50), "ingest_p95_us", micros(i95), "ingest_p99_us", micros(i99), "ingest_max_us", micros(imax),
		"read_p50_us", micros(r50), "read_p95_us", micros(r95), "read_p99_us", micros(r99), "read_max_us", micros(rmax),
		"wal_peak_bytes", w.walPeak,
		"peak_go_sys_mib", strconv.FormatFloat(float64(w.peakGoSys)/(1<<20), 'f', 1, 64),
		"peak_os_rss_mib", rssMiB(w),
	)
}

// populate builds the database the read stages share; its timings are not a result
func populate(ctx context.Context, dir string, seriesCount, samples int) {
	path := filepath.Join(dir, "read.db")
	os.Remove(path)
	os.Remove(path + "-wal")
	os.Remove(path + "-shm")
	store := openStore(ctx, path, seriesCount, 2)
	all := buildSeries(seriesCount)
	step := int64(10000)
	start := time.Now().UnixMilli() - int64(samples)*step
	value := int64(0)
	begin := time.Now()
	// one transaction is one fsync, so a batch spans series rather than one of them
	pending := make([]metrics.Batch, 0, 256)
	held := 0
	flush := func() {
		if len(pending) == 0 {
			return
		}
		if err := store.Ingest(ctx, pending); err != nil {
			log.Fatalf("populate: %v", err)
		}
		if _, err := store.Maintain(ctx); err != nil {
			log.Fatalf("populate maintain: %v", err)
		}
		pending, held = pending[:0], 0
	}
	for i := range all {
		for chunk := 0; chunk < samples; chunk += 4096 {
			size := min(4096, samples-chunk)
			points := make([]metrics.Sample, size)
			for j := range points {
				value += int64(rand.IntN(7)) - 3
				points[j] = metrics.Sample{At: start + int64(chunk+j)*step, Value: float64(value)}
			}
			pending = append(pending, metrics.Batch{Series: all[i], Samples: points})
			held += size
			if held >= 32768 || len(pending) == cap(pending) {
				flush()
			}
		}
	}
	flush()
	for range 4096 {
		result, err := store.Maintain(ctx)
		if err != nil {
			log.Fatalf("populate drain: %v", err)
		}
		if result.SealedBlocks == 0 {
			break
		}
	}
	if err := store.Close(ctx); err != nil {
		log.Fatal(err)
	}
	report("populate", "series", seriesCount, "samples_per_series", samples,
		"total_samples", seriesCount*samples,
		"seconds", strconv.FormatFloat(time.Since(begin).Seconds(), 'f', 2, 64),
		"file_bytes", fileBytes(path),
		"bytes_per_sample", strconv.FormatFloat(float64(fileBytes(path))/float64(seriesCount*samples), 'f', 4, 64))
}

func main() {
	dir := flag.String("dir", ".", "directory for the database")
	file := flag.String("file", "read.db", "database filename for the objects stage")
	corpus := flag.String("corpus", "", "normalized JSONL corpus for TSBS RSS ingestion")
	stage := flag.String("stage", "ingest", "ingest, register, append, ready_churn, maintenance_batch, churn_prepare, churn_expire, long_churn, multi_store, idle_rss, tsbs_rss, tsbs_ingest_rss, tsbs_ingest_maint_rss, maintenance_ingest_rss, narrow_populate, populate, steady, read, aggregate, mixed or objects")
	label := flag.String("label", "run", "name for this run")
	seriesCount := flag.Int("series", 1000, "series to write")
	samples := flag.Int("samples", 1000, "samples per series")
	batch := flag.Int("batch", 100, "samples in one Ingest call")
	seedSamples := flag.Int("seed-samples", 1, "initial samples per series before append timing")
	maintainEvery := flag.Int("maintain-every", 16, "run Maintain after this many batches, zero to skip")
	readers := flag.Int("readers", 1, "concurrent readers")
	activeReads := flag.Int("active-reads", 0, "admitted reads including decode; zero follows the reader pool")
	shape := flag.String("shape", "", "run only this read shape, empty for all")
	seconds := flag.Int("seconds", 10, "how long a read or mixed stage runs")
	epochs := flag.Int("epochs", 30, "rotating cardinality epochs")
	sharedBytes := flag.Int64("shared-bytes", 0, "shared active-work reservation bytes for multi_store")
	flag.Parse()
	defer startProfiles()()
	if err := os.MkdirAll(*dir, 0o755); err != nil {
		log.Fatal(err)
	}
	ctx := context.Background()
	switch *stage {
	case "ingest":
		ingest(ctx, *dir, *label, *seriesCount, *samples, *batch, *maintainEvery)
	case "register", "append":
		writePath(ctx, *dir, *stage, *seriesCount, *samples, *batch, *seedSamples)
	case "objects":
		objects(ctx, *dir, *file)
	case "ready_churn":
		readyChurn(ctx, *dir, *samples)
	case "maintenance_batch":
		maintenanceBatch(ctx, *dir, *seriesCount, *samples)
	case "churn_prepare":
		churnPrepare(ctx, *dir, *seriesCount)
	case "churn_expire":
		churnExpire(ctx, *dir, *seriesCount)
	case "long_churn":
		longChurn(ctx, *dir, *seriesCount, *epochs, *corpus)
	case "multi_store":
		multiStore(ctx, *dir, *seriesCount, *seconds, *sharedBytes)
	case "idle_rss":
		idleRSS(ctx, *dir, *seriesCount, *readers, *seconds)
	case "tsbs_rss":
		tsbsRSS(ctx, *dir, *shape, *readers, *seconds)
	case "tsbs_ingest_rss":
		tsbsIngestRSS(ctx, *dir, *corpus, false)
	case "tsbs_ingest_maint_rss":
		tsbsIngestRSS(ctx, *dir, *corpus, true)
	case "maintenance_ingest_rss":
		maintenanceIngestRSS(ctx, *dir, *seriesCount, *seconds)
	case "narrow_populate":
		narrowPopulate(ctx, *dir, *seriesCount, *samples)
	case "populate":
		populate(ctx, *dir, *seriesCount, *samples)
	case "steady":
		steady(ctx, *dir, *label, *seriesCount, *samples)
	case "read":
		read(ctx, *dir, *label, *shape, *seriesCount, *readers, *activeReads, *seconds)
	case "aggregate":
		aggregate(ctx, *dir, *shape, *seriesCount, *readers, *seconds)
	case "mixed":
		mixed(ctx, *dir, *label, *seriesCount, *readers, *activeReads, *seconds)
	default:
		log.Fatalf("unknown stage %q", *stage)
	}
}
