package spike

import (
	"bytes"
	"encoding/binary"
	"hash/crc32"
	"math"
	"slices"
	"strings"
	"testing"
)

func recordTestCodec(t testing.TB) *recordBlockCodec {
	t.Helper()
	codec, err := newRecordBlockCodec()
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(codec.close)
	return codec
}

func assertRecordEvents(t testing.TB, expected, actual []recordEvent) {
	t.Helper()
	if len(expected) != len(actual) {
		t.Fatalf("record count: want %d, got %d", len(expected), len(actual))
	}
	for i, event := range expected {
		if !bytes.Equal(appendRecordEvent(nil, event), appendRecordEvent(nil, actual[i])) {
			t.Fatalf("record %d differs: want %#v, got %#v", i, event, actual[i])
		}
	}
}

func TestRecordEventWireKeepsTypesOrderAndOptionalFields(t *testing.T) {
	input := `{"version":1,"time":"2026-09-25T12:00:00.000000123+03:00","stream":"frontend","name":"ui.click","body":"","level":0,"trace_id":"000102030405060708090a0b0c0d0e0f","span_id":"0001020304050607","context":{"browser":"Chrome"},"attrs":{"large":18446744073709551615,"negative":-0.00,"fraction":1.2300,"null":null,"empty":"","nested":{"a":[1,true,null]},"large":1e1000}}`
	event, err := parseRecordEvent([]byte(input))
	if err != nil {
		t.Fatal(err)
	}
	if event.body == nil || *event.body != "" || event.level == nil || *event.level != 0 {
		t.Fatal("present empty body or zero level was lost")
	}
	expected := []recordField{
		{"large", "18446744073709551615"},
		{"negative", "-0.00"},
		{"fraction", "1.2300"},
		{"null", "null"},
		{"empty", `""`},
		{"nested", `{"a":[1,true,null]}`},
		{"large", "1e1000"},
	}
	if !slices.Equal(event.attrs, expected) || event.at%1000 != 123 || len(event.traceID) != 16 || len(event.spanID) != 8 {
		t.Fatalf("normalization lost a field: %#v", event)
	}
	for _, invalid := range []string{
		input + "{}", strings.Replace(input, `"version":1`, `"version":2`, 1),
		strings.Replace(input, `"version":1`, `"version":1,"version":1`, 1),
		strings.Replace(input, `"context":{"browser":"Chrome"}`, `"context":[]`, 1),
		strings.Replace(input, `"name":"ui.click"`, `"unknown":1`, 1),
		strings.Replace(input, "0001020304050607\"", "01\"", 1),
	} {
		if _, err = parseRecordEvent([]byte(invalid)); err == nil {
			t.Fatalf("accepted invalid envelope: %s", invalid)
		}
	}
}

func recordEdgeEvents() []recordEvent {
	return []recordEvent{
		{at: math.MinInt64, name: "event", attrs: []recordField{{"n", "18446744073709551615"}}},
		{at: math.MaxInt64, stream: "java", name: "log", body: new("java.Exception\n\tat X\t\x00\xff")},
		{at: 0, stream: "browser", name: "click", body: new(""), level: new(int64(0))},
		{
			at: -1, name: "log", traceID: make([]byte, 16), spanID: make([]byte, 8),
			context: []recordField{{"a", `"Chrome"`}}, attrs: []recordField{{"n", "-0"}, {"n", "null"}},
		},
		{at: 0, name: "event", attrs: []recordField{{"x", `{"nested":[true,1e999,"\\u0061"]}`}, {"n", "1.2300"}}},
	}
}

func TestRecordBlocksPreserveEventsAcrossEveryLayout(t *testing.T) {
	codec := recordTestCodec(t)
	seeds := recordEdgeEvents()
	events := make([]recordEvent, 500)
	for i := range events {
		events[i] = seeds[i%len(seeds)]
	}
	raw, err := rawRecordEvents(events)
	if err != nil {
		t.Fatal(err)
	}
	for _, tuple := range []bool{false, true} {
		for _, shared := range []bool{false, true} {
			layout, ok := planRecordColumns(events, tuple, shared)
			if !ok {
				t.Fatal("layout exceeded limits")
			}
			block := recordEnvelope(1, len(events), len(raw), codec.encodeRecordLayout(layout))
			decoded, decodeErr := codec.decode(block)
			if decodeErr != nil {
				t.Fatal(decodeErr)
			}
			assertRecordEvents(t, events, decoded)
		}
	}
	baseline, err := codec.baseline(events)
	if err != nil {
		t.Fatal(err)
	}
	block, err := codec.encode(events)
	if err != nil || len(block) > len(baseline) {
		t.Fatalf("fallback guarantee: %d > %d, %v", len(block), len(baseline), err)
	}
	decoded, err := codec.decode(block)
	if err != nil {
		t.Fatal(err)
	}
	assertRecordEvents(t, events, decoded)
}

func TestRecordScalarsAndNumberRepresentationsAreExact(t *testing.T) {
	codec := recordTestCodec(t)
	values := []string{
		"0", "-0", "001", "+1", "-0.00", "-0.25", "1.2300", "18446744073709551615",
		"-9223372036854775808", "9223372036854775807", "1e999", "null", "true", `""`, `"\\u0061"`,
		`"00010203-0405-0607-0809-0a0b0c0d0e0f"`, "DEADBEEF", "deadbeef", "dEaDbEeF", "a\x00b\xff",
	}
	for _, value := range values {
		if got, ok := parseRecordScalar(value).render(); !ok || got != value {
			t.Fatalf("scalar %q became %q", value, got)
		}
	}
	for _, candidate := range [][]byte{appendRecordStrings([]byte{0}, values), codec.dictionaryColumn(values), codec.typedColumn(values)} {
		cursor := recordCursor{data: codec.pack(candidate)}
		decoded := codec.decodeColumn(&cursor, len(values))
		if err := cursor.finish(); err != nil || !slices.Equal(decoded, values) {
			t.Fatalf("column round trip: %v, %q", err, decoded)
		}
	}
	numbers := []uint64{0, 0, 1, math.MaxUint64, math.MaxInt64, 1 << 63, 4, 2, 1}
	for _, input := range [][]uint64{numbers, {0, 0, 0}, {7, 7, 7}, {}} {
		for mode := range 4 {
			transformed := slices.Clone(input)
			if mode >= 2 {
				previous := uint64(0)
				for i, value := range input {
					transformed[i], previous = recordZigzag(value-previous), value
				}
			}
			body := []byte{byte(mode)}
			if mode&1 == 0 {
				body = append(body, recordVarints(transformed)...)
			} else {
				body = append(body, recordPackedNumbers(transformed)...)
			}
			cursor := recordCursor{data: body}
			got := readRecordNumbers(&cursor, len(input))
			if err := cursor.finish(); err != nil || !slices.Equal(input, got) {
				t.Fatalf("numeric mode %d: %v, %v", mode, err, got)
			}
		}
	}
}

func TestRecordStreamBoundsOwnershipAndIndependentBlocks(t *testing.T) {
	codec := recordTestCodec(t)
	stream := recordStream{codec: codec}
	event := recordEvent{
		name: "click", body: new("before"), level: new(int64(0)),
		attrs: []recordField{{"key", "42"}}, traceID: make([]byte, 16),
	}
	if _, err := stream.add(event); err != nil {
		t.Fatal(err)
	}
	event.attrs[0].value, *event.body, *event.level, event.traceID[0] = "43", "after", 8, 1
	first, err := stream.flush()
	if err != nil {
		t.Fatal(err)
	}
	decoded, err := recordTestCodec(t).decode(first)
	if err != nil || decoded[0].attrs[0].value != "42" || *decoded[0].body != "before" || *decoded[0].level != 0 || decoded[0].traceID[0] != 0 {
		t.Fatalf("buffer borrowed caller memory: %v", err)
	}
	event = recordEvent{name: "later", body: new(strings.Repeat("x", recordByteLimit/2))}
	if block, addErr := stream.add(event); addErr != nil || block != nil {
		t.Fatal("first half block", addErr)
	}
	if block, addErr := stream.add(event); addErr != nil || block == nil {
		t.Fatal("byte boundary failed to seal", addErr)
	}
	if _, err = stream.add(recordEvent{name: "oversized", body: new(strings.Repeat("x", recordByteLimit))}); err == nil {
		t.Fatal("oversized event accepted")
	}
	if len(stream.pending) != 1 {
		t.Fatal("refused event changed pending records")
	}
	again, err := recordTestCodec(t).decode(first)
	if err != nil {
		t.Fatal(err)
	}
	assertRecordEvents(t, decoded, again)
}

func TestRecordCorruptionIsRefused(t *testing.T) {
	codec := recordTestCodec(t)
	block, err := codec.encode(recordEdgeEvents())
	if err != nil {
		t.Fatal(err)
	}
	for i := range block {
		corrupt := bytes.Clone(block)
		corrupt[i] ^= 0x80
		if _, err = codec.decode(corrupt); err == nil {
			t.Fatalf("accepted changed byte %d", i)
		}
	}
	for end := range len(block) {
		if _, err = codec.decode(block[:end]); err == nil {
			t.Fatalf("accepted truncation %d", end)
		}
	}
	bad := recordEnvelope(1, 1, 20, codec.pack([]byte{0, 0, 0}))
	if _, err = codec.decode(bad); err == nil {
		t.Fatal("accepted invalid counts behind a valid checksum")
	}
	events := make([]recordEvent, recordEventLimit)
	for i := range events {
		events[i] = recordEvent{name: "click", context: []recordField{{"large", recordJSON(strings.Repeat("x", 6000))}}}
	}
	layout, ok := planRecordColumns(events, true, true)
	if !ok {
		t.Fatal("could not construct expansion fixture")
	}
	bad = recordEnvelope(1, len(events), recordByteLimit, codec.encodeRecordLayout(layout))
	if _, err = codec.decode(bad); err == nil {
		t.Fatal("accepted a small context dictionary expanding beyond the block budget")
	}
}

func FuzzRecordBlockDecode(f *testing.F) {
	codec := recordTestCodec(f)
	block, err := codec.encode(recordEdgeEvents())
	if err != nil {
		f.Fatal(err)
	}
	f.Add(block)
	for _, tuple := range []bool{false, true} {
		layout, _ := planRecordColumns(recordEdgeEvents(), tuple, true)
		raw, _ := rawRecordEvents(recordEdgeEvents())
		f.Add(recordEnvelope(1, 5, len(raw), codec.encodeRecordLayout(layout)))
	}
	f.Fuzz(func(t *testing.T, input []byte) {
		if len(input) > recordByteLimit+64 {
			return
		}
		data := bytes.Clone(input)
		if len(data) >= 8 {
			binary.LittleEndian.PutUint32(data[len(data)-4:], crc32.ChecksumIEEE(data[:len(data)-4]))
		}
		decoded, decodeErr := codec.decode(data)
		if decodeErr != nil {
			return
		}
		repacked, encodeErr := codec.encode(decoded)
		if encodeErr != nil {
			t.Fatal(encodeErr)
		}
		again, decodeErr := codec.decode(repacked)
		if decodeErr != nil {
			t.Fatal(decodeErr)
		}
		assertRecordEvents(t, decoded, again)
	})
}
