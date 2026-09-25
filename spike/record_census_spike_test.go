package spike

import (
	"maps"
	"os"
	"path/filepath"
	"slices"
	"testing"

	"github.com/klauspost/compress/zstd"
)

func censusRecordBlock(c *recordBlockCodec, block []byte, prefix string, costs map[string]int) {
	cursor := recordCursor{data: block[4 : len(block)-4]}
	mode := cursor.number(1)
	count := cursor.number(recordEventLimit)
	cursor.number(recordByteLimit)
	costs[prefix+"headers"] += len(block) - len(cursor.data)
	if mode == 0 {
		costs[prefix+"rows"] += len(cursor.data)
		return
	}
	before := len(cursor.data)
	metadata := recordCursor{data: c.unpack(&cursor)}
	layout, counts := readRecordLayout(&metadata, count)
	costs[prefix+"shapes"] += before - len(cursor.data)
	names := map[int]string{0: "time", 1: "stream", 2: "name"}
	for _, shape := range layout.shapes {
		var fields []string
		for i, name := range []string{"level", "body", "trace", "span"} {
			if shape.presence>>i&1 != 0 {
				fields = append(fields, name)
			}
		}
		if layout.tuple {
			fields = append(fields, "context")
		}
		for _, key := range shape.context {
			fields = append(fields, "context."+key)
		}
		for _, key := range shape.attrs {
			fields = append(fields, "attrs."+key)
		}
		for i, column := range shape.columns {
			names[column] = fields[i]
		}
	}
	for i := range counts {
		before = len(cursor.data)
		switch cursor.number(2) {
		case 0:
			c.unpack(&cursor)
		case 1:
			cursor.unsigned()
		case 2:
			cursor.unsigned()
			c.unpack(&cursor)
		}
		costs[prefix+names[i]] += before - len(cursor.data)
	}
}

func TestRecordOuterCompression(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1")
	}
	events := recordFixture("frontend", 10_000)
	c := recordTestCodec(t)
	c.features = recordDeepAll | recordScope16
	segment, err := c.encodeRecordSegment(recordDeepBatches(t, events))
	if err != nil {
		t.Fatal(err)
	}
	for _, level := range []zstd.EncoderLevel{zstd.SpeedDefault, zstd.SpeedBetterCompression, zstd.SpeedBestCompression} {
		writer, writerErr := zstd.NewWriter(nil, zstd.WithEncoderLevel(level), zstd.WithWindowSize(recordByteLimit),
			zstd.WithEncoderConcurrency(1), zstd.WithLowerEncoderMem(true))
		if writerErr != nil {
			t.Fatal(writerErr)
		}
		packed := writer.EncodeAll(segment[3:len(segment)-4], nil)
		writer.Close()
		body := recordSegmentEnvelope(4, packed)
		blocks := []recordMeasuredBlock{{first: events[0].at, last: events[len(events)-1].at, count: len(events), adaptive: body}}
		size, objects, fileErr := measureRecordFile(t.Context(), filepath.Join(t.TempDir(), "outer.db"), blocks, true)
		if fileErr != nil {
			t.Fatal(fileErr)
		}
		t.Logf("level=%s before=%d after=%d file=%d bytes_per_record=%.4f objects=%s", level, len(segment), len(body), size, float64(size)/float64(len(events)), objects)
	}
}

func censusRecordSegment(c *recordBlockCodec, segment []byte, costs map[string]int) {
	cursor := recordCursor{data: segment[4 : len(segment)-4]}
	costs["segment.headers"] += 8
	if segment[3] == 1 {
		before := len(cursor.data)
		c.readRecordContexts(&cursor)
		costs["segment.context_dictionary"] += before - len(cursor.data)
	}
	before := len(cursor.data)
	count := cursor.number(recordSegmentMaxBlocks)
	costs["segment.headers"] += before - len(cursor.data)
	for range count {
		before = len(cursor.data)
		block := cursor.take(cursor.number(recordByteLimit + 64))
		costs["segment.headers"] += before - len(cursor.data) - len(block)
		censusRecordBlock(c, block, "record.", costs)
		if segment[3] == 1 {
			before = len(cursor.data)
			c.unpack(&cursor)
			costs["segment.context_ids"] += before - len(cursor.data)
		}
	}
}

func TestRecordByteCensus(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1")
	}
	events := recordFixture("frontend", 10_000)
	batches := recordDeepBatches(t, events)
	for _, features := range []uint32{0, (recordDeepAll | recordScope16) &^ recordBucketOrder} {
		c := recordTestCodec(t)
		c.features = features
		costs := map[string]int{}
		total := 0
		for start := 0; start < len(batches); start += c.segmentBlockLimit() {
			segment, err := c.encodeRecordSegment(batches[start:min(start+c.segmentBlockLimit(), len(batches))])
			if err != nil {
				t.Fatal(err)
			}
			total += len(segment)
			t.Logf("outer-pack features=%d segment=%d packed=%d", features, len(segment), len(c.pack(segment))+8)
			censusRecordSegment(c, segment, costs)
		}
		sum := 0
		for _, key := range slices.Sorted(maps.Keys(costs)) {
			sum += costs[key]
			t.Logf("features=%d %-30s bytes=%d per_record=%.4f", features, key, costs[key], float64(costs[key])/float64(len(events)))
		}
		if sum != total {
			t.Fatalf("census %d != %d", sum, total)
		}
	}
}
