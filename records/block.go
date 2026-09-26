package records

import (
	"encoding/binary"
	"hash/crc32"
	"log/slog"
	"slices"
	"strings"
	"time"
)

// a block row is its records' columns in the schema's slot order, readable
// with its segment row and nothing else:
//
//	version | count | each slot's length | each slot's column | crc-32
//
// encodedBlock is one block row, and what its index row and filters keep
type encodedBlock struct {
	first, last int64
	count       int
	levels      int64
	body        []byte
	traces      []byte // a bloom over its trace ids; nil when it has none
	filters     []blockFilter
}

// blockFilter is a bloom over the values of one id-like attribute key
type blockFilter struct {
	key   string
	bloom []byte
}

// columns are one block's values, gathered by slot, and the record times of
// the values a time may be kept against
type columns struct {
	times, names, shapes, contexts, levels []int64
	bodies, raws, traces, spans            []string
	attrs                                  [][]string // by attribute column
	bodyTimes                              []int64
	attrTimes                              [][]int64
}

func (e *encoder) encodeBlock(s *schema, records []Record, ids recordIDs) encodedBlock {
	gathered := gatherColumns(s, records, ids)
	block := encodedBlock{first: gathered.times[0], last: gathered.times[0], count: len(records)}
	for _, at := range gathered.times {
		block.first, block.last = min(block.first, at), max(block.last, at)
	}
	for _, level := range gathered.levels {
		block.levels |= levelBit(level)
	}
	if len(gathered.traces) > 0 {
		block.traces = bloomOf(gathered.traces)
	}
	block.filters = idFilters(s, records, ids, &gathered)
	block.body = e.appendBlock(s, &gathered)
	return block
}

func (e *encoder) appendBlock(s *schema, gathered *columns) []byte {
	var payload []byte
	lengths := make([]int, len(s.slots))
	for i, slot := range s.slots {
		before := len(payload)
		payload = e.appendSlot(payload, slot, gathered)
		lengths[i] = len(payload) - before
	}
	body := appendCount([]byte{segmentVersion}, len(gathered.times))
	for _, length := range lengths {
		body = appendCount(body, length)
	}
	body = append(body, payload...)
	return binary.LittleEndian.AppendUint32(body, crc32.ChecksumIEEE(body))
}

func gatherColumns(s *schema, records []Record, ids recordIDs) columns {
	gathered := columns{attrs: make([][]string, s.columns), attrTimes: make([][]int64, s.columns)}
	for column, count := range s.columnCounts(ids.shapes) {
		gathered.attrs[column], gathered.attrTimes[column] = make([]string, 0, count), make([]int64, 0, count)
	}
	for i := range records {
		r := &records[i]
		gathered.times = append(gathered.times, r.At.UnixNano())
		gathered.names = append(gathered.names, int64(ids.names[i]))
		gathered.shapes = append(gathered.shapes, int64(ids.shapes[i]))
		if ids.contexts[i] >= 0 {
			gathered.contexts = append(gathered.contexts, int64(ids.contexts[i]))
		}
		if r.Level != nil {
			gathered.levels = append(gathered.levels, int64(*r.Level))
		}
		if r.Body != nil {
			gathered.bodies = append(gathered.bodies, *r.Body)
			gathered.bodyTimes = append(gathered.bodyTimes, r.At.UnixNano())
		}
		if r.TraceID != (TraceID{}) {
			gathered.traces = append(gathered.traces, string(r.TraceID[:]))
		}
		if r.SpanID != (SpanID{}) {
			gathered.spans = append(gathered.spans, string(r.SpanID[:]))
		}
		shape := &s.shapes[ids.shapes[i]]
		if shape.presence&rawAttrs != 0 {
			gathered.raws = append(gathered.raws, string(appendFields(nil, r.Attrs)))
			continue
		}
		for position, field := range r.Attrs {
			column := shape.columns[position]
			gathered.attrs[column] = append(gathered.attrs[column], field.Value)
			gathered.attrTimes[column] = append(gathered.attrTimes[column], r.At.UnixNano())
		}
	}
	return gathered
}

func (e *encoder) appendSlot(out []byte, slot slot, gathered *columns) []byte {
	switch slot.kind {
	case slotTime:
		return e.appendInts(out, gathered.times)
	case slotName:
		return e.appendInts(out, gathered.names)
	case slotShape:
		return e.appendInts(out, gathered.shapes)
	case slotContext:
		return appendIfAny(out, gathered.contexts, e.appendInts)
	case slotLevel:
		return appendIfAny(out, gathered.levels, e.appendInts)
	case slotBody:
		return e.appendTimedValues(out, gathered.bodies, gathered.bodyTimes)
	case slotTrace:
		return appendIfAny(out, gathered.traces, e.appendTexts)
	case slotSpan:
		return appendIfAny(out, gathered.spans, e.appendTexts)
	case slotRaw:
		return appendIfAny(out, gathered.raws, e.appendTexts)
	}
	return e.appendTimedValues(out, gathered.attrs[slot.column], gathered.attrTimes[slot.column])
}

// appendTimedValues writes a value column whose times are its records'
func (e *encoder) appendTimedValues(out []byte, values []string, times []int64) []byte {
	if len(values) == 0 {
		return out
	}
	return e.appendValues(out, values, times)
}

// appendIfAny writes nothing for a slot no record of the block carries
func appendIfAny[T any](out []byte, values []T, appendColumn func([]byte, []T) []byte) []byte {
	if len(values) == 0 {
		return out
	}
	return appendColumn(out, values)
}

// levelBit gives slog's levels a bit of a block's level mask each, so a query
// for warnings skips a block of debug lines without reading it:
//
//	below 0  0..3  4..7  8..11  12..15  16..19  20..23  24 and above
//	bit 0    1     2     3      4       5       6       7
func levelBit(level int64) int64 {
	switch {
	case level < 0:
		return 1
	case level >= 24:
		return 1 << 7
	}
	return 1 << (level/4 + 1)
}

// levelsFrom is the mask of every bit a level of at least minimum can set
func levelsFrom(minimum int64) int64 {
	mask := int64(0)
	for bit := levelBit(minimum); bit <= 1<<7; bit <<= 1 {
		mask |= bit
	}
	return mask
}

// idFilters gives a bloom to each attribute key whose values in the block are
// id-like, serialized attributes included, so a lookup that skips the block
// never misses a value
func idFilters(s *schema, records []Record, ids recordIDs, gathered *columns) []blockFilter {
	values := map[string][]string{}
	for column, key := range s.columnKeys {
		values[key] = append(values[key], gathered.attrs[column]...)
	}
	for i := range records {
		if s.shapes[ids.shapes[i]].presence&rawAttrs == 0 {
			continue
		}
		for _, field := range records[i].Attrs {
			values[field.Key] = append(values[field.Key], field.Value)
		}
	}
	var filters []blockFilter
	for key, keyValues := range values {
		if idLike(keyValues) {
			filters = append(filters, blockFilter{key: key, bloom: bloomOf(keyValues)})
		}
	}
	slices.SortFunc(filters, func(a, b blockFilter) int { return strings.Compare(a.key, b.key) })
	return filters
}

// appendFields serializes a record's attributes when its shape keeps them whole
func appendFields(out []byte, fields []Field) []byte {
	out = appendCount(out, len(fields))
	for _, field := range fields {
		out = appendString(appendString(out, field.Key), field.Value)
	}
	return out
}

func parseFields(text string) ([]Field, error) {
	c := cursor{data: []byte(text)}
	fields := readFields(&c)
	return fields, c.finish()
}

// openedBlock is a block whose directory is read; its columns decode on demand
type openedBlock struct {
	schema   *schema
	count    int
	payloads [][]byte // each slot's column
	counts   []int    // how many values each slot holds
	shapes   []int64  // each row's shape
	names    []int64  // each row's event name
	times    []int64  // each row's time
	budget   expansion
}

// openBlock is the one parser of a block row: every read of a block, whole or
// one column, starts from what it returns
func (d *decoder) openBlock(s *schema, body []byte) (*openedBlock, error) {
	if len(body) < 5 || body[0] != segmentVersion {
		return nil, corrupt("block version")
	}
	end := len(body) - 4
	if crc32.ChecksumIEEE(body[:end]) != binary.LittleEndian.Uint32(body[end:]) {
		return nil, corrupt("block checksum")
	}
	c := cursor{data: body[1:end]}
	block := &openedBlock{schema: s, count: c.count(maxBlockRecords), budget: expansion{limit: maxExpandedText}}
	lengths := make([]int, len(s.slots))
	for i := range lengths {
		lengths[i] = c.count(len(body))
	}
	for _, length := range lengths {
		block.payloads = append(block.payloads, c.take(length))
	}
	if err := c.finish(); err != nil {
		return nil, err
	}
	if block.count == 0 {
		return nil, corrupt("empty block")
	}
	return block, d.readDirectory(block)
}

// readDirectory decodes each row's shape and name, which say how many values
// every other slot holds, and each row's time, which every read asks for
// first and a value column may be kept against
func (d *decoder) readDirectory(block *openedBlock) error {
	s := block.schema
	block.shapes = d.idColumn(block, slotShape, len(s.shapes))
	block.names = d.idColumn(block, slotName, len(s.names))
	if block.shapes == nil || block.names == nil {
		return corrupt("block shape or name ids")
	}
	block.counts = make([]int, len(s.slots))
	for _, id := range block.shapes {
		for _, index := range s.shapeSlots[id] {
			block.counts[index]++
		}
	}
	var err error
	block.times, err = d.intColumn(block, 0)
	return err
}

// idColumn reads ids that must stay below limit; a slot the segment does not
// need holds only zeros
func (d *decoder) idColumn(block *openedBlock, kind byte, limit int) []int64 {
	index := block.schema.slotOf(kind, 0)
	if index < 0 {
		return make([]int64, block.count)
	}
	c := cursor{data: block.payloads[index]}
	ids := d.ints(&c, block.count)
	if c.finish() != nil {
		return nil
	}
	for _, id := range ids {
		if id < 0 || id >= int64(limit) {
			return nil
		}
	}
	return ids
}

func (d *decoder) intColumn(block *openedBlock, index int) ([]int64, error) {
	c := cursor{data: block.payloads[index]}
	if block.counts[index] == 0 {
		return nil, c.finish()
	}
	values := d.ints(&c, block.counts[index])
	return values, c.finish()
}

func (d *decoder) valueColumn(block *openedBlock, index int) ([]string, error) {
	c := cursor{data: block.payloads[index], budget: &block.budget}
	if block.counts[index] == 0 {
		return nil, c.finish()
	}
	values := d.values(&c, block.counts[index], func() []int64 { return block.slotTimes(index) })
	return values, c.finish()
}

// slotTimes are the times of the rows that have a value in a slot
func (b *openedBlock) slotTimes(index int) []int64 {
	times := make([]int64, 0, b.counts[index])
	b.eachValue(index, func(row, _ int) { times = append(times, b.times[row]) })
	return times
}

func (d *decoder) textColumn(block *openedBlock, index, width int) ([]string, error) {
	c := cursor{data: block.payloads[index], budget: &block.budget}
	if block.counts[index] == 0 {
		return nil, c.finish()
	}
	values := d.texts(&c, block.counts[index])
	for _, value := range values {
		if width > 0 && len(value) != width {
			c.fail("fixed id width")
		}
	}
	return values, c.finish()
}

// decodedBlock holds every column of a block, each read in row order, and
// the fields its records' lists are cut from
type decodedBlock struct {
	ints   [][]int64
	values [][]string
	levels [][]slog.Level
	next   []int
	fields fieldArena
}

// fieldArena hands out the attribute and context lists of a block's records
// from shared chunks: thousands of fields cost one allocation. A list's
// capacity is its length, so appending to one never reaches its neighbour.
type fieldArena struct {
	free []Field
}

func (a *fieldArena) take(n int) []Field {
	if n > len(a.free) {
		a.free = make([]Field, max(n, 4096))
	}
	taken := a.free[:n:n]
	a.free = a.free[n:]
	return taken
}

func (d *decoder) decodeColumns(block *openedBlock) (*decodedBlock, error) {
	slots := block.schema.slots
	decoded := &decodedBlock{
		ints: make([][]int64, len(slots)), values: make([][]string, len(slots)),
		levels: make([][]slog.Level, len(slots)), next: make([]int, len(slots)),
	}
	var err error
	for index, slot := range slots {
		switch slot.kind {
		case slotName, slotShape:
			continue
		case slotTime:
			decoded.ints[index] = block.times
		case slotContext:
			decoded.ints[index], err = d.intColumn(block, index)
		case slotLevel:
			decoded.levels[index], err = d.levelColumn(block, index)
		case slotTrace:
			decoded.values[index], err = d.textColumn(block, index, len(TraceID{}))
		case slotSpan:
			decoded.values[index], err = d.textColumn(block, index, len(SpanID{}))
		case slotRaw:
			decoded.values[index], err = d.textColumn(block, index, 0)
		default:
			decoded.values[index], err = d.valueColumn(block, index)
		}
		if err != nil {
			return nil, err
		}
	}
	return decoded, nil
}

func (d *decoder) levelColumn(block *openedBlock, index int) ([]slog.Level, error) {
	values, err := d.intColumn(block, index)
	levels := make([]slog.Level, len(values))
	for i, value := range values {
		if value < minLevel || value > maxLevel {
			return nil, corrupt("level out of range")
		}
		levels[i] = slog.Level(value)
	}
	return levels, err
}

// records rebuilds the rows keep asks for, every row when keep is nil; each
// record owns what it holds except the strings, which are never changed
func (d *decoder) records(block *openedBlock, keep func(row int) bool) ([]Record, error) {
	decoded, err := d.decodeColumns(block)
	if err != nil {
		return nil, err
	}
	records := make([]Record, 0, block.count)
	for row := range block.count {
		if keep != nil && !keep(row) {
			block.skip(decoded, row)
			continue
		}
		record, err := block.rebuild(decoded, row)
		if err != nil {
			return nil, err
		}
		records = append(records, record)
	}
	return records, nil
}

func (b *openedBlock) skip(decoded *decodedBlock, row int) {
	for _, index := range b.schema.shapeSlots[b.shapes[row]] {
		decoded.next[index]++
	}
}

func (b *openedBlock) rebuild(decoded *decodedBlock, row int) (Record, error) {
	s := b.schema
	shape := &s.shapes[b.shapes[row]]
	record := Record{Stream: s.stream, Name: s.names[b.names[row]]}
	for _, index := range s.shapeSlots[b.shapes[row]] {
		if err := b.fill(&record, decoded, index); err != nil {
			return record, err
		}
		decoded.next[index]++
	}
	if shape.presence&rawAttrs == 0 {
		record.Attrs = decoded.fields.take(len(shape.columns))
		for position, column := range shape.columns {
			index := s.attrSlot + column
			value := decoded.values[index][decoded.next[index]-1]
			record.Attrs[position] = Field{Key: shape.keys[position], Value: value}
		}
	}
	return record, nil
}

// fill sets what one slot holds for the record being rebuilt
func (b *openedBlock) fill(record *Record, decoded *decodedBlock, index int) error {
	next := decoded.next[index]
	var err error
	switch b.schema.slots[index].kind {
	case slotTime:
		record.At = time.Unix(0, decoded.ints[index][next]).UTC()
	case slotContext:
		id := decoded.ints[index][next]
		if id < 0 || id >= int64(len(b.schema.contexts)) {
			return corrupt("context reference")
		}
		context := b.schema.contexts[id]
		record.Context = decoded.fields.take(len(context))
		copy(record.Context, context)
	case slotLevel:
		record.Level = &decoded.levels[index][next]
	case slotBody:
		record.Body = &decoded.values[index][next]
	case slotTrace:
		copy(record.TraceID[:], decoded.values[index][next])
	case slotSpan:
		copy(record.SpanID[:], decoded.values[index][next])
	case slotRaw:
		record.Attrs, err = parseFields(decoded.values[index][next])
	}
	return err
}
