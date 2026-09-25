package spike

import (
	"encoding/binary"
	"slices"
	"strconv"
	"strings"
)

// a value column may be written as a function of an earlier column over the same rows:
//
//	user_id  92831               route  "/users/92831"   → recipe "/users/ + user_id + "
//	x        100007              y      300028           → affine y = 3x + 7
//
// eight or sixteen rows decide whether the whole column is tried
const (
	v2PredictedFlag = 1 << 6
	v2Recipe        = 0
	v2Affine        = 1
	v2PredictWindow = 8
)

// v2Carriers names, per slot, the shapes of this block that carry it: equal names mean equal rows
func v2Carriers(schema *v2Schema, shapes []int) []string {
	present := slices.Compact(slices.Sorted(slices.Values(shapes)))
	carriers := make([]string, len(schema.slots))
	for _, shape := range present {
		for _, index := range schema.shapeSlots[shape] {
			carriers[index] += strconv.Itoa(shape) + ","
		}
	}
	return carriers
}

func v2ValueColumn(slot v2Slot, columns *v2Columns) []string {
	switch slot.kind {
	case v2SlotBody:
		return columns.bodies
	case v2SlotAttr:
		return columns.attrs[slot.column]
	}
	return nil
}

func (e *v2Encoder) predictSlot(payload []byte, before, index int, schema *v2Schema, columns *v2Columns,
	carriers []string,
) []byte {
	target := v2ValueColumn(schema.slots[index], columns)
	if len(target) < 2 {
		return payload
	}
	for reference := max(0, index-v2PredictWindow); reference < index; reference++ {
		source := v2ValueColumn(schema.slots[reference], columns)
		if len(source) != len(target) || carriers[reference] != carriers[index] {
			continue
		}
		for _, candidate := range [][]byte{e.recipe(reference, source, target), e.affine(reference, source, target)} {
			if candidate != nil && len(candidate) < len(payload)-before {
				payload = append(payload[:before], candidate...)
			}
		}
	}
	return payload
}

func v2RecipeParts(source, target string) (string, string, bool) {
	text, _ := v2Unquote(source)
	at := strings.Index(target, text)
	if text == "" || at < 0 {
		return "", "", false
	}
	return target[:at], target[at+len(text):], true
}

func v2RecipeHolds(prefix, suffix, source, target string) bool {
	text, _ := v2Unquote(source)
	return len(target) == len(prefix)+len(text)+len(suffix) && strings.HasPrefix(target, prefix) &&
		strings.HasSuffix(target, suffix) && target[len(prefix):len(target)-len(suffix)] == text
}

func (e *v2Encoder) recipe(reference int, source, target []string) []byte {
	prefix, suffix, ok := v2RecipeParts(source[0], target[0])
	if !ok || len(prefix)+len(suffix) > recordByteLimit {
		return nil
	}
	sample := min(len(target), 2*v2PredictWindow)
	misses := 0
	for i := range sample {
		if !v2RecipeHolds(prefix, suffix, source[i], target[i]) {
			misses++
		}
	}
	if misses*8 > sample {
		return nil
	}
	var positions []int64
	var exceptions []string
	for i := range target {
		if !v2RecipeHolds(prefix, suffix, source[i], target[i]) {
			positions, exceptions = append(positions, int64(i)), append(exceptions, target[i])
		}
	}
	if len(positions)*4 > len(target) {
		return nil
	}
	out := binary.AppendUvarint([]byte{v2PredictedFlag}, uint64(reference))
	out = appendRecordString(appendRecordString(append(out, v2Recipe), prefix), suffix)
	out = e.appendNestedInts(binary.AppendUvarint(out, uint64(len(positions))), positions)
	return e.appendText(out, exceptions)
}

func v2ParseInts(values []string) ([]int64, bool) {
	numbers := make([]int64, len(values))
	for i, value := range values {
		number, ok := v2ParseInt(value)
		if !ok {
			return nil, false
		}
		numbers[i] = number
	}
	return numbers, true
}

func (e *v2Encoder) affine(reference int, source, target []string) []byte {
	sample := min(len(target), 2*v2PredictWindow)
	x, ok := v2ParseInts(source[:sample])
	y, fine := v2ParseInts(target[:sample])
	if !ok || !fine {
		return nil
	}
	slope, offset, found := v2AffineFit(x, y)
	if !found {
		return nil
	}
	misses := 0
	for i := range sample {
		if slope*x[i]+offset != y[i] {
			misses++
		}
	}
	if misses*8 > sample {
		return nil
	}
	x, ok = v2ParseInts(source)
	y, fine = v2ParseInts(target)
	if !ok || !fine {
		return nil
	}
	residuals := make([]int64, len(y))
	for i := range y {
		residuals[i] = y[i] - (slope*x[i] + offset)
	}
	out := binary.AppendUvarint([]byte{v2PredictedFlag}, uint64(reference))
	out = binary.AppendVarint(binary.AppendVarint(append(out, v2Affine), slope), offset)
	return e.appendInts(out, residuals)
}

func v2AffineFit(x, y []int64) (int64, int64, bool) {
	for i := 1; i < len(x); i++ {
		if dx := x[i] - x[0]; dx != 0 && (y[i]-y[0])%dx == 0 {
			slope := (y[i] - y[0]) / dx
			return slope, y[0] - slope*x[0], true
		}
	}
	return 0, 0, false
}

// predicted reads a column written as a function of an earlier, unpredicted one
func (d *v2Decoder) predicted(block *v2Opened, index int, cursor *recordCursor, count int) []string {
	if cursor.number(0x7f) != v2PredictedFlag {
		cursor.fail("predicted column flags")
		return nil
	}
	reference := cursor.number(recordColumnLimit)
	if reference >= index || block.counts[reference] != count || !v2IsValueSlot(block.schema.slots[reference]) ||
		(len(block.payloads[reference]) > 0 && block.payloads[reference][0]&v2PredictedFlag != 0) {
		cursor.fail("prediction reference")
		return nil
	}
	source, err := d.valueSlot(block, reference)
	if err != nil {
		cursor.fail("prediction source: " + err.Error())
		return nil
	}
	if cursor.number(v2Affine) == v2Affine {
		return d.affineValues(cursor, source)
	}
	return d.recipeValues(cursor, source)
}

func v2IsValueSlot(slot v2Slot) bool {
	return slot.kind == v2SlotBody || slot.kind == v2SlotAttr
}

func (d *v2Decoder) recipeValues(cursor *recordCursor, source []string) []string {
	prefix, suffix := cursor.text(), cursor.text()
	values := make([]string, len(source))
	for i, value := range source {
		text, _ := v2Unquote(value)
		if !cursor.reserveText(len(prefix) + len(text) + len(suffix)) {
			return nil
		}
		values[i] = prefix + text + suffix
	}
	exceptions := cursor.number(len(source))
	positions := d.ints(cursor, exceptions, true)
	replacements := d.text(cursor, exceptions)
	if cursor.err != nil {
		return nil
	}
	for i, position := range positions {
		if position < 0 || position >= int64(len(values)) || (i > 0 && positions[i-1] >= position) {
			cursor.fail("recipe exception position")
			return nil
		}
		values[position] = replacements[i]
	}
	return values
}

func (d *v2Decoder) affineValues(cursor *recordCursor, source []string) []string {
	slope, offset := cursor.signed(), cursor.signed()
	x, ok := v2ParseInts(source)
	residuals := d.ints(cursor, len(source), false)
	if !ok || cursor.err != nil {
		cursor.fail("affine source")
		return nil
	}
	values := make([]string, len(x))
	for i := range x {
		values[i] = strconv.FormatInt(slope*x[i]+offset+residuals[i], 10)
	}
	return values
}
