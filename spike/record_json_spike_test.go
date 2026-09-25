package spike

import (
	"encoding/binary"
	"encoding/json"
	"strings"
)

const recordJSONCellLimit = 16 << 10

type recordJSONShape struct {
	parts   []string
	columns []int
}

func recordJSONParts(value string) ([]string, []string, bool) {
	if !json.Valid([]byte(value)) {
		return nil, nil, false
	}
	var parts, leaves []string
	last := 0
	for at := 0; at < len(value); {
		end, scalar := recordJSONToken(value, at)
		if scalar {
			after := end
			for after < len(value) && strings.ContainsRune(" \r\n\t", rune(value[after])) {
				after++
			}
			if value[at] != '"' || after == len(value) || value[after] != ':' {
				if len(leaves) == recordFieldLimit {
					return nil, nil, false
				}
				parts = append(parts, value[last:at])
				leaves = append(leaves, value[at:end])
				last = end
			}
		}
		at = end
	}
	return append(parts, value[last:]), leaves, true
}

func recordJSONToken(value string, at int) (int, bool) {
	if value[at] == '"' {
		for end := at + 1; end < len(value); end++ {
			switch value[end] {
			case '\\':
				end++
			case '"':
				return end + 1, true
			}
		}
	}
	if strings.ContainsRune("-0123456789tfn", rune(value[at])) {
		end := at + 1
		for end < len(value) && !strings.ContainsRune(" \r\n\t,]}", rune(value[end])) {
			end++
		}
		return end, true
	}
	return at + 1, false
}

func (c *recordBlockCodec) jsonRecordColumn(values []string) []byte {
	var shapes []recordJSONShape
	var columns [][]string
	ids := make([]uint64, len(values))
	known := map[string]int{}
	cells, complex := 0, false
	for row, value := range values {
		parts, leaves, ok := recordJSONParts(value)
		if !ok {
			return nil
		}
		complex = complex || strings.ContainsAny(parts[0], "{[")
		cells += len(leaves)
		if cells > recordJSONCellLimit {
			return nil
		}
		key := string(appendRecordStrings(nil, parts))
		id, found := known[key]
		if !found {
			if len(shapes) == recordShapeLimit || len(columns)+len(leaves) > recordColumnLimit {
				return nil
			}
			id = len(shapes)
			known[key] = id
			shape := recordJSONShape{parts: parts}
			for range leaves {
				shape.columns = append(shape.columns, len(columns))
				columns = append(columns, nil)
			}
			shapes = append(shapes, shape)
		}
		ids[row] = uint64(id)
		for i, value := range leaves {
			column := shapes[id].columns[i]
			columns[column] = append(columns[column], value)
		}
	}
	if !complex {
		return nil
	}
	out := binary.AppendUvarint([]byte{3}, uint64(len(shapes)))
	for _, shape := range shapes {
		out = binary.AppendUvarint(out, uint64(len(shape.columns)))
		out = appendRecordStrings(out, shape.parts)
	}
	out = append(out, c.encodeNumbers(ids)...)
	child := *c
	child.features &^= recordJSONColumns
	for _, column := range columns {
		out = append(out, child.encodeColumn(column)...)
	}
	return out
}

func (c *recordBlockCodec) readRecordJSON(cursor *recordCursor, count int) []string {
	shapes := make([]recordJSONShape, cursor.number(recordShapeLimit))
	columnCount := 0
	for i := range shapes {
		fields := cursor.number(recordFieldLimit)
		shapes[i].parts = readRecordStrings(cursor, fields+1)
		for range fields {
			shapes[i].columns = append(shapes[i].columns, columnCount)
			columnCount++
		}
	}
	if columnCount > recordColumnLimit || cursor.err != nil {
		cursor.fail("JSON column count")
		return nil
	}
	ids := readRecordNumbers(cursor, count)
	counts := make([]int, columnCount)
	cells := 0
	for _, id := range ids {
		if id >= uint64(len(shapes)) {
			cursor.fail("JSON shape reference")
			return nil
		}
		for _, column := range shapes[id].columns {
			counts[column]++
			cells++
		}
	}
	if cells > recordJSONCellLimit {
		cursor.fail("JSON cell count")
		return nil
	}
	columns := make([][]string, columnCount)
	for i, size := range counts {
		columns[i] = c.decodeColumn(cursor, size)
	}
	if cursor.err != nil {
		return nil
	}
	return rebuildRecordJSON(cursor, shapes, ids, columns)
}

func rebuildRecordJSON(cursor *recordCursor, shapes []recordJSONShape, ids []uint64, columns [][]string) []string {
	positions := make([]int, len(columns))
	values := make([]string, len(ids))
	bytes := 0
	for row, id := range ids {
		shape := shapes[id]
		var value strings.Builder
		for i, part := range shape.parts {
			bytes += len(part)
			if i < len(shape.columns) {
				column := shape.columns[i]
				bytes += len(columns[column][positions[column]])
			}
			if bytes > recordWorkLimit {
				cursor.fail("JSON reconstruction byte limit")
				return nil
			}
			value.WriteString(part)
			if i < len(shape.columns) {
				column := shape.columns[i]
				value.WriteString(columns[column][positions[column]])
				positions[column]++
			}
		}
		values[row] = value.String()
		if !json.Valid([]byte(values[row])) {
			cursor.fail("reconstructed JSON syntax")
			return nil
		}
	}
	return values
}

func recordFixedColumn(values []string, transpose bool) []byte {
	if len(values) == 0 || len(values[0]) == 0 || len(values[0]) > 256 {
		return nil
	}
	width := len(values[0])
	mode := byte(4)
	if transpose {
		mode = 5
	}
	out := binary.AppendUvarint([]byte{mode}, uint64(width))
	payload := make([]byte, width*len(values))
	for row, value := range values {
		if len(value) != width {
			return nil
		}
		for column := range width {
			at := row*width + column
			if transpose {
				at = column*len(values) + row
			}
			payload[at] = value[column]
		}
	}
	return append(out, payload...)
}

func readRecordFixed(cursor *recordCursor, count int, transpose bool) []string {
	width := cursor.number(256)
	if width == 0 || count*width > recordWorkLimit {
		cursor.fail("fixed column width or size")
		return nil
	}
	payload := cursor.take(count * width)
	if cursor.err != nil {
		return nil
	}
	values := make([]string, count)
	for row := range values {
		value := make([]byte, width)
		for column := range width {
			at := row*width + column
			if transpose {
				at = column*count + row
			}
			value[column] = payload[at]
		}
		values[row] = string(value)
	}
	return values
}
