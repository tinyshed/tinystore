package spike

import (
	"bytes"
	"database/sql"
	"fmt"
	"math"
	"math/rand/v2"
	"os"
	"path/filepath"
	"runtime"
	"slices"
	"strconv"
	"testing"
	"time"
)

// v2Batches cuts each stream's arrivals into segments, as a head would seal them
func v2Batches(events []recordEvent) [][]recordEvent {
	var order []string
	streams := map[string][]recordEvent{}
	for _, event := range events {
		if _, ok := streams[event.stream]; !ok {
			order = append(order, event.stream)
		}
		streams[event.stream] = append(streams[event.stream], event)
	}
	var batches [][]recordEvent
	for _, stream := range order {
		pending, size := streams[stream], 0
		start := 0
		for i := range pending {
			next := v2EventBytes(&pending[i])
			if i-start == v2SegmentEvents || size+next > v2SegmentBytes {
				batches, start, size = append(batches, pending[start:i]), i, 0
			}
			size += next
		}
		batches = append(batches, pending[start:])
	}
	return batches
}

type v2Measured struct {
	segments []v2Segment
	stored   []recordEvent
	encode   time.Duration
	payload  int
	blooms   int
}

func v2Measure(t testing.TB, events []recordEvent, arrival bool) v2Measured {
	t.Helper()
	encoder, decoder := v2TestCodec(t)
	if value, err := strconv.Atoi(os.Getenv("TINYSTORE_RECORD_BLOCK")); err == nil {
		encoder.blockEvents, encoder.blockBytes = value, 4*recordByteLimit
	}
	var measured v2Measured
	for _, batch := range v2Batches(events) {
		start := time.Now()
		segment, stored, err := encoder.encodeSegment(batch, arrival)
		measured.encode += time.Since(start)
		if err != nil {
			t.Fatal(err)
		}
		schema, err := decoder.decodeSchema(segment.row)
		if err != nil {
			t.Fatal(err)
		}
		var decoded []recordEvent
		for _, block := range segment.blocks {
			part, err := decoder.decodeBlock(&schema, block.body)
			if err != nil {
				t.Fatal(err)
			}
			decoded = append(decoded, part...)
			measured.payload += len(block.body)
			if len(block.traces) > 0 {
				measured.blooms += len(v2Bloom(block.traces, 16))
			}
			for _, filter := range block.filters {
				measured.blooms += len(filter.bloom)
			}
		}
		assertRecordEvents(t, stored, decoded)
		measured.payload += len(segment.row)
		measured.segments = append(measured.segments, segment)
		measured.stored = append(measured.stored, stored...)
	}
	return measured
}

func v2Fixture(name string, count int) []recordEvent {
	if name == "stateful" {
		return recordStatefulFixture(count)
	}
	return recordFixture(name, count)
}

func TestRecordV2Density(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1 to measure v2 record segments")
	}
	type input struct {
		name   string
		events []recordEvent
	}
	var inputs []input
	for _, name := range []string{"frontend", "backend", "mixed", "context_churn", "derived_fields", "stateful"} {
		inputs = append(inputs, input{name, v2Fixture(name, 10_000)})
	}
	for _, name := range []string{"frontend", "backend"} {
		inputs = append(inputs, input{name + "_1m", v2Fixture(name, 1_000_000)})
	}
	if path := os.Getenv("TINYSTORE_RECORD_EVENTS"); path != "" {
		inputs = append(inputs, input{"github", readRecordArchive(t, path)})
	}
	t.Log("dataset records mode payload_B/rec bloom_B/rec file_B/rec encode_rec/s write_rec/s objects")
	for _, in := range inputs {
		for _, arrival := range []bool{false, true} {
			v2LogDensity(t, in.name, in.events, arrival)
		}
	}
}

func v2LogDensity(t *testing.T, name string, events []recordEvent, arrival bool) {
	t.Helper()
	measured := v2Measure(t, events, arrival)
	db, err := v2OpenStore(t.Context(), filepath.Join(t.TempDir(), "records.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	start := time.Now()
	if err = v2WriteSegments(t.Context(), db, measured.segments); err != nil {
		t.Fatal(err)
	}
	write := time.Since(start)
	size, objects, err := v2StoreSize(t.Context(), db)
	if err != nil {
		t.Fatal(err)
	}
	mode := "time"
	if arrival {
		mode = "arrival"
	}
	n := float64(len(events))
	t.Logf("%-14s %8d %-7s %8.4f %6.4f %8.4f %10.0f %10.0f %s", name, len(events), mode, float64(measured.payload)/n,
		float64(measured.blooms)/n, float64(size)/n, n/measured.encode.Seconds(), n/write.Seconds(), objects)
}

// v2SameRecords compares as multisets: equal times may come back from different blocks
func v2SameRecords(t testing.TB, expected, actual []recordEvent) {
	t.Helper()
	key := func(events []recordEvent) [][]byte {
		keys := make([][]byte, len(events))
		for i, event := range events {
			keys[i] = appendRecordEvent(nil, event)
		}
		slices.SortFunc(keys, bytes.Compare)
		return keys
	}
	if !slices.EqualFunc(key(expected), key(actual), bytes.Equal) {
		t.Fatalf("query returned %d records, expected %d or different ones", len(actual), len(expected))
	}
	for i := 1; i < len(actual); i++ {
		if actual[i-1].at > actual[i].at {
			t.Fatal("query result is not in time order")
		}
	}
}

func v2Where(events []recordEvent, from, to int64, keep func(*recordEvent) bool) []recordEvent {
	var kept []recordEvent
	for i := range events {
		if events[i].at >= from && events[i].at < to && keep(&events[i]) {
			kept = append(kept, events[i])
		}
	}
	return kept
}

type v2Query struct {
	name     string
	from, to int64
	run      func(r *v2Reader) ([]recordEvent, error)
	keep     func(*recordEvent) bool
}

func v2RunQueries(t *testing.T, events []recordEvent, queries []v2Query) {
	t.Helper()
	measured := v2Measure(t, events, false)
	db, err := v2OpenStore(t.Context(), filepath.Join(t.TempDir(), "records.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	if err = v2WriteSegments(t.Context(), db, measured.segments); err != nil {
		t.Fatal(err)
	}
	var blocks int
	if err = db.QueryRowContext(t.Context(), `select count(*) from blocks`).Scan(&blocks); err != nil {
		t.Fatal(err)
	}
	t.Logf("%d records in %d segments, %d blocks", len(events), len(measured.segments), blocks)
	for _, query := range queries {
		_, decoder := v2TestCodec(t)
		reader := &v2Reader{db: db, decoder: decoder, schemas: map[int64]*v2Schema{}}
		start := time.Now()
		got, err := query.run(reader)
		elapsed := time.Since(start)
		if err != nil {
			t.Fatal(query.name, err)
		}
		v2SameRecords(t, v2Where(events, query.from, query.to, query.keep), got)
		t.Logf("%-34s rows=%6d %s time=%v", query.name, len(got), reader, elapsed.Round(time.Microsecond))
	}
}

func TestRecordV2Queries(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1 to measure v2 record queries")
	}
	const base = int64(1_790_000_000_000_000_000)
	all := func(*recordEvent) bool { return true }
	frontend := recordFixture("frontend", 1_000_000)
	middle := base + 500_000*1_234_567
	session := frontend[123_456].context[4]
	element := recordField{"element", recordJSON("buy")}
	v2RunQueries(t, frontend, []v2Query{
		{"frontend: one second", middle, middle + 1e9, func(r *v2Reader) ([]recordEvent, error) {
			return r.readRange(t.Context(), middle, middle+1e9)
		}, all},
		{"frontend: one session, all time", math.MinInt64, math.MaxInt64, func(r *v2Reader) ([]recordEvent, error) {
			return r.readContext(t.Context(), session, math.MinInt64, math.MaxInt64)
		}, func(e *recordEvent) bool { return slices.Contains(e.context, session) }},
		{"frontend: one session, one minute", middle, middle + 60e9, func(r *v2Reader) ([]recordEvent, error) {
			return r.readContext(t.Context(), session, middle, middle+60e9)
		}, func(e *recordEvent) bool { return slices.Contains(e.context, session) }},
		{"frontend: element=buy, ten seconds", middle, middle + 10e9, func(r *v2Reader) ([]recordEvent, error) {
			return r.readAttr(t.Context(), element, middle, middle+10e9)
		}, func(e *recordEvent) bool { return slices.Contains(e.attrs, element) }},
	})
	backend := recordFixture("backend", 1_000_000)
	middle = base + 500_000*1_000_003
	trace := backend[777_777].traceID
	v2RunQueries(t, backend, []v2Query{
		{"backend: one second", middle, middle + 1e9, func(r *v2Reader) ([]recordEvent, error) {
			return r.readRange(t.Context(), middle, middle+1e9)
		}, all},
		{"backend: level>=8, one minute", middle, middle + 60e9, func(r *v2Reader) ([]recordEvent, error) {
			return r.readLevel(t.Context(), middle, middle+60e9, 8)
		}, func(e *recordEvent) bool { return e.level != nil && *e.level >= 8 }},
		{"backend: one trace, all time", math.MinInt64, math.MaxInt64, func(r *v2Reader) ([]recordEvent, error) {
			return r.readTrace(t.Context(), trace, math.MinInt64, math.MaxInt64)
		}, func(e *recordEvent) bool { return bytes.Equal(e.traceID, trace) }},
		{"backend: one trace, one minute", middle, middle + 60e9, func(r *v2Reader) ([]recordEvent, error) {
			return r.readTrace(t.Context(), trace, middle, middle+60e9)
		}, func(e *recordEvent) bool { return bytes.Equal(e.traceID, trace) }},
	})
}

// TestRecordV2PageFit divides the same records by block size and SQLite page size
func TestRecordV2PageFit(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1 to measure page fit")
	}
	for _, name := range []string{"frontend", "backend"} {
		events := recordFixture(name, 1_000_000)
		for _, blockEvents := range []int{1024, 2048, 4096} {
			encoder, _ := v2TestCodec(t)
			encoder.blockEvents, encoder.blockBytes = blockEvents, 4*recordByteLimit
			var segments []v2Segment
			for _, batch := range v2Batches(events) {
				segment, _, err := encoder.encodeSegment(batch, false)
				if err != nil {
					t.Fatal(err)
				}
				segments = append(segments, segment)
			}
			for _, pageSize := range []int{1024, 4096, 16384} {
				db, err := sql.Open("sqlite", filepath.Join(t.TempDir(), "page.db"))
				if err != nil {
					t.Fatal(err)
				}
				db.SetMaxOpenConns(1)
				if _, err = db.ExecContext(t.Context(), fmt.Sprintf("pragma page_size=%d", pageSize)); err != nil {
					t.Fatal(err)
				}
				if _, err = db.ExecContext(t.Context(), v2StoreSchema); err != nil {
					t.Fatal(err)
				}
				if err = v2WriteSegments(t.Context(), db, segments); err != nil {
					t.Fatal(err)
				}
				size, objects, err := v2StoreSize(t.Context(), db)
				if err != nil {
					t.Fatal(err)
				}
				db.Close()
				t.Logf("%-8s block=%4d page=%5d file_B/rec=%.4f %s", name, blockEvents, pageSize,
					float64(size)/float64(len(events)), objects)
			}
		}
	}
}

// TestRecordV2AgainstV1 compares both codecs on identical input in one run
func TestRecordV2AgainstV1(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1 to compare v1 and v2")
	}
	t.Log("dataset v1_payload v1_encode_s v2_time_payload v2_arrival_payload v2_bloom v2_encode_s")
	for _, name := range []string{"frontend", "backend", "mixed", "context_churn", "derived_fields", "stateful"} {
		events := v2Fixture(name, 10_000)
		codec := recordTestCodec(t)
		codec.features = recordDeepAll | recordScope16 | recordOuterCompression
		batches := recordDeepBatches(t, events)
		start := time.Now()
		v1 := 0
		for s := 0; s < len(batches); s += codec.segmentBlockLimit() {
			body, err := codec.encodeRecordSegment(batches[s:min(s+codec.segmentBlockLimit(), len(batches))])
			if err != nil {
				t.Fatal(err)
			}
			v1 += len(body)
		}
		v1Time := time.Since(start)
		sorted, arrival := v2Measure(t, events, false), v2Measure(t, events, true)
		n := float64(len(events))
		t.Logf("%-15s %8.4f %8.3f %8.4f %8.4f %6.4f %8.3f", name, float64(v1)/n, v1Time.Seconds(),
			float64(sorted.payload)/n, float64(arrival.payload)/n, float64(sorted.blooms)/n, sorted.encode.Seconds())
	}
}

// TestRecordV2FrontendFloor states what the frontend generator draws per record, so a
// density claim on it can be read against the information it holds
func TestRecordV2FrontendFloor(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1 to compute the frontend floor")
	}
	events := recordFixture("frontend", 10_000)
	times := make([]int64, len(events))
	for i := range events {
		times[i] = events[i].at
	}
	slices.Sort(times)
	best := math.Inf(1)
	for k := range uint64(40) {
		cost := 64.0
		for i := 1; i < len(times); i++ {
			cost += float64(uint64(times[i]-times[i-1])>>k) + 1 + float64(k)
		}
		best = min(best, cost)
	}
	flag := 1.0 / 17
	n := float64(len(events))
	arrival, sorted := math.Log2(1e9)/8, best/n/8
	contexts := math.Log2(500) / 8
	permutations, _ := math.Lgamma(501)
	dictionary := (500*(128+math.Log2(15)) - permutations/math.Ln2) / 8 / n
	attributes := (math.Log2(1920) + math.Log2(1080) + 2 + math.Log2(3) -
		flag*math.Log2(flag) - (1-flag)*math.Log2(1-flag)) / 8
	rest := contexts + dictionary + attributes
	t.Logf("time arrival=%.4f sorted_rice=%.4f context_ids=%.4f dictionary=%.4f attributes=%.4f",
		arrival, sorted, contexts, dictionary, attributes)
	t.Logf("floor arrival_order=%.4f time_order=%.4f B/record", arrival+rest, sorted+rest)
}

// TestRecordV2LateRecords shifts one record in a hundred up to ten minutes into the past,
// as clients that were offline deliver them, and counts what one-second reads pay for it
func TestRecordV2LateRecords(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1 to measure late records")
	}
	for _, mode := range []string{"on time", "late", "late, own head"} {
		random := rand.New(rand.NewPCG(29, 31))
		events := recordFixture("frontend", 1_000_000)
		if mode != "on time" {
			for i := range events {
				if random.IntN(100) == 0 {
					events[i].at -= random.Int64N(int64(10 * time.Minute))
				}
			}
		}
		var measured v2Measured
		if mode == "late, own head" {
			measured = v2MeasureBatches(t, v2LateHead(v2Batches(events), int64(time.Minute)))
		} else {
			measured = v2Measure(t, events, false)
		}
		db, err := v2OpenStore(t.Context(), filepath.Join(t.TempDir(), "late.db"))
		if err != nil {
			t.Fatal(err)
		}
		if err = v2WriteSegments(t.Context(), db, measured.segments); err != nil {
			t.Fatal(err)
		}
		var widest, blocks int64
		const spans = `select max(last_at - first_at), count(*) from blocks`
		if err = db.QueryRowContext(t.Context(), spans).Scan(&widest, &blocks); err != nil {
			t.Fatal(err)
		}
		read, rows := 0, 0
		_, decoder := v2TestCodec(t)
		for i := range 100 {
			from := events[10_000+i*9_000].at
			reader := &v2Reader{db: db, decoder: decoder, schemas: map[int64]*v2Schema{}}
			got, err := reader.readRange(t.Context(), from, from+1e9)
			if err != nil {
				t.Fatal(err)
			}
			read, rows = read+reader.blocks, rows+len(got)
		}
		db.Close()
		t.Logf("%-15s blocks=%d widest_block=%v blocks_per_second_read=%.2f rows_per_read=%.0f payload=%.4f B/record",
			mode, blocks, time.Duration(widest), float64(read)/100, float64(rows)/100,
			float64(measured.payload)/float64(len(events)))
	}
}

// TestRecordV2SegmentMemory bounds what encoding and decoding one full segment allocate,
// the figure a memory reservation has to cover
func TestRecordV2SegmentMemory(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1 to measure segment memory")
	}
	encoder, decoder := v2TestCodec(t)
	for _, name := range []string{"frontend", "backend", "noise"} {
		events := v2Batches(recordFixture(name, v2SegmentEvents))[0]
		raw := 0
		for i := range events {
			raw += v2EventBytes(&events[i])
		}
		var before, after runtime.MemStats
		runtime.GC()
		runtime.ReadMemStats(&before)
		segment, _, err := encoder.encodeSegment(events, false)
		runtime.ReadMemStats(&after)
		if err != nil {
			t.Fatal(err)
		}
		encoded := after.TotalAlloc - before.TotalAlloc
		runtime.ReadMemStats(&before)
		schema, err := decoder.decodeSchema(segment.row)
		if err != nil {
			t.Fatal(err)
		}
		for _, block := range segment.blocks {
			if _, err = decoder.decodeBlock(&schema, block.body); err != nil {
				t.Fatal(err)
			}
		}
		runtime.ReadMemStats(&after)
		t.Logf("%-8s records=%d input=%.1f MiB encode_alloc=%.1f MiB decode_alloc=%.1f MiB", name, len(events),
			float64(raw)/(1<<20), float64(encoded)/(1<<20), float64(after.TotalAlloc-before.TotalAlloc)/(1<<20))
	}
}

// v2LateHead moves what a batch holds from more than margin before its median into a
// head of its own, sealed in event-time order like any other
func v2LateHead(batches [][]recordEvent, margin int64) [][]recordEvent {
	var onTime [][]recordEvent
	var late []recordEvent
	for _, batch := range batches {
		times := make([]int64, len(batch))
		for i := range batch {
			times[i] = batch[i].at
		}
		slices.Sort(times)
		median := times[len(times)/2]
		var kept []recordEvent
		for _, event := range batch {
			if event.at < median-margin {
				late = append(late, event)
			} else {
				kept = append(kept, event)
			}
		}
		onTime = append(onTime, kept)
	}
	return append(onTime, v2Batches(late)...)
}

func v2MeasureBatches(t testing.TB, batches [][]recordEvent) v2Measured {
	t.Helper()
	var measured v2Measured
	for _, batch := range batches {
		part := v2Measure(t, batch, false)
		measured.segments = append(measured.segments, part.segments...)
		measured.payload += part.payload
		measured.blooms += part.blooms
		measured.encode += part.encode
	}
	return measured
}
