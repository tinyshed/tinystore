package spike

import (
	"encoding/binary"
	"strconv"
)

const (
	recordShapeLimit  = 128
	recordColumnLimit = 1024
)

type recordShape struct {
	presence       byte
	context, attrs []string
	columns        []int
}

type recordColumnLayout struct {
	tuple   bool
	shapes  []recordShape
	order   []uint64
	columns [][]string
}

type recordSlot struct {
	key, value string
}

func (e recordEvent) presence() byte {
	var mask byte
	if e.level != nil {
		mask |= 1
	}
	if e.body != nil {
		mask |= 2
	}
	if e.traceID != nil {
		mask |= 4
	}
	if e.spanID != nil {
		mask |= 8
	}
	return mask
}

func recordShapeFor(event recordEvent, tuple bool) (recordShape, []recordSlot) {
	shape := recordShape{presence: event.presence()}
	var slots []recordSlot
	if event.level != nil {
		slots = append(slots, recordSlot{"level", strconv.FormatInt(*event.level, 10)})
	}
	if event.body != nil {
		slots = append(slots, recordSlot{"body", *event.body})
	}
	if event.traceID != nil {
		slots = append(slots, recordSlot{"trace", string(event.traceID)})
	}
	if event.spanID != nil {
		slots = append(slots, recordSlot{"span", string(event.spanID)})
	}
	if tuple {
		slots = append(slots, recordSlot{"context", string(appendRecordFields(nil, event.context))})
	} else {
		for _, field := range event.context {
			shape.context = append(shape.context, field.key)
			slots = append(slots, recordSlot{"context:" + field.key, field.value})
		}
	}
	for _, field := range event.attrs {
		shape.attrs = append(shape.attrs, field.key)
		slots = append(slots, recordSlot{"attr:" + field.key, field.value})
	}
	return shape, slots
}

func appendRecordShape(out []byte, shape recordShape) []byte {
	out = append(out, shape.presence)
	for _, keys := range [][]string{shape.context, shape.attrs} {
		out = binary.AppendUvarint(out, uint64(len(keys)))
		for _, key := range keys {
			out = appendRecordString(out, key)
		}
	}
	return out
}

func planRecordColumns(events []recordEvent, tuple, shared bool) (recordColumnLayout, bool) {
	layout := recordColumnLayout{tuple: tuple, columns: make([][]string, 3)}
	shapes, columns := map[string]int{}, map[string]int{}
	bodies := map[int]bool{}
	for _, event := range events {
		layout.columns[0] = append(layout.columns[0], strconv.FormatInt(event.at, 10))
		layout.columns[1] = append(layout.columns[1], event.stream)
		layout.columns[2] = append(layout.columns[2], event.name)
		shape, slots := recordShapeFor(event, tuple)
		keyBytes := appendRecordShape(nil, shape)
		if !shared {
			keyBytes = appendRecordString(keyBytes, event.stream)
			keyBytes = appendRecordString(keyBytes, event.name)
		}
		key := string(keyBytes)
		id, exists := shapes[key]
		if !exists {
			if len(layout.shapes) == recordShapeLimit {
				return recordColumnLayout{}, false
			}
			id = len(layout.shapes)
			shapes[key] = id
			for _, slot := range slots {
				columnKey := slot.key
				if !shared {
					columnKey = strconv.Itoa(id) + ":" + columnKey
				}
				column, found := columns[columnKey]
				if !found {
					if len(layout.columns) == recordColumnLimit {
						return recordColumnLayout{}, false
					}
					column = len(layout.columns)
					columns[columnKey] = column
					bodies[column] = slot.key == "body"
					layout.columns = append(layout.columns, nil)
				}
				shape.columns = append(shape.columns, column)
			}
			layout.shapes = append(layout.shapes, shape)
		}
		for i, slot := range slots {
			column := layout.shapes[id].columns[i]
			layout.columns[column] = append(layout.columns[column], slot.value)
		}
		layout.order = append(layout.order, uint64(id))
	}
	return moveRecordBodiesLast(layout, bodies), true
}

func moveRecordBodiesLast(layout recordColumnLayout, bodies map[int]bool) recordColumnLayout {
	var ordered [][]string
	remap := make([]int, len(layout.columns))
	for _, body := range []bool{false, true} {
		for i, column := range layout.columns {
			if bodies[i] == body {
				remap[i] = len(ordered)
				ordered = append(ordered, column)
			}
		}
	}
	for i, shape := range layout.shapes {
		for j, column := range shape.columns {
			layout.shapes[i].columns[j] = remap[column]
		}
	}
	layout.columns = ordered
	return layout
}

func (c *recordBlockCodec) encodeRecordLayout(layout recordColumnLayout) []byte {
	metadata := []byte{0}
	if layout.tuple {
		metadata[0] = 1
	}
	metadata = binary.AppendUvarint(metadata, uint64(len(layout.columns)))
	metadata = binary.AppendUvarint(metadata, uint64(len(layout.shapes)))
	for _, shape := range layout.shapes {
		metadata = appendRecordShape(metadata, shape)
		for _, column := range shape.columns {
			metadata = binary.AppendUvarint(metadata, uint64(column))
		}
	}
	metadata = append(metadata, c.encodeNumbers(layout.order)...)
	out := c.pack(metadata)
	previous := map[string]int{}
	for index, values := range layout.columns {
		body := c.encodeColumn(values)
		key := string(append(binary.AppendUvarint(nil, uint64(len(values))), body...))
		if alias, ok := previous[key]; ok {
			out = binary.AppendUvarint(append(out, 1), uint64(alias))
		} else {
			previous[key] = index
			candidate := append([]byte{0}, body...)
			if c.predict {
				for reference := max(0, index-8); reference < index; reference++ {
					if len(layout.columns[reference]) != len(values) {
						continue
					}
					prediction := c.predictRecordColumn(layout.columns[reference], values)
					if prediction == nil {
						continue
					}
					encoded := binary.AppendUvarint([]byte{2}, uint64(reference))
					encoded = append(encoded, c.pack(prediction)...)
					if len(encoded) < len(candidate) {
						candidate = encoded
					}
				}
			}
			out = append(out, candidate...)
		}
	}
	return out
}

func readRecordShape(cursor *recordCursor, tuple bool, columnCount int) recordShape {
	shape := recordShape{presence: byte(cursor.number(15))}
	for _, keys := range []*[]string{&shape.context, &shape.attrs} {
		for range cursor.number(recordFieldLimit) {
			*keys = append(*keys, cursor.text())
		}
	}
	slots := len(shape.context) + len(shape.attrs)
	for bit := range 4 {
		if shape.presence>>bit&1 != 0 {
			slots++
		}
	}
	if tuple {
		slots++
		if len(shape.context) != 0 {
			cursor.fail("tuple context has field columns")
		}
	}
	for range slots {
		column := cursor.number(columnCount - 1)
		if column < 3 {
			cursor.fail("field column overlaps a common column")
		}
		shape.columns = append(shape.columns, column)
	}
	return shape
}

func readRecordLayout(cursor *recordCursor, count int) (recordColumnLayout, []int) {
	layout := recordColumnLayout{tuple: cursor.number(1) == 1}
	columnCount, shapeCount := cursor.number(recordColumnLimit), cursor.number(recordShapeLimit)
	if columnCount < 3 || shapeCount == 0 {
		cursor.fail("layout counts")
		return layout, nil
	}
	for range shapeCount {
		layout.shapes = append(layout.shapes, readRecordShape(cursor, layout.tuple, columnCount))
	}
	layout.order = readRecordNumbers(cursor, count)
	counts := make([]int, columnCount)
	counts[0], counts[1], counts[2] = count, count, count
	total := 3 * count
	for _, id := range layout.order {
		if id >= uint64(len(layout.shapes)) || cursor.err != nil {
			cursor.fail("shape reference")
			return layout, nil
		}
		for _, column := range layout.shapes[id].columns {
			counts[column]++
			total++
		}
	}
	if total > recordCellLimit {
		cursor.fail("layout cell limit")
	}
	return layout, counts
}

func (c *recordBlockCodec) decodeRecordLayout(cursor *recordCursor, count int) []recordEvent {
	metadata := recordCursor{data: c.unpack(cursor)}
	layout, counts := readRecordLayout(&metadata, count)
	if err := metadata.finish(); err != nil {
		cursor.fail("layout metadata: " + err.Error())
		return nil
	}
	for index, size := range counts {
		if size == 0 || cursor.err != nil {
			cursor.fail("empty column")
			return nil
		}
		var values []string
		switch cursor.number(2) {
		case 1:
			alias := cursor.number(recordColumnLimit)
			if alias >= index || len(layout.columns[alias]) != size {
				cursor.fail("column alias")
				return nil
			}
			values = layout.columns[alias]
		case 2:
			reference := cursor.number(recordColumnLimit)
			if reference >= index || len(layout.columns[reference]) != size {
				cursor.fail("prediction reference")
				return nil
			}
			values = c.readRecordPrediction(cursor, layout.columns[reference])
		case 0:
			values = c.decodeColumn(cursor, size)
		}
		layout.columns = append(layout.columns, values)
	}
	if cursor.err != nil {
		return nil
	}
	positions := make([]int, len(layout.columns))
	take := func(column int) string {
		value := layout.columns[column][positions[column]]
		positions[column]++
		return value
	}
	events := make([]recordEvent, count)
	reconstructed := 0
	for i, id := range layout.order {
		events[i] = rebuildRecordEvent(cursor, layout.shapes[id], layout.tuple, take)
		if err := checkRecordEvent(events[i]); err != nil {
			cursor.fail("reconstructed event: " + err.Error())
			return nil
		}
		reconstructed += len(appendRecordEvent(nil, events[i]))
		if reconstructed > recordByteLimit {
			cursor.fail("reconstructed byte limit")
			return nil
		}
	}
	return events
}

func rebuildRecordEvent(cursor *recordCursor, shape recordShape, tuple bool, take func(int) string) recordEvent {
	at, err := strconv.ParseInt(take(0), 10, 64)
	if err != nil {
		cursor.fail("timestamp integer")
	}
	event := recordEvent{at: at, stream: take(1), name: take(2)}
	position := 0
	next := func() string {
		value := take(shape.columns[position])
		position++
		return value
	}
	if shape.presence&1 != 0 {
		level, levelErr := strconv.ParseInt(next(), 10, 64)
		if levelErr != nil {
			cursor.fail("level integer")
		}
		event.level = &level
	}
	if shape.presence&2 != 0 {
		body := next()
		event.body = &body
	}
	if shape.presence&4 != 0 {
		event.traceID = []byte(next())
	}
	if shape.presence&8 != 0 {
		event.spanID = []byte(next())
	}
	if tuple {
		fields := recordCursor{data: []byte(next())}
		event.context = fields.fields()
		if err = fields.finish(); err != nil {
			cursor.fail("context tuple: " + err.Error())
		}
	} else {
		for _, key := range shape.context {
			event.context = append(event.context, recordField{key, next()})
		}
	}
	for _, key := range shape.attrs {
		event.attrs = append(event.attrs, recordField{key, next()})
	}
	return event
}
