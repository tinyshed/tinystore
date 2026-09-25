package spike

import (
	"bufio"
	"encoding/json"
	"fmt"
	"math"
	"math/rand/v2"
	"os"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"testing"
	"time"
)

func recordStatefulFixture(count int) []recordEvent {
	random := rand.New(rand.NewPCG(191, 73))
	states := make([]int, 64)
	events := make([]recordEvent, count)
	for i := range events {
		actor := random.IntN(len(states))
		states[actor] += random.IntN(5) - 2
		events[i] = recordEvent{
			at:     1_790_000_000_000_000_000 + int64(i)*1_000_000,
			stream: "clients", name: "cursor.sample",
			context: []recordField{{"session", recordJSON(fmt.Sprintf("session-%d", actor))}},
			attrs: []recordField{
				{"x", strconv.Itoa(states[actor] + 100_000*actor)},
				{"y", strconv.Itoa(3*(states[actor]+100_000*actor) + 7)},
				{"object", fmt.Sprintf(`{"position":%d,"visible":true,"items":[1,2,3]}`, states[actor])},
			},
		}
	}
	return events
}

func readRecordArchive(t *testing.T, path string) []recordEvent {
	t.Helper()
	file, err := os.Open(path)
	if err != nil {
		t.Fatal(err)
	}
	defer file.Close()
	scanner := bufio.NewScanner(file)
	scanner.Buffer(make([]byte, 4096), 4<<20)
	var events []recordEvent
	for scanner.Scan() {
		fields, parseErr := parseRecordFields(scanner.Bytes())
		if parseErr != nil {
			t.Fatal(parseErr)
		}
		event := recordEvent{stream: "github"}
		for _, field := range fields {
			switch field.key {
			case "type":
				if err = json.Unmarshal([]byte(field.value), &event.name); err != nil {
					t.Fatal(err)
				}
			case "created_at":
				var value string
				if err = json.Unmarshal([]byte(field.value), &value); err != nil {
					t.Fatal(err)
				}
				at, timeErr := time.Parse(time.RFC3339Nano, value)
				if timeErr != nil {
					t.Fatal(timeErr)
				}
				event.at = at.UnixNano()
			default:
				event.attrs = append(event.attrs, field)
			}
		}
		if _, err = rawRecordEvents([]recordEvent{event}); err != nil {
			t.Fatalf("archive record %d: %v", len(events), err)
		}
		events = append(events, event)
	}
	if err = scanner.Err(); err != nil {
		t.Fatal(err)
	}
	return events
}

func recordDeepBatches(t testing.TB, events []recordEvent) [][]recordEvent {
	t.Helper()
	var batches [][]recordEvent
	start, bytes, cells := 0, 0, 0
	for i, event := range events {
		size := len(appendRecordEvent(nil, event))
		fields := len(event.context) + len(event.attrs) + 8
		if i-start == recordEventLimit || bytes+size > recordByteLimit || cells+fields > recordCellLimit {
			batches = append(batches, events[start:i])
			start, bytes, cells = i, 0, 0
		}
		bytes, cells = bytes+size, cells+fields
	}
	if start < len(events) {
		batches = append(batches, events[start:])
	}
	return batches
}

func measureDeepRecordMode(t *testing.T, name, mode string, events []recordEvent, features uint32) {
	t.Helper()
	codec := recordTestCodec(t)
	codec.features = features
	batches := recordDeepBatches(t, events)
	var segments []recordMeasuredBlock
	bytes, buckets, shared := 0, 0, 0
	for start := 0; start < len(batches); start += codec.segmentBlockLimit() {
		group := batches[start:min(start+codec.segmentBlockLimit(), len(batches))]
		body, err := codec.encodeRecordSegment(group)
		if err != nil {
			t.Fatal(err)
		}
		decoded, err := codec.decodeRecordSegment(body)
		if err != nil {
			t.Fatal(err)
		}
		var expected []recordEvent
		first, last := group[0][0].at, group[0][0].at
		for _, batch := range group {
			expected = append(expected, batch...)
			for _, event := range batch {
				first, last = min(first, event.at), max(last, event.at)
			}
		}
		assertRecordEvents(t, expected, decoded)
		bytes += len(body)
		mode := body[3]
		if mode == 4 {
			interior, unpackErr := codec.reader.DecodeAll(body[4:len(body)-4], nil)
			if unpackErr != nil || len(interior) == 0 {
				t.Fatal("inspect wrapped mode", unpackErr)
			}
			mode = interior[0]
		}
		switch mode {
		case 3:
			buckets++
		case 1:
			shared++
		}
		segments = append(segments, recordMeasuredBlock{first: first, last: last, count: len(expected), adaptive: body})
	}
	size, objects, err := measureRecordFile(t.Context(), filepath.Join(t.TempDir(), "deep.db"), segments, true)
	if err != nil {
		t.Fatal(err)
	}
	t.Logf("deep %-12s %-10s records=%d payload=%.4f file=%.4f shared=%d buckets=%d objects=%s", name, mode,
		len(events), float64(bytes)/float64(len(events)), float64(size)/float64(len(events)), shared, buckets, objects)
}

func TestDeepRecordDensity(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1 to measure deep record representations")
	}
	filter := os.Getenv("TINYSTORE_RECORD_CASE")
	for _, name := range []string{"frontend", "backend", "mixed", "context_churn", "derived_fields", "stateful", "github"} {
		if filter != "" && name != filter {
			continue
		}
		var events []recordEvent
		if name == "stateful" {
			events = recordStatefulFixture(10_000)
		} else if name == "github" {
			path := os.Getenv("TINYSTORE_RECORD_EVENTS")
			if path == "" {
				continue
			}
			events = readRecordArchive(t, path)
		} else {
			events = recordFixture(name, 10_000)
		}
		modes := []struct {
			name     string
			features uint32
		}{
			{"previous", 0},
			{"numbers", recordNumberModels},
			{"state", recordNumberModels | recordStateModels},
			{"json", recordNumberModels | recordStateModels | recordJSONColumns},
			{"all", recordDeepAll},
			{"scope16", recordScope16},
			{"all16", recordDeepAll | recordScope16},
			{"final", recordDeepAll | recordScope16 | recordOuterCompression},
		}
		for _, mode := range modes {
			if selected := os.Getenv("TINYSTORE_RECORD_MODE"); selected != "" && !slices.Contains(strings.Split(selected, ","), mode.name) {
				continue
			}
			measureDeepRecordMode(t, name, mode.name, events, mode.features)
		}
	}
}

func TestDeepRecordRepresentationsAreExact(t *testing.T) {
	codec := recordTestCodec(t)
	codec.features = recordDeepAll
	for _, numbers := range [][]uint64{
		{math.MaxUint64, 0, 1, 5, 9},
		{1000, 1200, 900, 1100},
		{1 << 63, 1<<63 + 1, 0, 1},
		{0, 0, 0},
		{1_790_000_000_000_000_000, 1_790_000_000_000_000_123, 1_790_000_000_000_000_256},
	} {
		cursor := recordCursor{data: codec.encodeNumbers(numbers)}
		got := readRecordNumbers(&cursor, len(numbers))
		if err := cursor.finish(); err != nil || !slices.Equal(got, numbers) {
			t.Fatalf("number model differs: %v", err)
		}
	}
	values := []string{`{"a":[1,null,true], "str":"x\\y"}`, `{"a":[2,null,false], "str":"z"}`, `[]`, `{}`, `null`}
	encoded := codec.jsonRecordColumn(values)
	if encoded == nil {
		t.Fatal("JSON layout was not constructed")
	}
	cursor := recordCursor{data: codec.pack(encoded)}
	got := codec.decodeColumn(&cursor, len(values))
	if err := cursor.finish(); err != nil || !slices.Equal(got, values) {
		t.Fatalf("JSON reconstruction differs: %v, %q", err, got)
	}
	for _, transpose := range []bool{false, true} {
		values = []string{"\x00\xffab", "cd\xfe\x00", "1234"}
		cursor = recordCursor{data: codec.pack(recordFixedColumn(values, transpose))}
		got = codec.decodeColumn(&cursor, len(values))
		if err := cursor.finish(); err != nil || !slices.Equal(got, values) {
			t.Fatalf("fixed-byte reconstruction differs: %v", err)
		}
	}
	events := recordStatefulFixture(200)
	segment := codec.bucketRecordSegment([][]recordEvent{events[:100], events[100:]})
	decoded, err := codec.decodeRecordSegment(segment)
	if err != nil {
		t.Fatal(err)
	}
	assertRecordEvents(t, events, decoded)
	for _, value := range []string{`{"bad":1`, strings.Repeat("[", 200)} {
		if _, _, ok := recordJSONParts(value); ok {
			t.Fatal("invalid JSON accepted")
		}
	}
}
