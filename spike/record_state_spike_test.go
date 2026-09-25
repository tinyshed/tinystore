package spike

import (
	"encoding/binary"
)

type recordNumericColumn struct {
	format     recordScalarFormat
	values     []uint64
	exceptions recordExceptions
}

func numericRecordColumn(values []string) (recordNumericColumn, bool) {
	column := recordNumericColumn{values: make([]uint64, len(values))}
	for _, value := range values {
		atom := parseRecordScalar(value)
		if atom.format.kind >= recordSigned && atom.format.kind <= recordDecimal {
			column.format = atom.format
			break
		}
	}
	if column.format.kind == recordLiteral {
		return column, false
	}
	for i, value := range values {
		atom := parseRecordScalar(value)
		if atom.format == column.format {
			column.values[i] = atom.number
		} else {
			column.exceptions.positions = append(column.exceptions.positions, uint64(i))
			column.exceptions.values = append(column.exceptions.values, value)
		}
	}
	return column, true
}

func appendRecordNumericFormat(out []byte, format recordScalarFormat) []byte {
	flags := byte(format.kind)
	if format.quoted {
		flags |= 8
	}
	return append(out, flags, byte(format.width))
}

func (c *recordBlockCodec) recordKeyedNumbers(reference []string, column recordNumericColumn, mode byte) []byte {
	state := map[string]uint64{}
	residuals := make([]uint64, len(reference))
	for i, key := range reference {
		previous, known := state[key]
		if !known && len(state) == recordContextLimit {
			return nil
		}
		residuals[i] = recordZigzag(column.values[i] - previous)
		if mode == 2 || !known {
			state[key] = column.values[i]
		}
	}
	out := appendRecordNumericFormat([]byte{mode}, column.format)
	out = append(out, c.encodeNumbers(residuals)...)
	return c.appendRecordExceptions(out, column.exceptions)
}

func (c *recordBlockCodec) recordAffineNumbers(reference []string, column recordNumericColumn) []byte {
	inputs := make([]uint64, len(reference))
	for i, value := range reference {
		atom := parseRecordScalar(value)
		if atom.format.kind < recordSigned || atom.format.kind > recordDecimal {
			return nil
		}
		inputs[i] = atom.number
	}
	coefficients := []uint64{1, ^uint64(0)}
	for i := 1; i < len(inputs); i++ {
		if difference := int64(inputs[i] - inputs[0]); difference != 0 {
			coefficients = append(coefficients, uint64(int64(column.values[i]-column.values[0])/difference))
			break
		}
	}
	var best []byte
	for _, multiplier := range coefficients {
		offset := column.values[0] - multiplier*inputs[0]
		residuals := make([]uint64, len(inputs))
		for i, input := range inputs {
			residuals[i] = recordZigzag(column.values[i] - (multiplier*input + offset))
		}
		out := appendRecordNumericFormat([]byte{4}, column.format)
		out = binary.AppendUvarint(out, multiplier)
		out = binary.AppendUvarint(out, offset)
		out = append(out, c.encodeNumbers(residuals)...)
		out = c.appendRecordExceptions(out, column.exceptions)
		if best == nil || len(c.pack(out)) < len(c.pack(best)) {
			best = out
		}
	}
	return best
}

func (c *recordBlockCodec) predictRecordNumbers(reference, values []string, best []byte) []byte {
	column, ok := numericRecordColumn(values)
	if !ok || len(values) < 2 {
		return best
	}
	candidates := [][]byte{
		c.recordKeyedNumbers(reference, column, 2), c.recordKeyedNumbers(reference, column, 3),
		c.recordAffineNumbers(reference, column),
	}
	for _, candidate := range candidates {
		if candidate != nil && (best == nil || len(c.pack(candidate)) < len(c.pack(best))) {
			best = candidate
		}
	}
	return best
}

func readRecordNumericPrediction(cursor *recordCursor, reference []string, mode int) []string {
	format := readRecordFormat(cursor)
	if format.kind < recordSigned || format.kind > recordDecimal {
		cursor.fail("numeric prediction format")
		return nil
	}
	var multiplier, offset uint64
	if mode == 4 {
		multiplier, offset = cursor.unsigned(), cursor.unsigned()
	}
	residuals := readRecordNumbers(cursor, len(reference))
	if cursor.err != nil {
		return nil
	}
	state := map[string]uint64{}
	values := make([]string, len(reference))
	for i, key := range reference {
		previous, known := state[key]
		if mode == 4 {
			atom := parseRecordScalar(key)
			if atom.format.kind < recordSigned || atom.format.kind > recordDecimal {
				cursor.fail("numeric prediction source")
				return nil
			}
			previous = multiplier*atom.number + offset
		} else if !known && len(state) == recordContextLimit {
			cursor.fail("numeric state cardinality")
			return nil
		}
		number := previous + recordUnzigzag(residuals[i])
		if mode == 2 || (mode == 3 && !known) {
			state[key] = number
		}
		value, ok := (recordScalar{format: format, number: number}).render()
		if !ok || !cursor.reserveText(len(value)) {
			cursor.fail("numeric prediction reconstruction")
			return nil
		}
		values[i] = value
	}
	return values
}
