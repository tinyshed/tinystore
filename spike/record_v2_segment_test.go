package spike

import (
	"bytes"
	"cmp"
	"encoding/binary"
	"errors"
	"hash/crc32"
	"slices"
	"strconv"
)

// a segment is one stream's records sorted by event time: one row for what its
// blocks share, one row per block of at most 1024 records
//
//	arrival  .300 buy   .100 menu   .200 save
//	stored   .100 menu  .200 save   .300 buy     equal times keep arrival order
const (
	v2Version       = 1
	v2SegmentEvents = recordSegmentMaxBlocks * recordEventLimit
	v2SegmentBytes  = recordSegmentMaxBlocks * recordByteLimit
)

const (
	v2HasLevel = 1 << iota
	v2HasBody
	v2HasTrace
	v2HasSpan
	v2HasContext
	v2RawAttrs
)

// past this many shapes a record keeps its attributes as one serialized value
const (
	v2ShapeLimit   = recordShapeLimit - 64
	v2ContextCells = 1 << 20
)

const (
	v2SlotTime byte = iota
	v2SlotName
	v2SlotShape
	v2SlotContext
	v2SlotLevel
	v2SlotBody
	v2SlotTrace
	v2SlotSpan
	v2SlotRaw
	v2SlotAttr
)

var errV2Record = errors.New("invalid v2 record data")

type v2Shape struct {
	presence byte
	keys     []string
	columns  []int
}

type v2Slot struct {
	kind   byte
	column int
}

type v2Schema struct {
	dictionary []byte
	stream     string
	count      int
	names      []string
	shapes     []v2Shape
	contexts   [][]recordField
	columns    int
	slots      []v2Slot
	shapeSlots [][]int
	attrSlots  int
	columnKeys []string
}

type v2Block struct {
	first, last int64
	count       int
	levels      int64
	traces      []byte
	filters     []v2Filter
	body        []byte
}

// v2Filter is a bloom filter over one id-like attribute of one block
type v2Filter struct {
	key   string
	bloom []byte
}

type v2Segment struct {
	schema v2Schema
	first  int64
	last   int64
	row    []byte
	blocks []v2Block
}

// v2Ids are the per-record references a block stores beside its values
type v2Ids struct {
	names, shapes, contexts []int
}

func v2Presence(event *recordEvent) byte {
	presence := event.presence()
	if len(event.context) > 0 {
		presence |= v2HasContext
	}
	return presence
}

func v2HashString(hash uint64, value string) uint64 {
	hash = (hash ^ uint64(len(value))) * 1099511628211
	for i := range len(value) {
		hash = (hash ^ uint64(value[i])) * 1099511628211
	}
	return hash
}

func v2EventBytes(event *recordEvent) int {
	size := 32 + len(event.stream) + len(event.name) + len(event.traceID) + len(event.spanID)
	if event.body != nil {
		size += len(*event.body)
	}
	for _, fields := range [][]recordField{event.context, event.attrs} {
		for _, field := range fields {
			size += 4 + len(field.key) + len(field.value)
		}
	}
	return size
}

// derive numbers attribute columns by key and occurrence, in order of first use
func (s *v2Schema) derive() {
	ids := map[string]int{}
	presence := byte(0)
	for i := range s.shapes {
		shape := &s.shapes[i]
		presence |= shape.presence
		seen := map[string]int{}
		shape.columns = shape.columns[:0]
		for _, key := range shape.keys {
			name := key + "\x00" + strconv.Itoa(seen[key])
			seen[key]++
			id, ok := ids[name]
			if !ok {
				id = len(ids)
				ids[name] = id
			}
			shape.columns = append(shape.columns, id)
		}
		if len(ids) > recordColumnLimit {
			s.columns = len(ids)
			return
		}
	}
	s.columns = len(ids)
	// bodies come last so they can be written as a function of an attribute
	present := map[byte]bool{
		v2SlotName: len(s.names) > 1, v2SlotShape: len(s.shapes) > 1, v2SlotContext: presence&v2HasContext != 0,
		v2SlotLevel: presence&v2HasLevel != 0, v2SlotTrace: presence&v2HasTrace != 0,
		v2SlotSpan: presence&v2HasSpan != 0, v2SlotRaw: presence&v2RawAttrs != 0,
	}
	s.slots = append(s.slots[:0], v2Slot{kind: v2SlotTime})
	for _, kind := range []byte{v2SlotName, v2SlotShape, v2SlotContext, v2SlotLevel, v2SlotTrace, v2SlotSpan, v2SlotRaw} {
		if present[kind] {
			s.slots = append(s.slots, v2Slot{kind: kind})
		}
	}
	s.columnKeys = make([]string, s.columns)
	for _, shape := range s.shapes {
		for position, column := range shape.columns {
			s.columnKeys[column] = shape.keys[position]
		}
	}
	s.attrSlots = len(s.slots)
	for column := range s.columns {
		s.slots = append(s.slots, v2Slot{kind: v2SlotAttr, column: column})
	}
	if presence&v2HasBody != 0 {
		s.slots = append(s.slots, v2Slot{kind: v2SlotBody})
	}
	s.shapeSlots = make([][]int, len(s.shapes))
	for id := range s.shapes {
		shape := &s.shapes[id]
		for index := range s.attrSlots {
			if v2Carries(shape, s.slots[index]) {
				s.shapeSlots[id] = append(s.shapeSlots[id], index)
			}
		}
		for _, column := range shape.columns {
			s.shapeSlots[id] = append(s.shapeSlots[id], s.attrSlots+column)
		}
		if shape.presence&v2HasBody != 0 {
			s.shapeSlots[id] = append(s.shapeSlots[id], len(s.slots)-1)
		}
	}
}

func v2Carries(shape *v2Shape, slot v2Slot) bool {
	switch slot.kind {
	case v2SlotTime, v2SlotName, v2SlotShape:
		return true
	case v2SlotContext:
		return shape.presence&v2HasContext != 0
	case v2SlotLevel:
		return shape.presence&v2HasLevel != 0
	case v2SlotBody:
		return shape.presence&v2HasBody != 0
	case v2SlotTrace:
		return shape.presence&v2HasTrace != 0
	case v2SlotSpan:
		return shape.presence&v2HasSpan != 0
	case v2SlotRaw:
		return shape.presence&v2RawAttrs != 0
	}
	return slices.Contains(shape.columns, slot.column)
}

type v2Interner struct {
	names    map[string]int
	shapes   map[uint64][]int
	contexts map[uint64][]int
	columns  map[string]bool
}

func (e *v2Encoder) schemaFor(stream string, events []recordEvent) (v2Schema, v2Ids) {
	schema := v2Schema{stream: stream, count: len(events)}
	ids := v2Ids{make([]int, len(events)), make([]int, len(events)), make([]int, len(events))}
	intern := v2Interner{map[string]int{}, map[uint64][]int{}, map[uint64][]int{}, map[string]bool{}}
	for i := range events {
		event := &events[i]
		name, ok := intern.names[event.name]
		if !ok {
			name = len(schema.names)
			intern.names[event.name] = name
			schema.names = append(schema.names, event.name)
		}
		ids.names[i] = name
		ids.shapes[i] = intern.shape(&schema, event)
		ids.contexts[i] = -1
		if len(event.context) > 0 {
			ids.contexts[i] = intern.context(&schema, event.context)
		}
	}
	schema.derive()
	return schema, ids
}

func (n *v2Interner) shape(schema *v2Schema, event *recordEvent) int {
	presence := v2Presence(event)
	if id, ok := n.find(schema, presence, event.attrs); ok {
		return id
	}
	keys := make([]string, len(event.attrs))
	for i, field := range event.attrs {
		keys[i] = field.key
	}
	if len(schema.shapes) >= v2ShapeLimit || !n.fitColumns(keys) {
		if id, ok := n.find(schema, presence|v2RawAttrs, nil); ok {
			return id
		}
		presence, keys = presence|v2RawAttrs, nil
	}
	schema.shapes = append(schema.shapes, v2Shape{presence: presence, keys: keys})
	hash := v2ShapeHash(presence, keys)
	n.shapes[hash] = append(n.shapes[hash], len(schema.shapes)-1)
	return len(schema.shapes) - 1
}

func v2ShapeHash[T any](presence byte, attrs []T) uint64 {
	hash := uint64(presence) * 1099511628211
	for _, attr := range attrs {
		switch key := any(attr).(type) {
		case string:
			hash = v2HashString(hash, key)
		case recordField:
			hash = v2HashString(hash, key.key)
		}
	}
	return hash
}

func (n *v2Interner) find(schema *v2Schema, presence byte, attrs []recordField) (int, bool) {
	for _, id := range n.shapes[v2ShapeHash(presence, attrs)] {
		shape := &schema.shapes[id]
		if shape.presence == presence && slices.EqualFunc(shape.keys, attrs,
			func(key string, field recordField) bool { return key == field.key }) {
			return id, true
		}
	}
	return 0, false
}

func (n *v2Interner) fitColumns(keys []string) bool {
	seen := map[string]int{}
	var added []string
	for _, key := range keys {
		name := key + "\x00" + strconv.Itoa(seen[key])
		seen[key]++
		if !n.columns[name] {
			added = append(added, name)
		}
	}
	if len(n.columns)+len(added) > recordColumnLimit {
		return false
	}
	for _, name := range added {
		n.columns[name] = true
	}
	return true
}

func (n *v2Interner) context(schema *v2Schema, fields []recordField) int {
	hash := uint64(14695981039346656037)
	for _, field := range fields {
		hash = v2HashString(v2HashString(hash, field.key), field.value)
	}
	for _, id := range n.contexts[hash] {
		if slices.Equal(schema.contexts[id], fields) {
			return id
		}
	}
	schema.contexts = append(schema.contexts, fields)
	n.contexts[hash] = append(n.contexts[hash], len(schema.contexts)-1)
	return len(schema.contexts) - 1
}

// encodeSegment sorts one stream's batch by event time unless arrival is asked for
func (e *v2Encoder) encodeSegment(events []recordEvent, arrival bool) (v2Segment, []recordEvent, error) {
	if len(events) == 0 || len(events) > v2SegmentEvents {
		return v2Segment{}, nil, errors.New("v2 segment event count")
	}
	stored := slices.Clone(events)
	if !arrival {
		slices.SortStableFunc(stored, func(a, b recordEvent) int { return cmp.Compare(a.at, b.at) })
	}
	bytes := 0
	for i := range stored {
		if err := checkRecordEvent(stored[i]); err != nil || stored[i].stream != stored[0].stream {
			return v2Segment{}, nil, errors.New("v2 segment event or stream")
		}
		bytes += v2EventBytes(&stored[i])
	}
	if bytes > v2SegmentBytes {
		return v2Segment{}, nil, errors.New("v2 segment byte limit")
	}
	schema, ids := e.schemaFor(stored[0].stream, stored)
	schema.dictionary = v2TextSample(stored, e.dictionary)
	if err := e.useDictionary(nil); err != nil {
		return v2Segment{}, nil, err
	}
	segment := v2Segment{schema: schema, row: e.appendSchema(nil, &schema)}
	if err := e.useDictionary(schema.dictionary); err != nil {
		return v2Segment{}, nil, err
	}
	defer func() { _ = e.useDictionary(nil) }()
	segment.first, segment.last = stored[0].at, stored[0].at
	for start := 0; start < len(stored); {
		end := v2BlockEnd(stored, start, e.blockEvents, e.blockBytes)
		block := e.encodeBlock(&segment.schema, stored[start:end], v2Ids{
			ids.names[start:end], ids.shapes[start:end], ids.contexts[start:end],
		})
		segment.first, segment.last = min(segment.first, block.first), max(segment.last, block.last)
		segment.blocks = append(segment.blocks, block)
		start = end
	}
	return segment, stored, nil
}

func v2BlockEnd(events []recordEvent, start, limit, byteLimit int) int {
	bytes := 0
	for end := start; end < len(events); end++ {
		bytes += v2EventBytes(&events[end])
		if end-start == limit || (end > start && bytes > byteLimit) {
			return end
		}
	}
	return len(events)
}

func v2AppendStrings(out []byte, values []string) []byte {
	out = binary.AppendUvarint(out, uint64(len(values)))
	for _, value := range values {
		out = appendRecordString(out, value)
	}
	return out
}

func (e *v2Encoder) appendSchema(out []byte, schema *v2Schema) []byte {
	out = append(out, 'R', 'V', '2', 'S', v2Version)
	out = binary.AppendUvarint(out, uint64(schema.count))
	out = appendRecordString(out, schema.stream)
	out = v2AppendStrings(out, schema.names)
	out = binary.AppendUvarint(out, uint64(len(schema.shapes)))
	for _, shape := range schema.shapes {
		out = v2AppendStrings(append(out, shape.presence), shape.keys)
	}
	out = e.appendContexts(out, schema.contexts)
	out = binary.AppendUvarint(out, uint64(len(schema.dictionary)))
	if len(schema.dictionary) > 0 {
		out = e.appendBlob(out, schema.dictionary)
	}
	return binary.LittleEndian.AppendUint32(out, crc32.ChecksumIEEE(out))
}

// contexts are stored as columns, grouped by their ordered keys
func (e *v2Encoder) appendContexts(out []byte, contexts [][]recordField) []byte {
	out = binary.AppendUvarint(out, uint64(len(contexts)))
	if len(contexts) == 0 {
		return out
	}
	var shapes [][]string
	owners := make([]int64, len(contexts))
	for i, fields := range contexts {
		shape := slices.IndexFunc(shapes, func(keys []string) bool {
			return slices.EqualFunc(keys, fields, func(key string, field recordField) bool { return key == field.key })
		})
		if shape < 0 {
			shape = len(shapes)
			keys := make([]string, len(fields))
			for j, field := range fields {
				keys[j] = field.key
			}
			shapes = append(shapes, keys)
		}
		owners[i] = int64(shape)
	}
	out = binary.AppendUvarint(out, uint64(len(shapes)))
	for _, keys := range shapes {
		out = v2AppendStrings(out, keys)
	}
	out = e.appendInts(out, owners)
	for shape, keys := range shapes {
		for position := range keys {
			var values []string
			for i, owner := range owners {
				if owner == int64(shape) {
					values = append(values, contexts[i][position].value)
				}
			}
			out = e.appendValues(out, values)
		}
	}
	return out
}

func (e *v2Encoder) encodeBlock(schema *v2Schema, events []recordEvent, ids v2Ids) v2Block {
	block := v2Block{first: events[0].at, last: events[len(events)-1].at, count: len(events)}
	columns := v2Gather(schema, events, ids)
	for _, event := range events {
		block.first, block.last = min(block.first, event.at), max(block.last, event.at)
		if event.level != nil {
			block.levels |= v2LevelBit(*event.level)
		}
	}
	block.traces = columns.traces
	for column, values := range columns.attrs {
		if v2IDLike(values) {
			block.filters = append(block.filters, v2Filter{schema.columnKeys[column], v2BloomValues(values)})
		}
	}
	var payload []byte
	lengths := make([]int, len(schema.slots))
	carriers := v2Carriers(schema, ids.shapes)
	for i, slot := range schema.slots {
		before := len(payload)
		payload = e.appendSlot(payload, slot, &columns)
		if v2IsValueSlot(slot) {
			payload = e.predictSlot(payload, before, i, schema, &columns, carriers)
		}
		lengths[i] = len(payload) - before
		if lengths[i] > 0 && payload[before]&v2PredictedFlag != 0 {
			carriers[i] = "predicted"
		}
	}
	body := binary.AppendUvarint([]byte{v2Version}, uint64(len(events)))
	for _, length := range lengths {
		body = binary.AppendUvarint(body, uint64(length))
	}
	body = append(body, payload...)
	block.body = binary.LittleEndian.AppendUint32(body, crc32.ChecksumIEEE(body))
	return block
}

// v2LevelBit maps slog's -4, 0, 4, 8 to bits 0..3 and clamps the rest into 0..7
func v2LevelBit(level int64) int64 {
	bucket := min(7, max(0, (level+4)/4))
	if level < -4 {
		bucket = 0
	}
	return 1 << bucket
}

type v2Columns struct {
	times, names, shapes, contexts, levels []int64
	bodies, raws                           []string
	traces, spans                          []byte
	attrs                                  [][]string
}

func v2Gather(schema *v2Schema, events []recordEvent, ids v2Ids) v2Columns {
	columns := v2Columns{attrs: make([][]string, schema.columns)}
	for i := range events {
		event := &events[i]
		columns.times = append(columns.times, event.at)
		columns.names = append(columns.names, int64(ids.names[i]))
		columns.shapes = append(columns.shapes, int64(ids.shapes[i]))
		if ids.contexts[i] >= 0 {
			columns.contexts = append(columns.contexts, int64(ids.contexts[i]))
		}
		if event.level != nil {
			columns.levels = append(columns.levels, *event.level)
		}
		if event.body != nil {
			columns.bodies = append(columns.bodies, *event.body)
		}
		columns.traces = append(columns.traces, event.traceID...)
		columns.spans = append(columns.spans, event.spanID...)
		if schema.shapes[ids.shapes[i]].presence&v2RawAttrs != 0 {
			columns.raws = append(columns.raws, string(appendRecordFields(nil, event.attrs)))
			continue
		}
		for position, field := range event.attrs {
			column := schema.shapes[ids.shapes[i]].columns[position]
			columns.attrs[column] = append(columns.attrs[column], field.value)
		}
	}
	return columns
}

func (e *v2Encoder) appendSlot(out []byte, slot v2Slot, columns *v2Columns) []byte {
	switch slot.kind {
	case v2SlotTime:
		return e.appendInts(out, columns.times)
	case v2SlotName:
		return e.appendInts(out, columns.names)
	case v2SlotShape:
		return e.appendInts(out, columns.shapes)
	case v2SlotContext:
		return v2AppendIfAny(out, columns.contexts, e.appendInts)
	case v2SlotLevel:
		return v2AppendIfAny(out, columns.levels, e.appendInts)
	case v2SlotBody:
		return v2AppendIfAny(out, columns.bodies, e.appendValues)
	case v2SlotTrace:
		return e.appendBytes(out, columns.traces, 16)
	case v2SlotSpan:
		return e.appendBytes(out, columns.spans, 8)
	case v2SlotRaw:
		return v2AppendIfAny(out, columns.raws, e.appendText)
	}
	return v2AppendIfAny(out, columns.attrs[slot.column], e.appendValues)
}

func v2AppendIfAny[T any](out []byte, values []T, appendColumn func([]byte, []T) []byte) []byte {
	if len(values) == 0 {
		return out
	}
	return appendColumn(out, values)
}

func (e *v2Encoder) appendBytes(out, flat []byte, width int) []byte {
	if len(flat) == 0 {
		return out
	}
	fixed := make([]string, len(flat)/width)
	for i := range fixed {
		fixed[i] = string(flat[i*width : (i+1)*width])
	}
	return e.appendText(out, fixed)
}

func v2ReadStrings(cursor *recordCursor, limit int) []string {
	values := make([]string, cursor.number(limit))
	for i := range values {
		values[i] = cursor.text()
	}
	return values
}

func (d *v2Decoder) decodeSchema(row []byte) (v2Schema, error) {
	if len(row) < 9 || !bytes.Equal(row[:5], []byte{'R', 'V', '2', 'S', v2Version}) {
		return v2Schema{}, errors.New("v2 segment header")
	}
	end := len(row) - 4
	if crc32.ChecksumIEEE(row[:end]) != binary.LittleEndian.Uint32(row[end:]) {
		return v2Schema{}, errors.New("v2 segment checksum")
	}
	expanded := 0
	cursor := recordCursor{data: row[5:end], expanded: &expanded}
	schema := v2Schema{count: cursor.number(v2SegmentEvents), stream: cursor.text()}
	schema.names = v2ReadStrings(&cursor, v2SegmentEvents)
	for range cursor.number(recordShapeLimit) {
		shape := v2Shape{presence: byte(cursor.number(0x3f))}
		shape.keys = v2ReadStrings(&cursor, recordFieldLimit)
		schema.shapes = append(schema.shapes, shape)
	}
	schema.contexts = d.contexts(&cursor)
	if size := cursor.number(2 * v2TextDictionary); size > 0 {
		schema.dictionary = bytes.Clone(d.blob(&cursor, size))
	}
	if err := cursor.finish(); err != nil {
		return v2Schema{}, err
	}
	if schema.count == 0 || len(schema.names) == 0 || len(schema.shapes) == 0 {
		return v2Schema{}, errors.New("v2 segment is empty")
	}
	schema.derive()
	if schema.columns > recordColumnLimit {
		return v2Schema{}, errors.New("v2 segment column limit")
	}
	return schema, nil
}

// contexts cost work in proportion to their cells, whatever the counts claim
func (d *v2Decoder) contexts(cursor *recordCursor) [][]recordField {
	count := cursor.number(v2SegmentEvents)
	if count == 0 || cursor.err != nil {
		return nil
	}
	shapes := make([][]string, cursor.number(min(count, recordShapeLimit)))
	for i := range shapes {
		shapes[i] = v2ReadStrings(cursor, recordFieldLimit)
	}
	owners := d.ints(cursor, count, false)
	members := make([][]int, len(shapes))
	cells := 0
	for i, owner := range owners {
		if owner < 0 || owner >= int64(len(shapes)) || len(shapes[owner]) == 0 {
			cursor.fail("context shape reference")
			return nil
		}
		members[owner] = append(members[owner], i)
		cells += len(shapes[owner])
	}
	if cursor.err != nil || cells > v2ContextCells {
		cursor.fail("context cells")
		return nil
	}
	contexts := make([][]recordField, count)
	for shape, keys := range shapes {
		for _, key := range keys {
			values := d.values(cursor, len(members[shape]))
			if cursor.err != nil {
				return nil
			}
			for j, i := range members[shape] {
				contexts[i] = append(contexts[i], recordField{key, values[j]})
			}
		}
	}
	return contexts
}

// v2Opened is a block whose directory is read and whose columns decode on demand
type v2Opened struct {
	schema   *v2Schema
	count    int
	payloads [][]byte
	counts   []int
	shapes   []int64
	names    []int64
	expanded int
}

func (d *v2Decoder) openBlock(schema *v2Schema, body []byte) (*v2Opened, error) {
	if len(body) < 6 || body[0] != v2Version {
		return nil, errors.New("v2 block header")
	}
	if err := d.useDictionary(schema.dictionary); err != nil {
		return nil, err
	}
	end := len(body) - 4
	if crc32.ChecksumIEEE(body[:end]) != binary.LittleEndian.Uint32(body[end:]) {
		return nil, errors.New("v2 block checksum")
	}
	cursor := recordCursor{data: body[1:end]}
	block := &v2Opened{schema: schema, count: cursor.number(v2SegmentEvents)}
	lengths := make([]int, len(schema.slots))
	for i := range lengths {
		lengths[i] = cursor.number(recordWorkLimit)
	}
	for _, length := range lengths {
		block.payloads = append(block.payloads, cursor.take(length))
	}
	if err := cursor.finish(); err != nil || block.count == 0 {
		return nil, errors.Join(errV2Record, err)
	}
	block.shapes = d.idColumn(block, v2SlotShape, len(schema.shapes))
	block.names = d.idColumn(block, v2SlotName, len(schema.names))
	if block.shapes == nil || block.names == nil {
		return nil, errors.New("v2 block shape or name ids")
	}
	block.counts = make([]int, len(schema.slots))
	for _, id := range block.shapes {
		for _, index := range schema.shapeSlots[id] {
			block.counts[index]++
		}
	}
	return block, nil
}

func (b *v2Opened) slot(kind byte, column int) int {
	return slices.IndexFunc(b.schema.slots, func(slot v2Slot) bool { return slot.kind == kind && slot.column == column })
}

func (d *v2Decoder) idColumn(block *v2Opened, kind byte, limit int) []int64 {
	index := block.slot(kind, 0)
	if index < 0 {
		return make([]int64, block.count)
	}
	cursor := recordCursor{data: block.payloads[index]}
	ids := d.ints(&cursor, block.count, false)
	if cursor.finish() != nil {
		return nil
	}
	for _, id := range ids {
		if id < 0 || id >= int64(limit) {
			return nil
		}
	}
	return ids
}

func (d *v2Decoder) intSlot(block *v2Opened, index int) ([]int64, error) {
	count := block.counts[index]
	cursor := recordCursor{data: block.payloads[index]}
	if count == 0 {
		return nil, cursor.finish()
	}
	values := d.ints(&cursor, count, false)
	return values, cursor.finish()
}

func (d *v2Decoder) valueSlot(block *v2Opened, index int) ([]string, error) {
	count := block.counts[index]
	cursor := recordCursor{data: block.payloads[index], expanded: &block.expanded}
	if count == 0 {
		return nil, cursor.finish()
	}
	if len(cursor.data) > 0 && cursor.data[0]&v2PredictedFlag != 0 {
		values := d.predicted(block, index, &cursor, count)
		return values, cursor.finish()
	}
	values := d.values(&cursor, count)
	return values, cursor.finish()
}

func (d *v2Decoder) textSlot(block *v2Opened, index int) ([]string, error) {
	cursor := recordCursor{data: block.payloads[index], expanded: &block.expanded}
	if block.counts[index] == 0 {
		return nil, cursor.finish()
	}
	values := d.text(&cursor, block.counts[index])
	return values, cursor.finish()
}

func (d *v2Decoder) bytesSlot(block *v2Opened, index, width int) ([][]byte, error) {
	count := block.counts[index]
	cursor := recordCursor{data: block.payloads[index], expanded: &block.expanded}
	if count == 0 {
		return nil, cursor.finish()
	}
	texts := d.text(&cursor, count)
	values := make([][]byte, len(texts))
	for i, text := range texts {
		if len(text) != width {
			cursor.fail("fixed id width")
		}
		values[i] = []byte(text)
	}
	return values, cursor.finish()
}

func (d *v2Decoder) decodeBlock(schema *v2Schema, body []byte) ([]recordEvent, error) {
	block, err := d.openBlock(schema, body)
	if err != nil {
		return nil, err
	}
	return d.events(block, nil)
}

// v2Decoded holds every column of one block, each consumed in record order
type v2Decoded struct {
	ints   [][]int64
	values [][]string
	bytes  [][][]byte
	next   []int
}

func (d *v2Decoder) decodeSlots(block *v2Opened) (*v2Decoded, error) {
	slots := block.schema.slots
	decoded := &v2Decoded{
		ints: make([][]int64, len(slots)), values: make([][]string, len(slots)),
		bytes: make([][][]byte, len(slots)), next: make([]int, len(slots)),
	}
	var err error
	for index, slot := range slots {
		switch slot.kind {
		case v2SlotName, v2SlotShape:
			continue
		case v2SlotTime, v2SlotContext, v2SlotLevel:
			decoded.ints[index], err = d.intSlot(block, index)
		case v2SlotTrace:
			decoded.bytes[index], err = d.bytesSlot(block, index, 16)
		case v2SlotSpan:
			decoded.bytes[index], err = d.bytesSlot(block, index, 8)
		case v2SlotRaw:
			decoded.values[index], err = d.textSlot(block, index)
		default:
			decoded.values[index], err = d.valueSlot(block, index)
		}
		if err != nil {
			return nil, err
		}
	}
	return decoded, nil
}

// events rebuilds the records at the ascending rows, or every record when rows is nil
func (d *v2Decoder) events(block *v2Opened, rows []int) ([]recordEvent, error) {
	decoded, err := d.decodeSlots(block)
	if err != nil {
		return nil, err
	}
	var events []recordEvent
	for row := range block.count {
		event, err := block.rebuild(decoded, row)
		if err != nil {
			return nil, err
		}
		if rows == nil || (len(rows) > 0 && rows[0] == row) {
			events = append(events, event)
			if rows != nil {
				rows = rows[1:]
			}
		}
	}
	return events, nil
}

func (b *v2Opened) rebuild(decoded *v2Decoded, row int) (recordEvent, error) {
	schema := b.schema
	shape := &schema.shapes[b.shapes[row]]
	event := recordEvent{stream: schema.stream, name: schema.names[b.names[row]]}
	for _, index := range schema.shapeSlots[b.shapes[row]] {
		next := decoded.next[index]
		switch slot := schema.slots[index]; slot.kind {
		case v2SlotAttr:
			continue
		case v2SlotTime:
			event.at = decoded.ints[index][next]
		case v2SlotContext:
			id := decoded.ints[index][next]
			if id < 0 || id >= int64(len(schema.contexts)) {
				return event, errors.New("v2 context reference")
			}
			event.context = slices.Clone(schema.contexts[id])
		case v2SlotLevel:
			event.level = &decoded.ints[index][next]
		case v2SlotBody:
			event.body = &decoded.values[index][next]
		case v2SlotTrace:
			event.traceID = decoded.bytes[index][next]
		case v2SlotSpan:
			event.spanID = decoded.bytes[index][next]
		case v2SlotRaw:
			fields := recordCursor{data: []byte(decoded.values[index][next])}
			event.attrs = fields.fields()
			if err := fields.finish(); err != nil {
				return event, err
			}
		}
		decoded.next[index]++
	}
	for position, column := range shape.columns {
		index := schema.attrSlots + column
		event.attrs = append(event.attrs, recordField{shape.keys[position], decoded.values[index][decoded.next[index]]})
		decoded.next[index]++
	}
	return event, nil
}

// v2TextSample takes bodies at an even stride until the sample is full; a segment with
// less than four samples of text gets none
func v2TextSample(events []recordEvent, limit int) []byte {
	total := 0
	for i := range events {
		if events[i].body != nil {
			total += len(*events[i].body) + 1
		}
	}
	if limit <= 0 || total < 4*limit {
		return nil
	}
	stride, next, seen := total/limit, 0, 0
	var sample []byte
	for i := range events {
		if events[i].body == nil || len(sample) >= limit {
			continue
		}
		if seen >= next {
			sample = append(append(sample, *events[i].body...), '\n')
			next += stride * (len(*events[i].body) + 1)
		}
		seen += len(*events[i].body) + 1
	}
	return sample[:min(len(sample), limit)]
}
