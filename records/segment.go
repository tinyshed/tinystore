package records

import (
	"cmp"
	"slices"
)

// encodedSegment is a segment row, its blocks, and the keys the index keeps of it
type encodedSegment struct {
	stream      string
	first, last int64
	count       int
	row         []byte
	blocks      []encodedBlock
	keys        []segmentKey
}

// a segment's keys let a query skip a segment without reading its row
const (
	keyName byte = iota
	keyAttr
	keyContext
)

type segmentKey struct {
	kind byte
	key  string
}

// encodeSegment sorts one stream's records by event time, equal times keeping
// their arrival order, and writes them as a segment row and its blocks:
//
//	arrival  .300 buy   .100 menu   .200 save
//	stored   .100 menu  .200 save   .300 buy      time gaps +100 +100, not −200 +100
func (e *encoder) encodeSegment(stream string, records []Record) encodedSegment {
	sortByTime(records)
	s, ids := newSchema(stream, records)
	segment := encodedSegment{
		stream: stream, count: len(records), row: e.appendSchema(nil, s), keys: keysOf(s, records),
		first: records[0].At.UnixNano(), last: records[len(records)-1].At.UnixNano(),
	}
	for start := 0; start < len(records); {
		end := blockEnd(records, start)
		block := e.encodeBlock(s, records[start:end], ids.slice(start, end))
		segment.blocks = append(segment.blocks, block)
		start = end
	}
	return segment
}

func sortByTime(records []Record) {
	slices.SortStableFunc(records, func(a, b Record) int { return cmp.Compare(a.At.UnixNano(), b.At.UnixNano()) })
}

func (ids recordIDs) slice(start, end int) recordIDs {
	return recordIDs{ids.names[start:end], ids.shapes[start:end], ids.contexts[start:end]}
}

// blockEnd cuts a block at maxBlockRecords records or maxBlockInput bytes of
// input; one record alone always fits
func blockEnd(records []Record, start int) int {
	size := 0
	for end := start; end < len(records); end++ {
		size += inputSize(&records[end])
		if end-start == maxBlockRecords || (end > start && size > maxBlockInput) {
			return end
		}
	}
	return len(records)
}

// inputSize is what a record weighs against the bounds, whatever it encodes to
func inputSize(r *Record) int {
	size := 32 + len(r.Stream) + len(r.Name)
	if r.TraceID != (TraceID{}) {
		size += len(r.TraceID)
	}
	if r.SpanID != (SpanID{}) {
		size += len(r.SpanID)
	}
	if r.Body != nil {
		size += len(*r.Body)
	}
	for _, fields := range [][]Field{r.Context, r.Attrs} {
		for _, field := range fields {
			size += 4 + len(field.Key) + len(field.Value)
		}
	}
	return size
}

// keysOf lists the event names, attribute keys and context keys a segment holds
func keysOf(s *schema, records []Record) []segmentKey {
	seen := map[segmentKey]bool{}
	add := func(kind byte, key string) {
		seen[segmentKey{kind: kind, key: key}] = true
	}
	for _, name := range s.names {
		add(keyName, name)
	}
	for _, key := range s.columnKeys {
		add(keyAttr, key)
	}
	for _, fields := range s.contexts {
		for _, field := range fields {
			add(keyContext, field.Key)
		}
	}
	for i := range records {
		for _, field := range records[i].Attrs {
			add(keyAttr, field.Key)
		}
	}
	keys := make([]segmentKey, 0, len(seen))
	for key := range seen {
		keys = append(keys, key)
	}
	slices.SortFunc(keys, func(a, b segmentKey) int {
		return cmp.Or(cmp.Compare(a.kind, b.kind), cmp.Compare(a.key, b.key))
	})
	return keys
}
