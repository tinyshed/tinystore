package records

import (
	"encoding/binary"
	"hash/crc32"
	"slices"
	"strconv"
)

// segmentVersion is the first byte of a segment row and of each of its blocks
const segmentVersion = 1

// a shape's presence names what its records carry besides their attributes
const (
	hasLevel byte = 1 << iota
	hasBody
	hasTrace
	hasSpan
	hasContext
	rawAttrs // the attributes kept as one serialized value
)

// a block holds its columns in slot order: time, what the segment's records
// carry, one slot per attribute column, and bodies last
const (
	slotTime byte = iota
	slotName
	slotShape
	slotContext
	slotLevel
	slotBody
	slotTrace
	slotSpan
	slotRaw
	slotAttr
)

// shape is a record's presence and ordered attribute keys. An attribute
// column is a key and its occurrence, so repeated keys keep their places:
//
//	keys    user  tag  tag
//	columns  0     1    2      where another shape's "tag" is column 1 too
type shape struct {
	presence byte
	keys     []string
	columns  []int
}

type slot struct {
	kind   byte
	column int // an attribute slot's column
}

// schema is what every block of a segment shares, which its row stores
type schema struct {
	stream     string
	count      int
	names      []string
	shapes     []shape
	contexts   [][]Field
	columns    int
	columnKeys []string
	slots      []slot
	attrSlot   int     // the first attribute slot
	shapeSlots [][]int // the slots a record of each shape has a value in
}

// derive numbers the attribute columns and lays out the slots; a decoder
// derives them again from the row, so they are never stored
func (s *schema) derive() {
	presence := s.numberColumns()
	if s.columns > maxAttrColumns {
		return
	}
	s.chooseSlots(presence)
	s.columnKeys = make([]string, s.columns)
	for _, shape := range s.shapes {
		for position, column := range shape.columns {
			s.columnKeys[column] = shape.keys[position]
		}
	}
	s.shapeSlots = make([][]int, len(s.shapes))
	for id := range s.shapes {
		s.shapeSlots[id] = s.slotsOf(&s.shapes[id])
	}
}

// numberColumns gives each key and occurrence a column in order of first use,
// and returns what any shape carries
func (s *schema) numberColumns() byte {
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
	}
	s.columns = len(ids)
	return presence
}

func (s *schema) chooseSlots(presence byte) {
	present := map[byte]bool{
		slotName: len(s.names) > 1, slotShape: len(s.shapes) > 1, slotContext: presence&hasContext != 0,
		slotLevel: presence&hasLevel != 0, slotTrace: presence&hasTrace != 0,
		slotSpan: presence&hasSpan != 0, slotRaw: presence&rawAttrs != 0,
	}
	s.slots = append(s.slots[:0], slot{kind: slotTime})
	for _, kind := range []byte{slotName, slotShape, slotContext, slotLevel, slotTrace, slotSpan, slotRaw} {
		if present[kind] {
			s.slots = append(s.slots, slot{kind: kind})
		}
	}
	s.attrSlot = len(s.slots)
	for column := range s.columns {
		s.slots = append(s.slots, slot{kind: slotAttr, column: column})
	}
	if presence&hasBody != 0 {
		s.slots = append(s.slots, slot{kind: slotBody})
	}
}

func (s *schema) slotsOf(shape *shape) []int {
	var slots []int
	for index := range s.attrSlot {
		if carries(shape, s.slots[index].kind) {
			slots = append(slots, index)
		}
	}
	for _, column := range shape.columns {
		slots = append(slots, s.attrSlot+column)
	}
	if shape.presence&hasBody != 0 {
		slots = append(slots, len(s.slots)-1)
	}
	return slots
}

func carries(shape *shape, kind byte) bool {
	switch kind {
	case slotContext:
		return shape.presence&hasContext != 0
	case slotLevel:
		return shape.presence&hasLevel != 0
	case slotTrace:
		return shape.presence&hasTrace != 0
	case slotSpan:
		return shape.presence&hasSpan != 0
	case slotRaw:
		return shape.presence&rawAttrs != 0
	}
	return true
}

// slotOf is the index of a slot, or -1 when the segment's records never carry it
func (s *schema) slotOf(kind byte, column int) int {
	return slices.IndexFunc(s.slots, func(slot slot) bool { return slot.kind == kind && slot.column == column })
}

// columnsOf are the attribute columns of a key, one per occurrence
func (s *schema) columnsOf(key string) []int {
	var columns []int
	for column, columnKey := range s.columnKeys {
		if columnKey == key {
			columns = append(columns, column)
		}
	}
	return columns
}

func (e *encoder) appendSchema(out []byte, s *schema) []byte {
	out = appendCount(append(out, segmentVersion), s.count)
	out = appendString(out, s.stream)
	out = appendStrings(out, s.names)
	out = appendCount(out, len(s.shapes))
	for _, shape := range s.shapes {
		out = appendStrings(append(out, shape.presence), shape.keys)
	}
	out = e.appendContexts(out, s.contexts)
	return binary.LittleEndian.AppendUint32(out, crc32.ChecksumIEEE(out))
}

// appendContexts stores contexts as columns, grouped by their ordered keys:
//
//	{service api, host a}  {service api, host b}  {session 7}
//	keys [service host] [session]   owners 0 0 1
//	service "api" "api"   host "a" "b"   session "7"
func (e *encoder) appendContexts(out []byte, contexts [][]Field) []byte {
	out = appendCount(out, len(contexts))
	if len(contexts) == 0 {
		return out
	}
	keyLists, owners := groupContexts(contexts)
	out = appendCount(out, len(keyLists))
	for _, keys := range keyLists {
		out = appendStrings(out, keys)
	}
	out = e.appendInts(out, owners)
	members := make([][]int, len(keyLists))
	for i, owner := range owners {
		members[owner] = append(members[owner], i)
	}
	for list, keys := range keyLists {
		values := make([]string, len(members[list]))
		for position := range keys {
			for j, i := range members[list] {
				values[j] = contexts[i][position].Value
			}
			out = e.appendValues(out, values)
		}
	}
	return out
}

// groupContexts finds each distinct list of keys, and which list each context has
func groupContexts(contexts [][]Field) (keyLists [][]string, owners []int64) {
	owners = make([]int64, len(contexts))
	found := map[string]int{}
	for i, fields := range contexts {
		keys := make([]string, len(fields))
		for j, field := range fields {
			keys[j] = field.Key
		}
		name := string(appendStrings(nil, keys))
		list, ok := found[name]
		if !ok {
			list = len(keyLists)
			found[name] = list
			keyLists = append(keyLists, keys)
		}
		owners[i] = int64(list)
	}
	return keyLists, owners
}

// parseSchema is the one parser of a segment row
func (d *decoder) parseSchema(row []byte) (*schema, error) {
	if len(row) < 5 || row[0] != segmentVersion {
		return nil, corrupt("segment version")
	}
	end := len(row) - 4
	if crc32.ChecksumIEEE(row[:end]) != binary.LittleEndian.Uint32(row[end:]) {
		return nil, corrupt("segment checksum")
	}
	c := cursor{data: row[1:end], budget: &expansion{limit: maxSegmentExpansion}}
	s := &schema{count: c.count(maxSegmentRecords), stream: c.string(maxBlockInput)}
	s.names = c.strings(maxSegmentRecords, maxBlockInput)
	for range c.count(maxShapes) {
		s.shapes = append(s.shapes, shape{presence: c.readByte(), keys: c.strings(maxFields, maxBlockInput)})
	}
	s.contexts = d.contexts(&c)
	if err := c.finish(); err != nil {
		return nil, err
	}
	return s, s.check()
}

func (s *schema) check() error {
	if s.count == 0 || len(s.names) == 0 || len(s.shapes) == 0 {
		return corrupt("empty segment")
	}
	for _, shape := range s.shapes {
		if shape.presence&^(hasLevel|hasBody|hasTrace|hasSpan|hasContext|rawAttrs) != 0 ||
			(shape.presence&rawAttrs != 0 && len(shape.keys) > 0) {
			return corrupt("shape presence")
		}
	}
	s.derive()
	if s.columns > maxAttrColumns {
		return corrupt("attribute columns over their bound")
	}
	return nil
}

// contexts costs work in proportion to its cells, whatever its counts claim
func (d *decoder) contexts(c *cursor) [][]Field {
	count := c.count(maxSegmentRecords)
	if count == 0 || c.err != nil {
		return nil
	}
	keyLists := make([][]string, c.count(count))
	for i := range keyLists {
		keyLists[i] = c.strings(maxFields, maxBlockInput)
	}
	owners := d.ints(c, count)
	members := make([][]int, len(keyLists))
	cells := 0
	for i, owner := range owners {
		if owner < 0 || owner >= int64(len(keyLists)) || len(keyLists[owner]) == 0 {
			c.fail("context key list reference")
			return nil
		}
		members[owner] = append(members[owner], i)
		cells += len(keyLists[owner])
	}
	if c.err != nil || cells > maxContextCells {
		c.fail("context cells")
		return nil
	}
	return d.contextValues(c, keyLists, members, count)
}

func (d *decoder) contextValues(c *cursor, keyLists [][]string, members [][]int, count int) [][]Field {
	contexts := make([][]Field, count)
	for list, keys := range keyLists {
		for _, key := range keys {
			values := d.values(c, len(members[list]))
			if c.err != nil {
				return nil
			}
			for j, i := range members[list] {
				contexts[i] = append(contexts[i], Field{Key: key, Value: values[j]})
			}
		}
	}
	return contexts
}

// interner gives each name, shape and context of a segment its id; scratch
// builds the map keys, so a record seen before costs no allocation
type interner struct {
	schema   *schema
	names    map[string]int
	shapes   map[string]int
	contexts map[string]int
	columns  map[string]bool
	scratch  []byte
}

// recordIDs are the references a block stores beside each record's values
type recordIDs struct {
	names, shapes, contexts []int
}

// newSchema interns one stream's records, already in event-time order
func newSchema(stream string, records []Record) (*schema, recordIDs) {
	s := &schema{stream: stream, count: len(records)}
	n := interner{
		schema: s, names: map[string]int{}, shapes: map[string]int{},
		contexts: map[string]int{}, columns: map[string]bool{},
	}
	ids := recordIDs{make([]int, len(records)), make([]int, len(records)), make([]int, len(records))}
	for i := range records {
		ids.names[i] = n.name(records[i].Name)
		ids.shapes[i] = n.shape(&records[i])
		ids.contexts[i] = -1
		if len(records[i].Context) > 0 {
			ids.contexts[i] = n.context(records[i].Context)
		}
	}
	s.derive()
	return s, ids
}

func (n *interner) name(name string) int {
	id, ok := n.names[name]
	if !ok {
		id = len(n.schema.names)
		n.names[name] = id
		n.schema.names = append(n.schema.names, name)
	}
	return id
}

// shape serializes a record's attributes once the segment has maxTypedShapes
// shapes or its attribute columns are full
func (n *interner) shape(r *Record) int {
	presence := presenceOf(r)
	if id, ok := n.shapes[string(n.shapeName(presence, r.Attrs))]; ok {
		return id
	}
	keys := make([]string, len(r.Attrs))
	for i, field := range r.Attrs {
		keys[i] = field.Key
	}
	if len(n.schema.shapes) >= maxTypedShapes || !n.fitColumns(keys) {
		presence, keys = presence|rawAttrs, nil
		if id, ok := n.shapes[string(n.shapeName(presence, nil))]; ok {
			return id
		}
	}
	id := len(n.schema.shapes)
	n.shapes[string(n.shapeName(presence, r.Attrs[:len(keys)]))] = id
	n.schema.shapes = append(n.schema.shapes, shape{presence: presence, keys: keys})
	return id
}

func (n *interner) shapeName(presence byte, attrs []Field) []byte {
	n.scratch = appendCount(append(n.scratch[:0], presence), len(attrs))
	for _, field := range attrs {
		n.scratch = appendString(n.scratch, field.Key)
	}
	return n.scratch
}

func presenceOf(r *Record) byte {
	var presence byte
	if r.Level != nil {
		presence |= hasLevel
	}
	if r.Body != nil {
		presence |= hasBody
	}
	if r.TraceID != (TraceID{}) {
		presence |= hasTrace
	}
	if r.SpanID != (SpanID{}) {
		presence |= hasSpan
	}
	if len(r.Context) > 0 {
		presence |= hasContext
	}
	return presence
}

// fitColumns takes the columns a new shape needs, unless they would pass maxAttrColumns
func (n *interner) fitColumns(keys []string) bool {
	seen := map[string]int{}
	var added []string
	for _, key := range keys {
		name := key + "\x00" + strconv.Itoa(seen[key])
		seen[key]++
		if !n.columns[name] && !slices.Contains(added, name) {
			added = append(added, name)
		}
	}
	if len(n.columns)+len(added) > maxAttrColumns {
		return false
	}
	for _, name := range added {
		n.columns[name] = true
	}
	return true
}

func (n *interner) context(fields []Field) int {
	n.scratch = n.scratch[:0]
	for _, field := range fields {
		n.scratch = appendString(appendString(n.scratch, field.Key), field.Value)
	}
	if id, ok := n.contexts[string(n.scratch)]; ok {
		return id
	}
	id := len(n.schema.contexts)
	n.contexts[string(n.scratch)] = id
	n.schema.contexts = append(n.schema.contexts, fields)
	return id
}
