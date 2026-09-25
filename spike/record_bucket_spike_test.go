package spike

import (
	"encoding/binary"
	"slices"
)

func (c *recordBlockCodec) bucketRecordSegment(batches [][]recordEvent) []byte {
	known := map[string]int{}
	var contexts []string
	var buckets [][]recordEvent
	var order []uint64
	bytes := 0
	out := binary.AppendUvarint(nil, uint64(len(batches)))
	for _, events := range batches {
		out = binary.AppendUvarint(out, uint64(len(events)))
		for _, event := range events {
			key := string(appendRecordFields(nil, event.context))
			id, ok := known[key]
			if !ok {
				bytes += len(key)
				if len(contexts) == recordContextLimit || bytes > recordDictionaryLimit {
					return nil
				}
				id = len(contexts)
				known[key] = id
				contexts = append(contexts, key)
				buckets = append(buckets, nil)
			}
			order = append(order, uint64(id))
			event.context = nil
			buckets[id] = append(buckets[id], event)
		}
	}
	out = binary.AppendUvarint(out, uint64(len(contexts)))
	out = append(out, c.encodeContextDictionary(contexts)...)
	out = append(out, c.pack(c.encodeNumbers(order))...)
	blocks, ok := c.packRecordBuckets(buckets)
	if !ok {
		return nil
	}
	out = binary.AppendUvarint(out, uint64(len(blocks)))
	for _, block := range blocks {
		out = appendRecordString(out, string(block))
	}
	return recordSegmentEnvelope(3, out)
}

func (c *recordBlockCodec) packRecordBuckets(buckets [][]recordEvent) ([][]byte, bool) {
	stream := recordStream{codec: c}
	var blocks [][]byte
	for _, events := range buckets {
		for _, event := range events {
			block, err := stream.add(event)
			if err != nil {
				return nil, false
			}
			if block != nil {
				blocks = append(blocks, block)
			}
		}
	}
	last, err := stream.flush()
	if err != nil {
		return nil, false
	}
	if last != nil {
		blocks = append(blocks, last)
	}
	return blocks, len(blocks) <= c.segmentBlockLimit()
}

func (c *recordBlockCodec) readRecordBuckets(cursor *recordCursor) []recordEvent {
	counts := make([]int, cursor.number(recordSegmentMaxBlocks))
	total := 0
	for i := range counts {
		counts[i] = cursor.number(recordEventLimit)
		total += counts[i]
		if counts[i] == 0 {
			cursor.fail("empty original microblock")
		}
	}
	contexts := c.readRecordContexts(cursor)
	references := recordCursor{data: c.unpack(cursor)}
	order := readRecordNumbers(&references, total)
	if err := references.finish(); err != nil {
		cursor.fail("bucket order: " + err.Error())
	}
	var sorted []recordEvent
	for range cursor.number(recordSegmentMaxBlocks) {
		block := cursor.take(cursor.number(recordByteLimit + 64))
		events, err := c.decode(block)
		if err != nil {
			cursor.fail("bucket microblock: " + err.Error())
			return nil
		}
		sorted = append(sorted, events...)
		if len(sorted) > total {
			cursor.fail("bucket event count")
			return nil
		}
	}
	if cursor.err != nil || len(sorted) != total || total == 0 {
		cursor.fail("bucket event count")
		return nil
	}
	events := restoreRecordBucketOrder(cursor, sorted, order, contexts)
	if cursor.err != nil {
		return nil
	}
	start := 0
	for _, count := range counts {
		if _, err := rawRecordEvents(events[start : start+count]); err != nil {
			cursor.fail("restored microblock: " + err.Error())
			return nil
		}
		start += count
	}
	return events
}

func restoreRecordBucketOrder(cursor *recordCursor, sorted []recordEvent, order []uint64, contexts [][]recordField) []recordEvent {
	positions := make([]int, len(contexts))
	for _, id := range order {
		if id >= uint64(len(contexts)) {
			cursor.fail("bucket context reference")
			return nil
		}
		positions[id]++
	}
	start := 0
	for i, count := range positions {
		positions[i] = start
		start += count
	}
	events := make([]recordEvent, len(order))
	for i, id := range order {
		event := sorted[positions[id]]
		positions[id]++
		if len(event.context) != 0 {
			cursor.fail("bucket has an inline context")
			return nil
		}
		event.context = slices.Clone(contexts[id])
		events[i] = event
	}
	return events
}
