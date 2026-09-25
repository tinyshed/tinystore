package spike

import (
	"bytes"
	"cmp"
	"encoding/binary"
	"fmt"
	"hash/crc32"
	"math"
	"math/rand/v2"
	"slices"
	"testing"
	"time"
)

func v2TestCodec(t testing.TB) (*v2Encoder, *v2Decoder) {
	t.Helper()
	encoder, err := newV2Encoder()
	if err != nil {
		t.Fatal(err)
	}
	decoder, err := newV2Decoder()
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		encoder.writer.Close()
		decoder.reader.Close()
	})
	return encoder, decoder
}

func v2SortedByTime(events []recordEvent) []recordEvent {
	sorted := slices.Clone(events)
	slices.SortStableFunc(sorted, func(a, b recordEvent) int { return cmp.Compare(a.at, b.at) })
	return sorted
}

func v2RoundTrip(t testing.TB, encoder *v2Encoder, decoder *v2Decoder, events []recordEvent, arrival bool) v2Segment {
	t.Helper()
	segment, stored, err := encoder.encodeSegment(events, arrival)
	if err != nil {
		t.Fatal(err)
	}
	expected := events
	if !arrival {
		expected = v2SortedByTime(events)
	}
	assertRecordEvents(t, expected, stored)
	schema, err := decoder.decodeSchema(segment.row)
	if err != nil {
		t.Fatal(err)
	}
	var decoded []recordEvent
	for _, block := range segment.blocks {
		events, err := decoder.decodeBlock(&schema, block.body)
		if err != nil {
			t.Fatal(err)
		}
		decoded = append(decoded, events...)
	}
	assertRecordEvents(t, expected, decoded)
	return segment
}

func TestRecordV2SegmentsRoundTripInTimeOrder(t *testing.T) {
	encoder, decoder := v2TestCodec(t)
	seeds := recordEdgeEvents()
	edge := make([]recordEvent, 700)
	for i := range edge {
		edge[i] = seeds[i%len(seeds)]
		edge[i].stream = "edge"
	}
	for _, arrival := range []bool{false, true} {
		v2RoundTrip(t, encoder, decoder, edge, arrival)
		for _, kind := range []string{"frontend", "backend", "context_churn", "derived_fields", "shape_churn", "noise"} {
			v2RoundTrip(t, encoder, decoder, recordFixture(kind, 3000), arrival)
		}
		v2RoundTrip(t, encoder, decoder, recordStatefulFixture(3000), arrival)
	}
}

func TestRecordV2MixedStreamsBecomeSeparateSegments(t *testing.T) {
	measured := v2Measure(t, recordFixture("mixed", 3000), false)
	streams := map[string]bool{}
	for _, segment := range measured.segments {
		if streams[segment.schema.stream] {
			t.Fatal("one stream split although it fits a segment")
		}
		streams[segment.schema.stream] = true
	}
	if len(streams) != 3 {
		t.Fatalf("want 3 stream segments, got %d", len(streams))
	}
}

func TestRecordV2IntegersAreExact(t *testing.T) {
	encoder, decoder := v2TestCodec(t)
	random := rand.New(rand.NewPCG(3, 5))
	sparse := make([]int64, 900)
	for i := range sparse {
		if i%23 == 0 {
			sparse[i] = random.Int64()
		}
	}
	skewed, sorted, outliers := make([]int64, 1024), make([]int64, 1024), make([]int64, 1024)
	for i := range skewed {
		skewed[i] = []int64{200, 200, 200, 404, 500}[random.IntN(5)]
		sorted[i] = int64(i)*1_234_567 + int64(random.IntN(1000))*1_000_000
		outliers[i] = int64(random.IntN(64))
		if i%100 == 0 {
			outliers[i] = math.MaxInt64 - int64(i)
		}
	}
	slices.Sort(sorted)
	for _, values := range [][]int64{
		{},
		{0},
		{math.MinInt64},
		{math.MaxInt64, math.MinInt64, 0, -1, 1},
		{7, 7, 7, 7, 7, 7, 7, 7, 7, 7},
		sparse, skewed, sorted, outliers,
	} {
		encoded := encoder.appendInts(nil, values)
		cursor := recordCursor{data: encoded}
		decoded := decoder.ints(&cursor, len(values), false)
		if err := cursor.finish(); err != nil || !slices.Equal(decoded, values) {
			t.Fatalf("integers %v: %v", values[:min(len(values), 8)], err)
		}
	}
}

func TestRecordV2ValuesKeepEverySpelling(t *testing.T) {
	encoder, decoder := v2TestCodec(t)
	for _, values := range [][]string{
		{"0", "-0", "007", "+1", "18446744073709551615", "-9223372036854775808", "1e999", "null", `""`},
		{"1", "2", "3", "4", "5", "6", "7", "null", "9", "10", "11", "12", "13", "14", "15", "16"},
		{`"00010203-0405-0607-0809-0a0b0c0d0e0f"`, `"ffffffff-ffff-ffff-ffff-ffffffffffff"`},
		{`"00010203-0405-0607-0809-0A0B0C0D0E0F"`, `"x"`},
		{`"deadbeef"`, `"0123abcd"`},
		{"deadbeef", "0123abcd"},
		{`"154"`, `"155"`, `"154"`},
		{"a\x00b\xff", "", `"`, `"a"`, "multi\nline"},
	} {
		encoded := encoder.appendValues(nil, values)
		cursor := recordCursor{data: encoded}
		decoded := decoder.values(&cursor, len(values))
		if err := cursor.finish(); err != nil || !slices.Equal(decoded, values) {
			t.Fatalf("values %q became %q: %v", values, decoded, err)
		}
	}
}

func TestRecordV2CorruptionIsRefused(t *testing.T) {
	encoder, decoder := v2TestCodec(t)
	segment := v2RoundTrip(t, encoder, decoder, recordFixture("derived_fields", 200), false)
	schema, err := decoder.decodeSchema(segment.row)
	if err != nil {
		t.Fatal(err)
	}
	body := segment.blocks[0].body
	for i := range body {
		corrupt := bytes.Clone(body)
		corrupt[i] ^= 0x20
		if _, err = decoder.decodeBlock(&schema, corrupt); err == nil {
			t.Fatalf("changed block byte %d accepted", i)
		}
	}
	for end := range len(body) {
		if _, err = decoder.decodeBlock(&schema, body[:end]); err == nil {
			t.Fatalf("truncated block %d accepted", end)
		}
	}
	for i := range segment.row {
		corrupt := bytes.Clone(segment.row)
		corrupt[i] ^= 0x20
		if _, err = decoder.decodeSchema(corrupt); err == nil {
			t.Fatalf("changed segment byte %d accepted", i)
		}
	}
}

func v2Repair(data []byte) []byte {
	data = bytes.Clone(data)
	if len(data) >= 8 {
		binary.LittleEndian.PutUint32(data[len(data)-4:], crc32.ChecksumIEEE(data[:len(data)-4]))
	}
	return data
}

func FuzzRecordV2Block(f *testing.F) {
	encoder, decoder := v2TestCodec(f)
	events := recordFixture("derived_fields", 60)
	events = append(events, recordStatefulFixture(60)...)
	for i := range events {
		events[i].stream = "fuzz"
	}
	segment, _, err := encoder.encodeSegment(events, false)
	if err != nil {
		f.Fatal(err)
	}
	for _, block := range segment.blocks {
		f.Add(segment.row, block.body)
	}
	f.Fuzz(func(t *testing.T, row, body []byte) {
		start := time.Now()
		defer func() {
			if elapsed := time.Since(start); elapsed > 2*time.Second {
				t.Fatalf("decoding %d+%d bytes took %v", len(row), len(body), elapsed)
			}
		}()
		schema, err := decoder.decodeSchema(v2Repair(row))
		if err != nil {
			return
		}
		decoded, err := decoder.decodeBlock(&schema, v2Repair(body))
		if err != nil || len(decoded) > v2SegmentEvents {
			return
		}
		again, _, err := encoder.encodeSegment(decoded, true)
		if err != nil {
			return
		}
		schema, err = decoder.decodeSchema(again.row)
		if err != nil {
			t.Fatal(err)
		}
		var roundTrip []recordEvent
		for _, block := range again.blocks {
			part, err := decoder.decodeBlock(&schema, block.body)
			if err != nil {
				t.Fatal(err)
			}
			roundTrip = append(roundTrip, part...)
		}
		assertRecordEvents(t, decoded, roundTrip)
	})
}

func BenchmarkRecordV2Throughput(b *testing.B) {
	for _, name := range []string{"frontend", "backend"} {
		events := recordFixture(name, v2SegmentEvents)
		encoder, decoder := v2TestCodec(b)
		segment, _, err := encoder.encodeSegment(events, false)
		if err != nil {
			b.Fatal(err)
		}
		b.Run(name+"/encode", func(b *testing.B) {
			b.ReportAllocs()
			for b.Loop() {
				if _, _, err := encoder.encodeSegment(events, false); err != nil {
					b.Fatal(err)
				}
			}
			b.ReportMetric(float64(b.N*len(events))/b.Elapsed().Seconds(), "records/s")
		})
		b.Run(name+"/decode", func(b *testing.B) {
			b.ReportAllocs()
			for b.Loop() {
				schema, err := decoder.decodeSchema(segment.row)
				if err != nil {
					b.Fatal(err)
				}
				for _, block := range segment.blocks {
					if _, err = decoder.decodeBlock(&schema, block.body); err != nil {
						b.Fatal(err)
					}
				}
			}
			b.ReportMetric(float64(b.N*len(events))/b.Elapsed().Seconds(), "records/s")
		})
	}
}

func TestRecordV2TextSampleIsKeptAndRequired(t *testing.T) {
	encoder, decoder := v2TestCodec(t)
	random := rand.New(rand.NewPCG(7, 11))
	events := make([]recordEvent, 4000)
	for i := range events {
		body := fmt.Sprintf("GET /api/items/%d?page=%d from 10.0.%d.%d took %dms status %d", random.IntN(90000),
			random.IntN(40), random.IntN(256), random.IntN(256), random.IntN(900), []int{200, 404, 500}[random.IntN(3)])
		events[i] = recordEvent{at: int64(i) * 1_000_000, stream: "text", name: "log", body: &body}
	}
	segment := v2RoundTrip(t, encoder, decoder, events, false)
	if len(segment.schema.dictionary) == 0 {
		t.Fatal("a text-heavy segment kept no sample")
	}
	schema, err := decoder.decodeSchema(segment.row)
	if err != nil {
		t.Fatal(err)
	}
	schema.dictionary = nil
	fresh := func() *v2Decoder {
		_, clean := v2TestCodec(t)
		return clean
	}()
	if _, err = fresh.decodeBlock(&schema, segment.blocks[0].body); err == nil {
		t.Fatal("a block decoded without its segment's text sample")
	}
}
