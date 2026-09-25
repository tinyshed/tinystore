package spike

import (
	"encoding/binary"
	"slices"
	"strings"
	"testing"
)

type recordExceptions struct {
	positions []uint64
	values    []string
}

func (c *recordBlockCodec) appendRecordExceptions(out []byte, exceptions recordExceptions) []byte {
	out = binary.AppendUvarint(out, uint64(len(exceptions.positions)))
	out = append(out, c.encodeNumbers(exceptions.positions)...)
	return append(out, c.encodeColumn(exceptions.values)...)
}

func (c *recordBlockCodec) recordDependency(reference, values []string) []byte {
	seen := map[string]string{}
	var dictionary []string
	var exceptions recordExceptions
	for i, key := range reference {
		predicted, ok := seen[key]
		if !ok {
			if len(dictionary) == 256 {
				return nil
			}
			predicted = values[i]
			seen[key] = predicted
			dictionary = append(dictionary, predicted)
		}
		if predicted != values[i] {
			exceptions.positions = append(exceptions.positions, uint64(i))
			exceptions.values = append(exceptions.values, values[i])
		}
	}
	out := binary.AppendUvarint([]byte{0}, uint64(len(dictionary)))
	out = append(out, c.encodeColumn(dictionary)...)
	return c.appendRecordExceptions(out, exceptions)
}

func recordUnquote(value string) string {
	if len(value) >= 2 && value[0] == '"' && value[len(value)-1] == '"' {
		return value[1 : len(value)-1]
	}
	return value
}

func (c *recordBlockCodec) recordRecipe(reference, values []string) []byte {
	for seed := range min(8, len(values)) {
		value := recordUnquote(reference[seed])
		if value == "" {
			continue
		}
		at := strings.Index(values[seed], value)
		if at < 0 {
			continue
		}
		prefix, suffix := values[seed][:at], values[seed][at+len(value):]
		if len(prefix)+len(suffix) > 4096 {
			continue
		}
		var exceptions recordExceptions
		for i, referenceValue := range reference {
			if prefix+recordUnquote(referenceValue)+suffix != values[i] {
				exceptions.positions = append(exceptions.positions, uint64(i))
				exceptions.values = append(exceptions.values, values[i])
			}
		}
		if len(exceptions.positions)*2 > len(values) {
			continue
		}
		out := appendRecordString([]byte{1}, prefix)
		out = appendRecordString(out, suffix)
		return c.appendRecordExceptions(out, exceptions)
	}
	return nil
}

func (c *recordBlockCodec) predictRecordColumn(reference, values []string) []byte {
	best := c.recordDependency(reference, values)
	if recipe := c.recordRecipe(reference, values); recipe != nil {
		if best == nil || len(c.pack(recipe)) < len(c.pack(best)) {
			best = recipe
		}
	}
	if c.features&recordStateModels != 0 {
		best = c.predictRecordNumbers(reference, values, best)
	}
	return best
}

func (c *recordBlockCodec) readRecordDependency(cursor *recordCursor, reference []string) []string {
	count := cursor.number(256)
	dictionary := c.decodeColumn(cursor, count)
	if cursor.err != nil {
		return nil
	}
	seen := map[string]string{}
	values := make([]string, len(reference))
	for i, key := range reference {
		value, ok := seen[key]
		if !ok {
			if len(seen) == len(dictionary) {
				cursor.fail("dependency dictionary exhausted")
				return nil
			}
			value = dictionary[len(seen)]
			seen[key] = value
		}
		values[i] = value
	}
	if len(seen) != len(dictionary) {
		cursor.fail("unused dependency values")
	}
	return values
}

func readRecordRecipe(cursor *recordCursor, reference []string) []string {
	prefix, suffix := cursor.text(), cursor.text()
	total := 0
	for _, value := range reference {
		value = recordUnquote(value)
		total += len(prefix) + len(value) + len(suffix)
		if total > recordWorkLimit {
			cursor.fail("recipe expansion byte limit")
			return nil
		}
	}
	if !cursor.reserveText(total) {
		return nil
	}
	values := make([]string, len(reference))
	for i, value := range reference {
		values[i] = prefix + recordUnquote(value) + suffix
	}
	return values
}

func (c *recordBlockCodec) readRecordPrediction(cursor *recordCursor, reference []string) []string {
	data := recordCursor{data: c.unpack(cursor), inflated: cursor.inflated, expanded: cursor.expanded}
	var values []string
	switch mode := data.number(4); mode {
	case 0:
		values = c.readRecordDependency(&data, reference)
		for _, value := range values {
			if !data.reserveText(len(value)) {
				break
			}
		}
	case 1:
		values = readRecordRecipe(&data, reference)
	default:
		values = readRecordNumericPrediction(&data, reference, mode)
	}
	count := data.number(len(reference))
	positions := readRecordNumbers(&data, count)
	exceptions := c.decodeColumn(&data, count)
	if data.err == nil && len(values) == len(reference) {
		for i, position := range positions {
			if position >= uint64(len(values)) || (i > 0 && positions[i-1] >= position) {
				data.fail("exception position")
				break
			}
			values[position] = exceptions[i]
		}
	}
	if err := data.finish(); err != nil {
		cursor.fail("prediction: " + err.Error())
	}
	return values
}

func TestRecordPredictionsKeepExceptions(t *testing.T) {
	codec := recordTestCodec(t)
	reference, values := make([]string, 200), make([]string, 200)
	for i := range reference {
		reference[i] = []string{`"Chrome"`, `"Firefox"`, `"Safari"`}[i%3]
		values[i] = []string{"1920", "1366", "390"}[i%3]
	}
	values[37] = "1024"
	dependency := codec.recordDependency(reference, values)
	cursor := recordCursor{data: codec.pack(dependency)}
	got := codec.readRecordPrediction(&cursor, reference)
	if err := cursor.finish(); err != nil || !slices.Equal(got, values) {
		t.Fatalf("dependency lost an exception: %v", err)
	}
	for i := range values {
		values[i] = "browser=" + recordUnquote(reference[i]) + "; active"
	}
	values[71] = "unknown browser"
	recipe := codec.recordRecipe(reference, values)
	if recipe == nil {
		t.Fatal("recipe was not found")
	}
	cursor = recordCursor{data: codec.pack(recipe)}
	got = codec.readRecordPrediction(&cursor, reference)
	if err := cursor.finish(); err != nil || !slices.Equal(got, values) {
		t.Fatalf("recipe lost an exception: %v", err)
	}
}
