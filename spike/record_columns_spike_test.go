package spike

import (
	"encoding/binary"
	"encoding/hex"
	"errors"
	"strconv"
	"strings"
)

const (
	recordLiteral = iota
	recordSigned
	recordUnsigned
	recordDecimal
	recordHex
	recordUUID
)

type recordScalarFormat struct {
	kind   int
	width  int
	quoted bool
	upper  bool
}

type recordScalar struct {
	format recordScalarFormat
	number uint64
	bytes  string
}

func parseRecordID(value string, width int) ([]byte, error) {
	if value == "" {
		return nil, nil
	}
	decoded, err := hex.DecodeString(value)
	if err != nil || len(decoded) != width {
		return nil, errors.New("record trace or span must be fixed-width hex")
	}
	return decoded, nil
}

func parseRecordScalar(value string) recordScalar {
	original := value
	quoted := len(value) >= 2 && value[0] == '"' && value[len(value)-1] == '"'
	if quoted {
		value = value[1 : len(value)-1]
	}
	format := recordScalarFormat{quoted: quoted}
	if number, err := strconv.ParseInt(value, 10, 64); err == nil && strconv.FormatInt(number, 10) == value {
		format.kind = recordSigned
		return recordScalar{format: format, number: uint64(number)}
	}
	if number, err := strconv.ParseUint(value, 10, 64); err == nil && strconv.FormatUint(number, 10) == value {
		format.kind = recordUnsigned
		return recordScalar{format: format, number: number}
	}
	if whole, fraction, ok := strings.Cut(value, "."); ok && len(fraction) > 0 && len(fraction) <= 18 {
		if number, err := strconv.ParseInt(whole+fraction, 10, 64); err == nil {
			format.kind, format.width = recordDecimal, len(fraction)
			atom := recordScalar{format: format, number: uint64(number)}
			if rebuilt, valid := atom.render(); valid && rebuilt == original {
				return atom
			}
		}
	}
	if atom, ok := parseRecordHex(value, quoted); ok {
		return atom
	}
	return recordScalar{bytes: original}
}

func parseRecordHex(value string, quoted bool) (recordScalar, bool) {
	format := recordScalarFormat{kind: recordHex, quoted: quoted}
	digits := value
	if len(value) == 36 && value[8] == '-' && value[13] == '-' && value[18] == '-' && value[23] == '-' {
		format.kind = recordUUID
		digits = strings.ReplaceAll(value, "-", "")
	}
	if len(digits) < 8 || len(digits) > 64 || len(digits)%2 != 0 {
		return recordScalar{}, false
	}
	if digits != strings.ToLower(digits) {
		if digits != strings.ToUpper(digits) {
			return recordScalar{}, false
		}
		format.upper = true
	}
	decoded, err := hex.DecodeString(digits)
	if err != nil {
		return recordScalar{}, false
	}
	format.width = len(decoded)
	return recordScalar{format: format, bytes: string(decoded)}, true
}

func (a recordScalar) render() (string, bool) {
	var value string
	switch a.format.kind {
	case recordLiteral:
		return a.bytes, true
	case recordSigned:
		value = strconv.FormatInt(int64(a.number), 10)
	case recordUnsigned:
		value = strconv.FormatUint(a.number, 10)
	case recordDecimal:
		value = strconv.FormatInt(int64(a.number), 10)
		negative := strings.HasPrefix(value, "-")
		value = strings.TrimPrefix(value, "-")
		value = strings.Repeat("0", max(0, a.format.width+1-len(value))) + value
		cut := len(value) - a.format.width
		value = value[:cut] + "." + value[cut:]
		if negative {
			value = "-" + value
		}
	case recordHex, recordUUID:
		if len(a.bytes) != a.format.width {
			return "", false
		}
		value = hex.EncodeToString([]byte(a.bytes))
		if a.format.upper {
			value = strings.ToUpper(value)
		}
		if a.format.kind == recordUUID {
			value = value[:8] + "-" + value[8:12] + "-" + value[12:16] + "-" + value[16:20] + "-" + value[20:]
		}
	default:
		return "", false
	}
	if a.format.quoted {
		value = "\"" + value + "\""
	}
	return value, true
}

func appendRecordStrings(out []byte, values []string) []byte {
	for _, value := range values {
		out = appendRecordString(out, value)
	}
	return out
}

func (c *recordBlockCodec) dictionaryColumn(values []string) []byte {
	ids := make([]uint64, len(values))
	seen := map[string]uint64{}
	var words []string
	for i, value := range values {
		id, ok := seen[value]
		if !ok {
			id = uint64(len(words))
			seen[value] = id
			words = append(words, value)
		}
		ids[i] = id
	}
	out := binary.AppendUvarint([]byte{1}, uint64(len(words)))
	out = appendRecordStrings(out, words)
	return append(out, c.encodeNumbers(ids)...)
}

func (c *recordBlockCodec) typedColumn(values []string) []byte {
	return c.typedColumnEncoding(values, false)
}

func (c *recordBlockCodec) typedColumnEncoding(values []string, fixed bool) []byte {
	ids := make([]uint64, len(values))
	seen := map[recordScalarFormat]int{}
	var formats []recordScalarFormat
	var groups [][]recordScalar
	for i, value := range values {
		atom := parseRecordScalar(value)
		id, ok := seen[atom.format]
		if !ok {
			if len(formats) == 16 {
				return nil
			}
			id = len(formats)
			seen[atom.format] = id
			formats, groups = append(formats, atom.format), append(groups, nil)
		}
		ids[i] = uint64(id)
		groups[id] = append(groups[id], atom)
	}
	out := binary.AppendUvarint([]byte{2}, uint64(len(formats)))
	if fixed {
		out[0] = 6
	}
	for _, format := range formats {
		flags := byte(format.kind)
		if format.quoted {
			flags |= 8
		}
		if format.upper {
			flags |= 16
		}
		out = append(out, flags, byte(format.width))
	}
	out = append(out, c.encodeNumbers(ids)...)
	for i, group := range groups {
		if formats[i].kind >= recordSigned && formats[i].kind <= recordDecimal {
			numbers := make([]uint64, len(group))
			for j, atom := range group {
				numbers[j] = atom.number
			}
			out = append(out, c.encodeNumbers(numbers)...)
		} else {
			for _, atom := range group {
				if fixed && (formats[i].kind == recordHex || formats[i].kind == recordUUID) {
					out = append(out, atom.bytes...)
				} else {
					out = appendRecordString(out, atom.bytes)
				}
			}
		}
	}
	return out
}

func (c *recordBlockCodec) encodeColumn(values []string) []byte {
	best := c.pack(appendRecordStrings([]byte{0}, values))
	candidates := [][]byte{c.dictionaryColumn(values), c.typedColumn(values)}
	if c.features&recordJSONColumns != 0 {
		candidates = append(candidates, c.jsonRecordColumn(values))
	}
	if c.features&recordNumberModels != 0 {
		candidates = append(candidates, recordFixedColumn(values, false), recordFixedColumn(values, true), c.typedColumnEncoding(values, true))
	}
	for _, candidate := range candidates {
		if candidate == nil {
			continue
		}
		if packed := c.pack(candidate); len(packed) < len(best) {
			best = packed
		}
	}
	return best
}

func readRecordStrings(cursor *recordCursor, count int) []string {
	values := make([]string, count)
	for i := range values {
		values[i] = cursor.text()
	}
	return values
}

func readRecordDictionary(cursor *recordCursor, count int) []string {
	words := readRecordStrings(cursor, cursor.number(min(count, recordCellLimit)))
	ids := readRecordNumbers(cursor, count)
	values := make([]string, count)
	for i, id := range ids {
		if id >= uint64(len(words)) {
			cursor.fail("dictionary reference")
			return nil
		}
		values[i] = words[id]
	}
	return values
}

func readRecordFormat(cursor *recordCursor) recordScalarFormat {
	flags, width := cursor.number(31), cursor.number(32)
	format := recordScalarFormat{kind: flags & 7, width: width, quoted: flags&8 != 0, upper: flags&16 != 0}
	valid := format.kind <= recordUUID
	switch format.kind {
	case recordLiteral:
		valid = valid && flags == 0 && width == 0
	case recordSigned, recordUnsigned:
		valid = valid && !format.upper && width == 0
	case recordDecimal:
		valid = valid && !format.upper && width > 0 && width <= 18
	case recordHex:
		valid = valid && width >= 4
	case recordUUID:
		valid = valid && width == 16
	}
	if !valid {
		cursor.fail("scalar format")
	}
	return format
}

func readRecordTyped(cursor *recordCursor, count int, fixed bool) []string {
	formats := make([]recordScalarFormat, cursor.number(16))
	for i := range formats {
		formats[i] = readRecordFormat(cursor)
	}
	ids := readRecordNumbers(cursor, count)
	positions := make([][]int, len(formats))
	for i, id := range ids {
		if id >= uint64(len(formats)) || cursor.err != nil {
			cursor.fail("scalar format reference")
			return nil
		}
		positions[id] = append(positions[id], i)
	}
	values := make([]string, count)
	for id, format := range formats {
		if len(positions[id]) == 0 {
			cursor.fail("unused scalar format")
			return nil
		}
		var numbers []uint64
		if format.kind >= recordSigned && format.kind <= recordDecimal {
			numbers = readRecordNumbers(cursor, len(positions[id]))
		}
		for j, position := range positions[id] {
			atom := recordScalar{format: format}
			if numbers != nil {
				atom.number = numbers[j]
			} else if fixed && (format.kind == recordHex || format.kind == recordUUID) {
				atom.bytes = string(cursor.take(format.width))
			} else {
				atom.bytes = cursor.text()
			}
			value, ok := atom.render()
			if !ok {
				cursor.fail("scalar reconstruction")
				return nil
			}
			values[position] = value
		}
	}
	return values
}

func (c *recordBlockCodec) decodeColumn(cursor *recordCursor, count int) []string {
	if cursor.depth >= 4 {
		cursor.fail("column nesting depth")
		return nil
	}
	data := recordCursor{data: c.unpack(cursor), depth: cursor.depth + 1, inflated: cursor.inflated, expanded: cursor.expanded}
	var values []string
	switch mode := data.number(6); mode {
	case 0:
		values = readRecordStrings(&data, count)
	case 1:
		values = readRecordDictionary(&data, count)
	case 2:
		values = readRecordTyped(&data, count, false)
	case 6:
		values = readRecordTyped(&data, count, true)
	case 3:
		values = c.readRecordJSON(&data, count)
	case 4, 5:
		values = readRecordFixed(&data, count, mode == 5)
	}
	if err := data.finish(); err != nil {
		cursor.fail("column: " + err.Error())
	}
	for _, value := range values {
		if !cursor.reserveText(len(value)) {
			return nil
		}
	}
	return values
}
