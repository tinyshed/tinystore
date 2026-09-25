package spike

import (
	"bytes"
	"context"
	"crypto/sha256"
	"database/sql"
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"math/rand/v2"
	"os"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"testing"

	"github.com/klauspost/compress/zstd"
)

func recordJSON(value any) string {
	encoded, err := json.Marshal(value)
	if err != nil {
		panic(err)
	}
	return string(encoded)
}

func recordClick(index int, churn bool) recordEvent {
	session := index / 100
	if churn {
		session = index
	}
	context := []recordField{
		{"browser", recordJSON([]string{"Chrome", "Safari", "Firefox"}[session%3])},
		{"browser_version", recordJSON(strconv.Itoa(150 + session%5))},
		{"viewport_w", strconv.Itoa([]int{1920, 1366, 390}[session%3])},
		{"viewport_h", strconv.Itoa([]int{1080, 768, 844}[session%3])},
		{"session_id", recordJSON(fmt.Sprintf("aabbccdd-0000-4000-8000-%012x", session))},
		{"sdk", `"web/1.2.0"`},
	}
	attrs := []recordField{
		{"element", recordJSON([]string{"buy", "save", "cancel", "menu"}[index%4])},
		{"url", recordJSON([]string{"/catalog", "/checkout", "/profile"}[index%3])},
		{"x", strconv.Itoa(index * 37 % 1920)},
		{"y", strconv.Itoa(index * 19 % 1080)},
	}
	if index%17 == 0 {
		attrs = append(attrs, recordField{"experiment", `"button-b"`})
	}
	return recordEvent{
		at:     1_790_000_000_000_000_000 + int64(index)*1_234_567,
		stream: "frontend", name: "ui.click", context: context, attrs: attrs,
	}
}

func recordBackend(random *rand.Rand, index int) recordEvent {
	trace := make([]byte, 16)
	for i := range trace {
		trace[i] = byte(random.Uint32())
	}
	level := int64(0)
	body := "request finished"
	if index%16 == 0 {
		level, body = 8, "upstream refused connection"
	}
	return recordEvent{
		at:     1_790_000_000_000_000_000 + int64(index)*1_000_003,
		stream: "backend", name: "http.request", level: &level, body: &body, traceID: trace,
		context: []recordField{{"service", `"api"`}, {"environment", `"production"`}, {"version", `"1.2.3"`}},
		attrs: []recordField{
			{"method", `"GET"`},
			{"path", recordJSON([]string{"/notes", "/users", "/health"}[index%3])},
			{"status", strconv.Itoa([]int{200, 200, 200, 404, 500}[random.IntN(5)])},
			{"duration_ms", strconv.Itoa(random.IntN(400))},
			{"user_id", strconv.Itoa(random.IntN(5000))},
		},
	}
}

func recordRandomClick(random *rand.Rand, index int, churn bool) recordEvent {
	session := random.IntN(500)
	if churn {
		session = index
	}
	event := recordClick(session*100, false)
	digest := sha256.Sum256([]byte(fmt.Sprintf("session:%d", session)))
	event.context[4].value = recordJSON(fmt.Sprintf("%x-%x-%x-%x-%x",
		digest[:4], digest[4:6], digest[6:8], digest[8:10], digest[10:16]))
	event.at = 1_790_000_000_000_000_000 + int64(index)*1_234_567 + int64(random.IntN(1_000_000_000))
	event.attrs = []recordField{
		{"element", recordJSON([]string{"buy", "save", "cancel", "menu"}[random.IntN(4)])},
		{"url", recordJSON([]string{"/catalog", "/checkout", "/profile"}[random.IntN(3)])},
		{"x", strconv.Itoa(random.IntN(1920))},
		{"y", strconv.Itoa(random.IntN(1080))},
	}
	if random.IntN(17) == 0 {
		event.attrs = append(event.attrs, recordField{"experiment", `"button-b"`})
	}
	return event
}

func recordFixture(kind string, count int) []recordEvent {
	random := rand.New(rand.NewPCG(17, 23))
	events := make([]recordEvent, count)
	for i := range events {
		switch kind {
		case "regular_frontend":
			events[i] = recordClick(i, false)
		case "frontend", "context_churn":
			events[i] = recordRandomClick(random, i, kind == "context_churn")
		case "backend":
			events[i] = recordBackend(random, i)
		case "derived_fields":
			user := random.IntN(1_000_000)
			events[i] = recordEvent{
				at: int64(i), stream: "backend", name: "user.loaded",
				body: recordPointer(fmt.Sprintf("loaded user %d", user)),
				attrs: []recordField{
					{"user_id", strconv.Itoa(user)},
					{"route", recordJSON(fmt.Sprintf("/users/%d", user))},
					{"url", recordJSON(fmt.Sprintf("https://example.test/users/%d", user))},
				},
			}
			if i%97 == 0 {
				events[i].body = recordPointer("user lookup failed")
			}
		case "mixed":
			switch i % 3 {
			case 0:
				events[i] = recordBackend(random, i)
			case 1:
				events[i] = recordRandomClick(random, i, false)
			case 2:
				events[i] = recordEvent{
					at: int64(i), stream: "java", name: "log",
					body: recordPointer(fmt.Sprintf("worker %d failed\n\tat Example.java:%d\n", i%8, i%200)),
				}
			}
		case "shape_churn":
			events[i] = recordEvent{
				at: int64(i), stream: "webhook", name: "external.event",
				attrs: []recordField{{fmt.Sprintf("field_%d", i), strconv.Itoa(i)}},
			}
		case "noise":
			var randomBytes [128]byte
			for j := range randomBytes {
				randomBytes[j] = byte(random.Uint32())
			}
			events[i] = recordEvent{at: int64(i), name: "opaque", body: recordPointer(hex.EncodeToString(randomBytes[:]))}
		}
	}
	return events
}

type recordMeasuredBlock struct {
	first, last int64
	count       int
	baseline    []byte
	adaptive    []byte
}

type recordDensityResult struct {
	name                        string
	count, raw, base, packed    int
	baseFile, packedFile        int64
	rows, columns, tuples       int
	envelope, directory, values int
	withoutPrediction, segment  int
	segmentFile                 int64
	sharedSegments              int
	segmentRows                 int
	segmentRowsFile             int64
}

func measureRecordBlocks(t *testing.T, events []recordEvent) []recordMeasuredBlock {
	t.Helper()
	codec := recordTestCodec(t)
	stream := recordStream{codec: codec}
	var blocks []recordMeasuredBlock
	start := 0
	finish := func(end int, body []byte) {
		batch := events[start:end]
		baseline, err := codec.baseline(batch)
		if err != nil {
			t.Fatal(err)
		}
		if len(body) > len(baseline) {
			t.Fatal("adaptive block exceeded its complete baseline")
		}
		decoded, err := codec.decode(body)
		if err != nil {
			t.Fatal(err)
		}
		assertRecordEvents(t, batch, decoded)
		first, last := batch[0].at, batch[0].at
		for _, event := range batch {
			first, last = min(first, event.at), max(last, event.at)
		}
		blocks = append(blocks, recordMeasuredBlock{first, last, len(batch), baseline, body})
		start = end
	}
	for i, event := range events {
		block, err := stream.add(event)
		if err != nil {
			t.Fatal(err)
		}
		if block != nil {
			finish(i, block)
		}
	}
	last, err := stream.flush()
	if err != nil {
		t.Fatal(err)
	}
	if last != nil {
		finish(len(events), last)
	}
	return blocks
}

func measureRecordFile(ctx context.Context, path string, blocks []recordMeasuredBlock, adaptive bool) (int64, string, error) {
	db, err := sql.Open("sqlite", path)
	if err != nil {
		return 0, "", err
	}
	defer db.Close()
	const schema = `create table blocks (id integer primary key, first_at integer not null,
		last_at integer not null, count integer not null, body blob not null) strict;
		create index blocks_at on blocks(first_at);`
	if _, err = db.ExecContext(ctx, schema); err != nil {
		return 0, "", err
	}
	tx, err := db.BeginTx(ctx, nil)
	if err != nil {
		return 0, "", err
	}
	defer tx.Rollback()
	const insert = `insert into blocks(first_at,last_at,count,body) values (?,?,?,?)`
	for _, block := range blocks {
		body := block.baseline
		if adaptive {
			body = block.adaptive
		}
		if _, err = tx.ExecContext(ctx, insert, block.first, block.last, block.count, body); err != nil {
			return 0, "", err
		}
	}
	if err = tx.Commit(); err != nil {
		return 0, "", err
	}
	if _, err = db.ExecContext(ctx, `vacuum`); err != nil {
		return 0, "", err
	}
	var size int64
	if err = db.QueryRowContext(ctx, `select page_count*page_size from pragma_page_count,pragma_page_size`).Scan(&size); err != nil {
		return 0, "", err
	}
	objects, err := recordFileObjects(ctx, db)
	return size, objects, err
}

func recordFileObjects(ctx context.Context, db *sql.DB) (string, error) {
	const query = `select name,sum(pgsize),sum(payload),sum(unused) from dbstat group by name order by name`
	rows, err := db.QueryContext(ctx, query)
	if err != nil {
		return "", err
	}
	defer rows.Close()
	var report []string
	for rows.Next() {
		var name string
		var pages, payload, unused int64
		if err = rows.Scan(&name, &pages, &payload, &unused); err != nil {
			return "", err
		}
		report = append(report, fmt.Sprintf("%s=%d/%d/%d", name, pages, payload, unused))
	}
	return strings.Join(report, " "), rows.Err()
}

func (r *recordDensityResult) countBlock(codec *recordBlockCodec, block []byte) {
	cursor := recordCursor{data: block[4 : len(block)-4]}
	mode := cursor.number(1)
	cursor.number(recordEventLimit)
	cursor.number(recordByteLimit)
	r.envelope += len(block) - len(cursor.data)
	if mode == 0 {
		r.rows++
		r.values += len(cursor.data)
		return
	}
	r.columns++
	before := len(cursor.data)
	metadata := codec.unpack(&cursor)
	r.directory += before - len(cursor.data)
	r.values += len(cursor.data)
	if len(metadata) > 0 && metadata[0] == 1 {
		r.tuples++
	}
}

func measureRecordDensity(t *testing.T, name string, events []recordEvent) recordDensityResult {
	t.Helper()
	result := recordDensityResult{name: name, count: len(events)}
	blocks := measureRecordBlocks(t, events)
	codec := recordTestCodec(t)
	for _, event := range events {
		result.raw += len(appendRecordEvent(nil, event))
	}
	for _, block := range blocks {
		result.base += len(block.baseline)
		result.packed += len(block.adaptive)
		result.countBlock(codec, block.adaptive)
	}
	start := 0
	codec.predict = false
	for _, block := range blocks {
		body, encodeErr := codec.encode(events[start : start+block.count])
		if encodeErr != nil {
			t.Fatal(encodeErr)
		}
		result.withoutPrediction += len(body)
		start += block.count
	}
	codec.predict = true
	segments := measureRecordSegments(t, events, blocks)
	for _, segment := range segments {
		result.segment += len(segment.adaptive)
		result.segmentRows += len(segment.baseline)
		if segment.adaptive[3] == 1 {
			result.sharedSegments++
		}
	}
	if result.envelope+result.directory+result.values != result.packed {
		t.Fatal("payload byte accounting does not add up")
	}
	var err error
	var baseObjects, packedObjects string
	result.baseFile, baseObjects, err = measureRecordFile(t.Context(), filepath.Join(t.TempDir(), "base.db"), blocks, false)
	if err != nil {
		t.Fatal(err)
	}
	result.packedFile, packedObjects, err = measureRecordFile(t.Context(), filepath.Join(t.TempDir(), "adaptive.db"), blocks, true)
	if err != nil {
		t.Fatal(err)
	}
	t.Logf("dbstat %s baseline %s", name, baseObjects)
	t.Logf("dbstat %s adaptive %s", name, packedObjects)
	result.segmentFile, packedObjects, err = measureRecordFile(t.Context(), filepath.Join(t.TempDir(), "segments.db"), segments, true)
	if err != nil {
		t.Fatal(err)
	}
	t.Logf("dbstat %s segments %s", name, packedObjects)
	result.segmentRowsFile, baseObjects, err = measureRecordFile(t.Context(), filepath.Join(t.TempDir(), "segment-rows.db"), segments, false)
	if err != nil {
		t.Fatal(err)
	}
	t.Logf("dbstat %s segment_rows %s", name, baseObjects)
	return result
}

func measureRecordSegments(t *testing.T, events []recordEvent, blocks []recordMeasuredBlock) []recordMeasuredBlock {
	t.Helper()
	codec := recordTestCodec(t)
	writer, err := zstd.NewWriter(nil, zstd.WithEncoderConcurrency(1), zstd.WithEncoderLevel(zstd.SpeedDefault),
		zstd.WithWindowSize(recordSegmentBlocks*recordByteLimit), zstd.WithLowerEncoderMem(true))
	if err != nil {
		t.Fatal(err)
	}
	defer writer.Close()
	reader, err := zstd.NewReader(nil, zstd.WithDecoderConcurrency(1),
		zstd.WithDecoderMaxWindow(recordSegmentBlocks*recordByteLimit), zstd.WithDecoderMaxMemory(recordWorkLimit))
	if err != nil {
		t.Fatal(err)
	}
	defer reader.Close()
	var segments []recordMeasuredBlock
	offset := 0
	for start := 0; start < len(blocks); start += recordSegmentBlocks {
		group := blocks[start:min(start+recordSegmentBlocks, len(blocks))]
		var batches [][]recordEvent
		first, last, count := group[0].first, group[0].last, 0
		for _, block := range group {
			batches = append(batches, events[offset+count:offset+count+block.count])
			count += block.count
			first, last = min(first, block.first), max(last, block.last)
		}
		body, err := codec.encodeRecordSegment(batches)
		if err != nil {
			t.Fatal(err)
		}
		decoded, err := codec.decodeRecordSegment(body)
		if err != nil {
			t.Fatal(err)
		}
		assertRecordEvents(t, events[offset:offset+count], decoded)
		var raw []byte
		for _, event := range events[offset : offset+count] {
			raw = appendRecordEvent(raw, event)
		}
		packed := writer.EncodeAll(raw, nil)
		unpacked, unpackErr := reader.DecodeAll(packed, nil)
		if unpackErr != nil || !bytes.Equal(unpacked, raw) {
			t.Fatal("whole-segment zstd did not preserve rows", unpackErr)
		}
		baseline := recordSegmentEnvelope(2, append(binary.AppendUvarint(nil, uint64(count)), packed...))
		segments = append(segments, recordMeasuredBlock{first: first, last: last, count: count, baseline: baseline, adaptive: body})
		offset += count
	}
	return segments
}

func TestAdaptiveRecordDensity(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1 to measure normalized record blocks")
	}
	var results []recordDensityResult
	for _, name := range []string{"regular_frontend", "frontend", "backend", "mixed", "context_churn", "derived_fields", "shape_churn", "noise"} {
		results = append(results, measureRecordDensity(t, name, recordFixture(name, 10_000)))
	}
	if corpus := os.Getenv("TINYSTORE_LOGHUB"); corpus != "" {
		results = append(results, measureRecordLoghub(t, corpus)...)
	}
	t.Log("dataset records raw row_zstd no_predict adaptive segment_rows segment row_file adaptive_file segment_rows_file segment_file rows/columns/tuples shared_segments envelope/directory/values")
	for _, result := range results {
		per := func(n int) float64 { return float64(n) / float64(result.count) }
		t.Logf("%-16s %6d %7.2f %7.2f %7.2f %7.2f %7.2f %7.2f %7.2f %7.2f %7.2f %7.2f %d/%d/%d %d %.3f/%.3f/%.3f", result.name, result.count,
			per(result.raw), per(result.base), per(result.withoutPrediction), per(result.packed), per(result.segmentRows), per(result.segment),
			per(int(result.baseFile)), per(int(result.packedFile)), per(int(result.segmentRowsFile)), per(int(result.segmentFile)),
			result.rows, result.columns, result.tuples, result.sharedSegments, per(result.envelope), per(result.directory), per(result.values))
	}
}

func measureRecordLoghub(t *testing.T, corpus string) []recordDensityResult {
	t.Helper()
	paths, err := filepath.Glob(filepath.Join(corpus, "*", "*_2k.log"))
	if err != nil || len(paths) == 0 {
		t.Fatalf("no Loghub samples: %v", err)
	}
	var results []recordDensityResult
	for _, path := range paths {
		data, readErr := os.ReadFile(path)
		if readErr != nil {
			t.Fatal(readErr)
		}
		lines := bytes.Split(bytes.TrimSuffix(data, []byte{'\n'}), []byte{'\n'})
		events := make([]recordEvent, len(lines))
		name := filepath.Base(filepath.Dir(path))
		for i, line := range lines {
			events[i] = recordEvent{at: int64(i), stream: name, name: "log", body: recordPointer(string(line))}
		}
		results = append(results, measureRecordDensity(t, name, events))
	}
	return results
}

func TestRecordShapesCanFallBackAndContextsCanCompete(t *testing.T) {
	codec := recordTestCodec(t)
	for _, kind := range []string{"frontend", "backend", "mixed", "context_churn", "shape_churn", "noise"} {
		events := recordFixture(kind, 300)
		blocks := measureRecordBlocks(t, events)
		if len(blocks) != 1 {
			t.Fatal("unexpected fixture block boundary")
		}
		if kind == "shape_churn" && blocks[0].adaptive[4] != 0 {
			t.Fatal("shape ceiling did not choose raw rows")
		}
		for _, tuple := range []bool{false, true} {
			layout, ok := planRecordColumns(events, tuple, true)
			if !ok {
				continue
			}
			raw, err := rawRecordEvents(events)
			if err != nil {
				t.Fatal(err)
			}
			body := recordEnvelope(1, len(events), len(raw), codec.encodeRecordLayout(layout))
			got, err := codec.decode(body)
			if err != nil {
				t.Fatal(err)
			}
			assertRecordEvents(t, events, got)
		}
	}
	columns := [][]string{{"one", "two", "one"}, {"one", "two", "one"}}
	if !slices.Equal(codec.encodeColumn(columns[0]), codec.encodeColumn(columns[1])) {
		t.Fatal("equal columns have different encodings")
	}
}
