package spike

import (
	"bytes"
	"encoding/binary"
	"errors"
	"hash/crc32"
	"slices"
	"testing"
)

const (
	recordSegmentBlocks    = 8
	recordSegmentMaxBlocks = 16
	recordContextLimit     = 2048
	recordDictionaryLimit  = 256 << 10
)

func (c *recordBlockCodec) segmentBlockLimit() int {
	if c.features&recordScope16 != 0 {
		return recordSegmentMaxBlocks
	}
	return recordSegmentBlocks
}

func recordSegmentEnvelope(mode byte, payload []byte) []byte {
	out := append([]byte{'R', 'S', 1, mode}, payload...)
	return binary.LittleEndian.AppendUint32(out, crc32.ChecksumIEEE(out))
}

func (c *recordBlockCodec) encodeRecordSegment(batches [][]recordEvent) ([]byte, error) {
	if len(batches) == 0 || len(batches) > c.segmentBlockLimit() {
		return nil, errors.New("record segment block count")
	}
	baseline := binary.AppendUvarint(nil, uint64(len(batches)))
	for _, events := range batches {
		block, err := c.encode(events)
		if err != nil {
			return nil, err
		}
		baseline = appendRecordString(baseline, string(block))
	}
	best := recordSegmentEnvelope(0, baseline)
	if shared, ok := c.sharedRecordContexts(batches); ok {
		if candidate := recordSegmentEnvelope(1, shared); len(candidate) < len(best) {
			best = candidate
		}
	}
	if c.features&recordBucketOrder != 0 {
		if candidate := c.bucketRecordSegment(batches); candidate != nil && len(candidate) < len(best) {
			best = candidate
		}
	}
	if c.features&recordOuterCompression != 0 {
		candidate, err := c.compressRecordSegment(best)
		if err != nil {
			return nil, err
		}
		if len(candidate) < len(best) {
			best = candidate
		}
	}
	return best, nil
}

func (c *recordBlockCodec) sharedRecordContexts(batches [][]recordEvent) ([]byte, bool) {
	seen := map[string]uint64{}
	var contexts []string
	references := make([][]uint64, len(batches))
	dictionaryBytes := 0
	for i, events := range batches {
		for _, event := range events {
			value := string(appendRecordFields(nil, event.context))
			id, ok := seen[value]
			if !ok {
				dictionaryBytes += len(value)
				if len(contexts) == recordContextLimit || dictionaryBytes > recordDictionaryLimit {
					return nil, false
				}
				id = uint64(len(contexts))
				seen[value] = id
				contexts = append(contexts, value)
			}
			references[i] = append(references[i], id)
		}
	}
	out := binary.AppendUvarint(nil, uint64(len(contexts)))
	out = append(out, c.encodeContextDictionary(contexts)...)
	out = binary.AppendUvarint(out, uint64(len(batches)))
	for i, events := range batches {
		stripped := slices.Clone(events)
		for j := range stripped {
			stripped[j].context = nil
		}
		block, err := c.encode(stripped)
		if err != nil {
			return nil, false
		}
		out = appendRecordString(out, string(block))
		out = append(out, c.pack(c.encodeNumbers(references[i]))...)
	}
	return out, true
}

func (c *recordBlockCodec) decodeRecordSegment(data []byte) ([]recordEvent, error) {
	if len(data) < 9 || len(data) > recordSegmentMaxBlocks*(recordByteLimit+128) ||
		!bytes.Equal(data[:3], []byte{'R', 'S', 1}) {
		return nil, errors.New("record segment header or size")
	}
	end := len(data) - 4
	if crc32.ChecksumIEEE(data[:end]) != binary.LittleEndian.Uint32(data[end:]) {
		return nil, errors.New("record segment checksum")
	}
	inflated := 0
	cursor := recordCursor{data: data[3:end], inflated: &inflated}
	mode := cursor.number(4)
	if mode == 4 {
		return c.readCompressedRecordSegment(cursor.data)
	}
	if mode == 3 {
		events := c.readRecordBuckets(&cursor)
		return events, cursor.finish()
	}
	if mode == 2 {
		return nil, errors.New("whole-frame baseline is not a microblock segment")
	}
	var contexts [][]recordField
	if mode == 1 {
		contexts = c.readRecordContexts(&cursor)
	}
	count := cursor.number(recordSegmentMaxBlocks)
	if count == 0 {
		return nil, errors.New("record segment is empty")
	}
	var events []recordEvent
	for range count {
		block := cursor.take(cursor.number(recordByteLimit + 64))
		if cursor.err != nil {
			return nil, cursor.err
		}
		batch, err := c.decode(block)
		if err != nil {
			return nil, err
		}
		if mode == 1 {
			c.restoreRecordContexts(&cursor, batch, contexts)
		}
		if _, err = rawRecordEvents(batch); err != nil {
			return nil, err
		}
		events = append(events, batch...)
	}
	if err := cursor.finish(); err != nil {
		return nil, err
	}
	return events, nil
}

func (c *recordBlockCodec) readRecordContexts(cursor *recordCursor) [][]recordField {
	count := cursor.number(recordContextLimit)
	var values []string
	if cursor.number(1) == 0 {
		values = c.decodeColumn(cursor, count)
	} else {
		values = c.readContextDictionaryRows(cursor, count)
	}
	var contexts [][]recordField
	bytes := 0
	for _, value := range values {
		bytes += len(value)
		if bytes > recordDictionaryLimit {
			cursor.fail("context dictionary byte limit")
			return nil
		}
		fields := recordCursor{data: []byte(value)}
		context := fields.fields()
		if err := fields.finish(); err != nil {
			cursor.fail("context dictionary: " + err.Error())
			return nil
		}
		contexts = append(contexts, context)
	}
	return contexts
}

func (c *recordBlockCodec) encodeContextDictionary(contexts []string) []byte {
	best := append([]byte{0}, c.encodeColumn(contexts)...)
	stream := recordStream{codec: c}
	var blocks [][]byte
	for _, value := range contexts {
		fields := recordCursor{data: []byte(value)}
		event := recordEvent{name: "context", attrs: fields.fields()}
		if fields.finish() != nil {
			return best
		}
		block, err := stream.add(event)
		if err != nil {
			return best
		}
		if block != nil {
			blocks = append(blocks, block)
		}
	}
	last, err := stream.flush()
	if err != nil {
		return best
	}
	if last != nil {
		blocks = append(blocks, last)
	}
	candidate := binary.AppendUvarint([]byte{1}, uint64(len(blocks)))
	for _, block := range blocks {
		candidate = appendRecordString(candidate, string(block))
	}
	if len(candidate) < len(best) {
		best = candidate
	}
	return best
}

func (c *recordBlockCodec) readContextDictionaryRows(cursor *recordCursor, count int) []string {
	var values []string
	for range cursor.number(4) {
		body := cursor.take(cursor.number(recordByteLimit + 64))
		events, err := c.decode(body)
		if err != nil {
			cursor.fail("context dictionary block: " + err.Error())
			return nil
		}
		for _, event := range events {
			if event.at != 0 || event.name != "context" || event.stream != "" || event.presence() != 0 || len(event.context) != 0 {
				cursor.fail("context dictionary row shape")
				return nil
			}
			values = append(values, string(appendRecordFields(nil, event.attrs)))
			if len(values) > count {
				cursor.fail("context dictionary row count")
				return nil
			}
		}
	}
	if len(values) != count {
		cursor.fail("context dictionary row count")
	}
	return values
}

func (c *recordBlockCodec) restoreRecordContexts(cursor *recordCursor, batch []recordEvent, contexts [][]recordField) {
	references := recordCursor{data: c.unpack(cursor)}
	ids := readRecordNumbers(&references, len(batch))
	if err := references.finish(); err != nil {
		cursor.fail("context references: " + err.Error())
		return
	}
	for i, id := range ids {
		if id >= uint64(len(contexts)) || len(batch[i].context) != 0 {
			cursor.fail("context reference or duplicate context")
			return
		}
		batch[i].context = slices.Clone(contexts[id])
	}
}

func TestRecordSegmentsOwnContextsAndPreserveMicroblockBounds(t *testing.T) {
	codec := recordTestCodec(t)
	events := recordFixture("frontend", 2048)
	batches := [][]recordEvent{events[:1024], events[1024:]}
	segment, err := codec.encodeRecordSegment(batches)
	if err != nil {
		t.Fatal(err)
	}
	decoded, err := recordTestCodec(t).decodeRecordSegment(segment)
	if err != nil {
		t.Fatal(err)
	}
	assertRecordEvents(t, events, decoded)
	for i := range min(200, len(segment)) {
		bad := bytes.Clone(segment)
		bad[i] ^= 1
		if _, err = codec.decodeRecordSegment(bad); err == nil {
			t.Fatalf("segment corruption accepted at %d", i)
		}
	}
	if _, err = codec.encodeRecordSegment(make([][]recordEvent, recordSegmentBlocks+1)); err == nil {
		t.Fatal("unbounded segment accepted")
	}
	if len(decoded[0].context) > 0 {
		decoded[0].context[0].value = "null"
		again, decodeErr := codec.decodeRecordSegment(segment)
		if decodeErr != nil {
			t.Fatal(decodeErr)
		}
		assertRecordEvents(t, events, again)
	}
}

func FuzzRecordSegmentDecode(f *testing.F) {
	codec := recordTestCodec(f)
	events := []recordEvent{
		{name: "click", context: []recordField{{"browser", `"Chrome"`}}, attrs: []recordField{{"x", "12"}}},
		{name: "click", context: []recordField{{"browser", `"Chrome"`}}, attrs: []recordField{{"x", "13"}}},
	}
	segment, err := codec.encodeRecordSegment([][]recordEvent{events[:1], events[1:]})
	if err != nil {
		f.Fatal(err)
	}
	f.Add(segment)
	if shared, ok := codec.sharedRecordContexts([][]recordEvent{events}); ok {
		f.Add(recordSegmentEnvelope(1, shared))
	}
	f.Fuzz(func(t *testing.T, input []byte) {
		if len(input) > recordSegmentMaxBlocks*(recordByteLimit+128) {
			return
		}
		data := bytes.Clone(input)
		if len(data) >= 8 {
			binary.LittleEndian.PutUint32(data[len(data)-4:], crc32.ChecksumIEEE(data[:len(data)-4]))
		}
		decoded, decodeErr := codec.decodeRecordSegment(data)
		if decodeErr == nil && len(decoded) > recordSegmentMaxBlocks*recordEventLimit {
			t.Fatal("segment exceeded event limit")
		}
	})
}
