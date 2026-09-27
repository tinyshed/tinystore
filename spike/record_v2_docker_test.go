package spike

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"maps"
	"math"
	"os"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/klauspost/compress/zstd"
)

// a docker json-file corpus as bench/fetch-docker-logs.sh leaves it:
//
//	<corpus>/<host>/containers.txt    /name|image|log path
//	<corpus>/<host>/raw/<id>/<id>-json.log, .1, .2 …
//
// every entry becomes one record: docker's receive time, its stream as the event
// name, and the line as attributes when it is JSON that rebuilds byte for byte
type v2DockerLine struct {
	at     int64
	stream string
	text   string
}

type v2DockerContainer struct {
	name       string
	raw        int
	unreadable int
	lines      []v2DockerLine
}

func v2DockerFiles(dir string) []string {
	files, _ := filepath.Glob(filepath.Join(dir, "*-json.log*"))
	rotation := func(path string) int {
		if at := strings.LastIndex(path, ".log."); at >= 0 {
			value, _ := strconv.Atoi(path[at+5:])
			return value
		}
		return 0
	}
	slices.SortFunc(files, func(a, b string) int { return rotation(b) - rotation(a) })
	return files
}

func readDockerCorpus(t *testing.T, root string) []v2DockerContainer {
	t.Helper()
	hosts, err := os.ReadDir(root)
	if err != nil {
		t.Fatal(err)
	}
	var containers []v2DockerContainer
	for _, host := range hosts {
		names := map[string]string{}
		data, err := os.ReadFile(filepath.Join(root, host.Name(), "containers.txt"))
		if err != nil {
			continue
		}
		for line := range strings.SplitSeq(strings.TrimSpace(string(data)), "\n") {
			parts := strings.Split(strings.TrimSpace(line), "|")
			if len(parts) == 3 {
				names[filepath.Base(filepath.Dir(parts[2]))] = host.Name() + "/" + strings.TrimPrefix(parts[0], "/")
			}
		}
		for id, name := range names {
			container, err := readDockerContainer(filepath.Join(root, host.Name(), "raw", id), name)
			if err != nil {
				t.Fatal(name, err)
			}
			if len(container.lines) > 0 {
				containers = append(containers, container)
			}
		}
	}
	slices.SortFunc(containers, func(a, b v2DockerContainer) int { return strings.Compare(a.name, b.name) })
	return containers
}

// readDockerContainer joins the pieces docker splits lines longer than 16 KiB into
func readDockerContainer(dir, name string) (v2DockerContainer, error) {
	container := v2DockerContainer{name: name}
	var pending strings.Builder
	for _, path := range v2DockerFiles(dir) {
		file, err := os.Open(path)
		if err != nil {
			return container, err
		}
		scanner := bufio.NewScanner(file)
		scanner.Buffer(make([]byte, 64<<10), 16<<20)
		for scanner.Scan() {
			container.raw += len(scanner.Bytes()) + 1
			// an unclean shutdown can leave NUL padding where docker was writing
			var entry struct{ Log, Stream, Time string }
			if json.Unmarshal(scanner.Bytes(), &entry) != nil {
				container.unreadable++
				continue
			}
			pending.WriteString(entry.Log)
			if !strings.HasSuffix(entry.Log, "\n") {
				continue
			}
			at, parseErr := time.Parse(time.RFC3339Nano, entry.Time)
			if parseErr != nil {
				container.unreadable++
				pending.Reset()
				continue
			}
			text := strings.TrimSuffix(pending.String(), "\n")
			container.lines = append(container.lines, v2DockerLine{at.UnixNano(), entry.Stream, text})
			pending.Reset()
		}
		file.Close()
		if err = scanner.Err(); err != nil {
			return container, err
		}
	}
	return container, nil
}

func v2ExactJSON(text string) ([]recordField, bool) {
	if len(text) < 2 || text[0] != '{' {
		return nil, false
	}
	fields, err := parseRecordFields([]byte(text))
	if err != nil || len(fields) == 0 {
		return nil, false
	}
	var rebuilt strings.Builder
	rebuilt.WriteByte('{')
	for i, field := range fields {
		if i > 0 {
			rebuilt.WriteByte(',')
		}
		key, _ := json.Marshal(field.key)
		rebuilt.Write(key)
		rebuilt.WriteByte(':')
		rebuilt.WriteString(field.value)
	}
	rebuilt.WriteByte('}')
	return fields, rebuilt.String() == text
}

// v2PinoEvent reads a pino line as an embedded store would get it: application
// time, level and message, the other fields as attributes, no receive time
//
//	{"level":40,"time":1758000000123,"msg":"slow"} → at 1758000000123000000, level 4, body "slow"
func v2PinoEvent(event recordEvent, fields []recordField) (recordEvent, bool) {
	var rest []recordField
	found := 0
	for _, field := range fields {
		number, isInt := v2ParseInt(field.value)
		switch {
		case field.key == "level" && isInt && number%10 == 0 && number >= 10 && number <= 60:
			level := (number - 30) * 4 / 10
			event.level = &level
		case field.key == "time" && isInt && number < 1<<53:
			event.at = number * 1_000_000
		case field.key == "msg" && json.Unmarshal([]byte(field.value), new(string)) == nil:
			var message string
			_ = json.Unmarshal([]byte(field.value), &message)
			event.body = &message
		default:
			rest = append(rest, field)
			continue
		}
		found++
	}
	event.attrs, event.name = rest, "log"
	return event, found == 3
}

type v2DockerKind int

const (
	v2DockerText v2DockerKind = iota
	v2DockerJSON
	v2DockerPino
)

// v2DockerEvents normalizes one container; embedded reads pino lines as slog would deliver them
func v2DockerEvents(container *v2DockerContainer, embedded bool) ([]recordEvent, v2DockerKind, int) {
	events := make([]recordEvent, 0, len(container.lines))
	kinds := map[v2DockerKind]int{}
	oversized := 0
	for _, line := range container.lines {
		event := recordEvent{at: line.at, stream: container.name, name: line.stream}
		kind := v2DockerText
		if fields, ok := v2ExactJSON(line.text); ok {
			event.attrs, kind = fields, v2DockerJSON
			if pino, isPino := v2PinoEvent(event, fields); isPino {
				kind = v2DockerPino
				if embedded {
					event = pino
				}
			}
		} else {
			text := line.text
			event.body = &text
		}
		if checkRecordEvent(event) != nil || v2EventBytes(&event) > recordByteLimit {
			oversized++
			continue
		}
		kinds[kind]++
		events = append(events, event)
	}
	dominant := v2DockerText
	for kind, count := range kinds {
		if count > kinds[dominant] {
			dominant = kind
		}
	}
	return events, dominant, oversized
}

var errV2NoCorpus = errors.New("no docker corpus")

func v2DockerCorpus(t *testing.T) []v2DockerContainer {
	t.Helper()
	root := os.Getenv("TINYSTORE_RECORD_DOCKER")
	if os.Getenv("TINYSTORE_SPIKE") == "" || root == "" {
		t.Skip("set TINYSTORE_SPIKE=1 and TINYSTORE_RECORD_DOCKER=<corpus> to measure container logs")
	}
	containers := readDockerCorpus(t, root)
	if len(containers) == 0 {
		t.Fatal(errV2NoCorpus)
	}
	return containers
}

type v2DockerGroup struct {
	name                       string
	containers, records, lines int
	content, stamped, payload  int
	zstdContent, zstdStamped   int
	encode                     time.Duration
}

func (g *v2DockerGroup) log(t *testing.T) {
	t.Helper()
	n := float64(max(g.records, 1))
	t.Logf("%-20s containers=%2d records=%8d content=%7.1f zstd_content=%6.2f zstd_stamped=%6.2f v2=%6.2f"+
		" encode=%.0f/s", g.name, g.containers, g.records, float64(g.content)/n, float64(g.zstdContent)/n,
		float64(g.zstdStamped)/n, float64(g.payload)/n, n/g.encode.Seconds())
}

// v2ZstdLines compresses lines in chunks of one segment, as a log file compressed per segment
func v2ZstdLines(t testing.TB, lines []string) int {
	t.Helper()
	encoder, err := zstd.NewWriter(nil, zstd.WithEncoderConcurrency(1), zstd.WithEncoderLevel(zstd.SpeedDefault),
		zstd.WithWindowSize(recordSegmentMaxBlocks*recordByteLimit))
	if err != nil {
		t.Fatal(err)
	}
	defer encoder.Close()
	total := 0
	for start := 0; start < len(lines); start += v2SegmentEvents {
		var chunk []byte
		for _, line := range lines[start:min(start+v2SegmentEvents, len(lines))] {
			chunk = append(append(chunk, line...), '\n')
		}
		total += len(encoder.EncodeAll(chunk, nil))
	}
	return total
}

func v2DockerMeasure(t *testing.T, group *v2DockerGroup, container *v2DockerContainer, events []recordEvent) []v2Segment {
	t.Helper()
	measured := v2Measure(t, events, false)
	content, stamped := make([]string, len(container.lines)), make([]string, len(container.lines))
	for i, line := range container.lines {
		content[i] = line.text
		stamped[i] = time.Unix(0, line.at).UTC().Format(time.RFC3339Nano) + " " + line.stream + " " + line.text
		group.content += len(line.text) + 1
		group.stamped += len(stamped[i]) + 1
	}
	group.containers++
	group.records += len(events)
	group.lines += len(container.lines)
	group.payload += measured.payload + measured.blooms
	group.zstdContent += v2ZstdLines(t, content)
	group.zstdStamped += v2ZstdLines(t, stamped)
	group.encode += measured.encode
	return measured.segments
}

func TestRecordV2DockerLogs(t *testing.T) {
	containers := v2DockerCorpus(t)
	names := map[v2DockerKind]string{v2DockerPino: "pino JSON", v2DockerJSON: "other JSON", v2DockerText: "text"}
	groups := map[v2DockerKind]*v2DockerGroup{}
	all, embedded := &v2DockerGroup{name: "all"}, &v2DockerGroup{name: "pino as slog"}
	var segments, pino, text []v2Segment
	var everything []recordEvent
	pinoRecords, textRecords := 0, 0
	oversized, unreadable := 0, 0
	for i := range containers {
		container := &containers[i]
		unreadable += container.unreadable
		events, kind, skipped := v2DockerEvents(container, false)
		oversized += skipped
		if groups[kind] == nil {
			groups[kind] = &v2DockerGroup{name: names[kind]}
		}
		segments = append(segments, v2DockerMeasure(t, groups[kind], container, events)...)
		v2DockerMeasure(t, all, container, events)
		everything = append(everything, events...)
		if kind == v2DockerPino {
			slog, _, _ := v2DockerEvents(container, true)
			pino = append(pino, v2DockerMeasure(t, embedded, container, slog)...)
			pinoRecords += len(slog)
		}
		if kind == v2DockerText {
			text = append(text, segments[len(segments)-len(v2Batches(events)):]...)
			textRecords += len(events)
		}
	}
	t.Logf("%d containers, %d lines, %d records, %d too large for one block, %d unreadable docker entries",
		len(containers), all.lines, all.records, oversized, unreadable)
	for _, kind := range []v2DockerKind{v2DockerPino, v2DockerJSON, v2DockerText} {
		if groups[kind] != nil {
			groups[kind].log(t)
		}
	}
	all.log(t)
	embedded.log(t)
	t.Log("pino as slog, by column:")
	v2Census(t, pino, pinoRecords)
	t.Log("text, by column:")
	v2Census(t, text, textRecords)
	v2DockerStore(t, segments, everything)
}

func v2DockerStore(t *testing.T, segments []v2Segment, events []recordEvent) {
	t.Helper()
	db, err := v2OpenStore(t.Context(), filepath.Join(t.TempDir(), "records.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	if err = v2WriteSegments(t.Context(), db, segments); err != nil {
		t.Fatal(err)
	}
	size, objects, err := v2StoreSize(t.Context(), db)
	if err != nil {
		t.Fatal(err)
	}
	t.Logf("file B/record=%.4f %s", float64(size)/float64(len(events)), objects)
	for _, query := range v2DockerQueries(events) {
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

// v2DockerQueries asks what an operator would: the busiest minute, one request, every error
func v2DockerQueries(events []recordEvent) []v2Query {
	minutes := map[int64]int{}
	var request recordField
	for i := range events {
		minutes[events[i].at/60e9]++
		for _, field := range events[i].attrs {
			if field.key == "requestId" && request.key == "" && i > len(events)/2 {
				request = field
			}
		}
	}
	busiest := int64(0)
	for minute, count := range minutes {
		if count > minutes[busiest] || (count == minutes[busiest] && minute < busiest) {
			busiest = minute
		}
	}
	from, to := busiest*60e9, (busiest+1)*60e9
	errorLevel := recordField{"level", "50"}
	queries := []v2Query{{"busiest minute", from, to, func(r *v2Reader) ([]recordEvent, error) {
		return r.readRange(context.Background(), from, to)
	}, func(*recordEvent) bool { return true }}}
	for _, field := range []recordField{request, errorLevel} {
		if field.key == "" {
			continue
		}
		queries = append(queries, v2Query{
			"one " + field.key + " value, all time", math.MinInt64,
			math.MaxInt64, func(r *v2Reader) ([]recordEvent, error) {
				return r.readAttr(context.Background(), field, math.MinInt64, math.MaxInt64)
			}, func(e *recordEvent) bool { return slices.Contains(e.attrs, field) },
		})
	}
	return queries
}

// v2Census divides segment payload by slot, naming attribute columns by their key
func v2Census(t *testing.T, segments []v2Segment, records int) {
	t.Helper()
	_, decoder := v2TestCodec(t)
	costs := map[string]int{}
	for _, segment := range segments {
		schema, err := decoder.decodeSchema(segment.row)
		if err != nil {
			t.Fatal(err)
		}
		costs["segment rows"] += len(segment.row)
		names := map[int]string{}
		for _, shape := range schema.shapes {
			for position, column := range shape.columns {
				names[column] = "attr " + shape.keys[position]
			}
		}
		for _, block := range segment.blocks {
			opened, err := decoder.openBlock(&schema, block.body)
			if err != nil {
				t.Fatal(err)
			}
			costs["block headers"] += len(block.body)
			for i, slot := range schema.slots {
				name := map[byte]string{
					v2SlotTime: "time", v2SlotName: "name", v2SlotShape: "shape", v2SlotContext: "context",
					v2SlotLevel: "level", v2SlotBody: "body", v2SlotTrace: "trace", v2SlotSpan: "span", v2SlotRaw: "raw attrs",
				}[slot.kind]
				if slot.kind == v2SlotAttr {
					name = names[slot.column]
				}
				costs[name] += len(opened.payloads[i])
				costs["block headers"] -= len(opened.payloads[i])
			}
		}
	}
	keys := slices.SortedFunc(maps.Keys(costs), func(a, b string) int { return costs[b] - costs[a] })
	for _, key := range keys[:min(12, len(keys))] {
		t.Logf("  %-28s %7.3f B/record", key, float64(costs[key])/float64(records))
	}
}

// TestRecordV2DockerTextScope divides what text bodies cost by compression scope: the
// whole segment, one block, one block with the segment's sample, and the v2 text column
func TestRecordV2DockerTextScope(t *testing.T) {
	containers := v2DockerCorpus(t)
	encoder, _ := v2TestCodec(t)
	plain, err := zstd.NewWriter(nil, zstd.WithEncoderConcurrency(1), zstd.WithEncoderLevel(zstd.SpeedDefault),
		zstd.WithWindowSize(4<<20))
	if err != nil {
		t.Fatal(err)
	}
	defer plain.Close()
	var records int
	costs := map[string]int{}
	for i := range containers {
		events, kind, _ := v2DockerEvents(&containers[i], false)
		if kind != v2DockerText {
			continue
		}
		for _, batch := range v2Batches(events) {
			var texts []recordEvent
			var bodies []string
			for _, event := range batch {
				if event.body != nil {
					texts, bodies = append(texts, event), append(bodies, *event.body)
				}
			}
			records += len(bodies)
			v2TextScope(t, encoder, plain, bodies, v2TextSample(texts, v2TextDictionary), costs)
		}
	}
	for _, name := range []string{"zstd, segment", "zstd, block", "zstd, block and sample", "v2 column", "v2 column and sample"} {
		t.Logf("%-24s %6.2f B/record of %d text records", name, float64(costs[name])/float64(records), records)
	}
}

func v2TextScope(t *testing.T, encoder *v2Encoder, plain *zstd.Encoder, bodies []string, sample []byte, costs map[string]int) {
	t.Helper()
	joined := func(values []string) []byte {
		var out []byte
		for _, value := range values {
			out = append(append(out, value...), '\n')
		}
		return out
	}
	costs["zstd, segment"] += len(plain.EncodeAll(joined(bodies), nil))
	if err := encoder.useDictionary(sample); err != nil {
		t.Fatal(err)
	}
	defer func() { _ = encoder.useDictionary(nil) }()
	sampled := plain
	if encoder.dictWriter != nil {
		sampled = encoder.dictWriter
	}
	for start := 0; start < len(bodies); start += recordEventLimit {
		block := bodies[start:min(start+recordEventLimit, len(bodies))]
		costs["zstd, block"] += len(plain.EncodeAll(joined(block), nil))
		costs["zstd, block and sample"] += len(sampled.EncodeAll(joined(block), nil))
		costs["v2 column and sample"] += len(encoder.appendText(nil, block))
	}
	if len(sample) > 0 {
		costs["v2 column and sample"] += len(plain.EncodeAll(sample, nil))
	}
	_ = encoder.useDictionary(nil)
	for start := 0; start < len(bodies); start += recordEventLimit {
		costs["v2 column"] += len(encoder.appendText(nil, bodies[start:min(start+recordEventLimit, len(bodies))]))
	}
}

// v2SealByAge cuts each stream's arrivals as a head would seal them: a full segment,
// or the oldest waiting record older than age by the newest arrival's clock
func v2SealByAge(events []recordEvent, age int64) [][]recordEvent {
	var batches [][]recordEvent
	for _, stream := range v2Batches(events) {
		start := 0
		for i := range stream {
			if age > 0 && i > start && stream[i].at-stream[start].at > age {
				batches, start = append(batches, stream[start:i]), i
			}
		}
		batches = append(batches, stream[start:])
	}
	return batches
}

// TestRecordV2SealPolicy measures what sealing by age costs a real, mostly quiet, fleet
func TestRecordV2SealPolicy(t *testing.T) {
	containers := v2DockerCorpus(t)
	var events []recordEvent
	for i := range containers {
		part, _, _ := v2DockerEvents(&containers[i], false)
		events = append(events, part...)
	}
	for _, age := range []time.Duration{time.Minute, 10 * time.Minute, time.Hour, 6 * time.Hour, 24 * time.Hour, 0} {
		encoder, _ := v2TestCodec(t)
		var segments []v2Segment
		payload, sizes := 0, []int{}
		for _, batch := range v2SealByAge(events, int64(age)) {
			segment, _, err := encoder.encodeSegment(batch, false)
			if err != nil {
				t.Fatal(err)
			}
			segments = append(segments, segment)
			sizes = append(sizes, len(batch))
			payload += len(segment.row)
			for _, block := range segment.blocks {
				payload += len(block.body)
			}
		}
		size := v2FileSize(t, segments)
		slices.Sort(sizes)
		t.Logf("seal after %-8v segments=%6d median_records=%5d payload=%.3f file=%.3f B/record", age, len(segments),
			sizes[len(sizes)/2], float64(payload)/float64(len(events)), float64(size)/float64(len(events)))
	}
}

func v2FileSize(t *testing.T, segments []v2Segment) int64 {
	t.Helper()
	db, err := v2OpenStore(t.Context(), filepath.Join(t.TempDir(), "records.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	if err = v2WriteSegments(t.Context(), db, segments); err != nil {
		t.Fatal(err)
	}
	size, _, err := v2StoreSize(t.Context(), db)
	if err != nil {
		t.Fatal(err)
	}
	return size
}

// TestRecordV2HeadFlushes measures a head written once a second per stream: what one
// flush costs as plain rows, as rows under zstd, and as a v2 segment of its own
func TestRecordV2HeadFlushes(t *testing.T) {
	containers := v2DockerCorpus(t)
	var events []recordEvent
	for i := range containers {
		part, _, _ := v2DockerEvents(&containers[i], false)
		events = append(events, part...)
	}
	flushes := v2SealByAge(events, int64(time.Second))
	encoder, _ := v2TestCodec(t)
	writer, err := zstd.NewWriter(nil, zstd.WithEncoderConcurrency(1), zstd.WithEncoderLevel(zstd.SpeedDefault))
	if err != nil {
		t.Fatal(err)
	}
	defer writer.Close()
	var rows, compressed, segments int
	for _, flush := range flushes {
		var raw []byte
		for _, event := range flush {
			raw = appendRecordEvent(raw, event)
		}
		rows += len(raw)
		compressed += min(len(raw), len(writer.EncodeAll(raw, nil)))
		segment, _, err := encoder.encodeSegment(flush, false)
		if err != nil {
			t.Fatal(err)
		}
		segments += len(segment.row)
		for _, block := range segment.blocks {
			segments += len(block.body)
		}
	}
	n := float64(len(events))
	t.Logf("flushes=%d records/flush=%.1f rows=%.1f rows_zstd=%.1f v2_per_flush=%.1f B/record",
		len(flushes), n/float64(len(flushes)), float64(rows)/n, float64(compressed)/n, float64(segments)/n)
}
