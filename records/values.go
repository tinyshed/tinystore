package records

import (
	"encoding/hex"
	"math"
	"slices"
	"strconv"
	"strings"
)

// a value column keeps each value's spelling byte for byte, and types what it
// can; inside a block, a time a value spells is kept as its distance behind
// its record's time:
//
//	"9cbaf3d1-0c27-47bc-8fed-cb6e0763b9b2"   → quoted uuid, 16 bytes
//	"154"                                    → quoted integer 154
//	1920 1366 390 1920 1366 390 1920 null    → integers, null an exception
//	"2026-09-23 00:47:32,100 GET /notes"     → stamped text, see stamp.go
const (
	valueText byte = iota
	valueInteger
	valueHex
	valueUUID
	valueStamped
)

// integers that count time, as a JSON line's own "time" does, can be written
// as their distance behind their record's time in the unit they count:
//
//	record 1727300000123456789 ns, "time" 1727300000121   → 2 ms behind
const (
	valueQuoted     = 1 << 4
	valueExceptions = 1 << 5
	valueTimed      = 1 << 6
)

func (e *encoder) appendValues(out []byte, values []string, times []int64) []byte {
	flags := e.innerValues(values)
	if typed, ok := e.appendTyped(out, flags, times); ok {
		return typed
	}
	if stamped, ok := e.appendStamped(out, flags, times); ok {
		return stamped
	}
	// the text kind is zero, so the flags alone name it
	return e.appendTexts(append(out, flags), e.inner)
}

// innerValues strips the quotes when every value has them, so the kinds below
// see what is inside
func (e *encoder) innerValues(values []string) byte {
	e.inner = e.inner[:0]
	quoted := len(values) > 0
	for _, value := range values {
		inner, ok := unquote(value)
		quoted = quoted && ok
		e.inner = append(e.inner, inner)
	}
	if !quoted {
		e.inner = append(e.inner[:0], values...)
		return 0
	}
	return valueQuoted
}

func (e *encoder) appendTyped(out []byte, flags byte, times []int64) ([]byte, bool) {
	integers, uuids, hexes := 0, 0, 0
	for _, value := range e.inner {
		if _, ok := parseInt(value); ok {
			integers++
		}
		if isUUID(value) {
			uuids++
		}
		if isHex(value) && len(value) == len(e.inner[0]) {
			hexes++
		}
	}
	switch n := len(e.inner); {
	case n > 0 && integers == n:
		return e.appendIntegers(out, flags, times), true
	case n > 0 && uuids == n:
		return e.appendFixed(append(out, flags|valueUUID), 16), true
	case n > 0 && hexes == n:
		out = appendCount(append(out, flags|valueHex), len(e.inner[0])/2)
		return e.appendFixed(out, len(e.inner[0])/2), true
	case flags == 0 && n >= 8 && integers*8 >= n*7:
		return e.appendExceptions(append(out, valueInteger|valueExceptions)), true
	}
	return out, false
}

// appendIntegers writes integers as they are, or as their distance behind
// their records' times when that costs fewer bytes
func (e *encoder) appendIntegers(out []byte, flags byte, times []int64) []byte {
	e.numbers = e.numbers[:0]
	for _, value := range e.inner {
		number, _ := parseInt(value)
		e.numbers = append(e.numbers, number)
	}
	if unit, distances, ok := e.timedIntegers(times); ok {
		return e.appendInts(append(out, flags|valueInteger|valueTimed, unit), distances)
	}
	return e.appendInts(append(out, flags|valueInteger), e.numbers)
}

// timeUnits are what an integer may count time in, as powers of ten of a
// nanosecond: seconds, milliseconds, microseconds and nanoseconds
var timeUnits = [...]byte{9, 6, 3, 0}

// timedIntegers finds the unit whose distances cost fewest bytes, when they
// cost fewer than the integers themselves
func (e *encoder) timedIntegers(times []int64) (unit byte, distances []int64, ok bool) {
	if times == nil || len(e.numbers) < 8 {
		return 0, nil, false
	}
	cheapest := -1 // what the integers cost as they are, planned once a unit counts time
	for _, candidate := range timeUnits {
		if !countsTime(e.numbers, times, candidate) {
			continue
		}
		behind, fits := distancesBehind(e.numbers, times, candidate)
		if !fits {
			continue
		}
		if cheapest < 0 {
			cheapest = e.planInts(e.numbers).bytes
		}
		if cost := e.planInts(behind).bytes; cost < cheapest {
			cheapest = cost
			unit, distances, ok = candidate, behind, true
		}
	}
	return unit, distances, ok
}

// countsTime screens a unit on the first sixteen integers: a unit they count
// time in leaves distances spread at most a quarter as wide as they are
func countsTime(numbers, times []int64, unit byte) bool {
	sample := min(16, len(numbers))
	low, high := int64(math.MaxInt64), int64(math.MinInt64)
	for i, number := range numbers[:sample] {
		distance, ok := distanceBehind(times[i], number, int(unit))
		if !ok {
			return false
		}
		low, high = min(low, distance), max(high, distance)
	}
	return 4*distance(low, high) < spread(numbers[:sample])
}

func distancesBehind(numbers, times []int64, unit byte) ([]int64, bool) {
	distances := make([]int64, len(numbers))
	for i, number := range numbers {
		distance, ok := distanceBehind(times[i], number, int(unit))
		if !ok {
			return nil, false
		}
		distances[i] = distance
	}
	return distances, true
}

func spread(values []int64) uint64 {
	return distance(slices.Min(values), slices.Max(values))
}

// appendExceptions writes where the values that are not integers stand, those
// values as text, and then every integer
func (e *encoder) appendExceptions(out []byte) []byte {
	var positions []int64
	var texts []string
	numbers := make([]int64, 0, len(e.inner))
	for i, value := range e.inner {
		if number, ok := parseInt(value); ok {
			numbers = append(numbers, number)
		} else {
			positions, texts = append(positions, int64(i)), append(texts, value)
		}
	}
	out = e.appendDirectInts(appendCount(out, len(positions)), positions)
	out = e.appendTexts(out, texts)
	return e.appendInts(out, numbers)
}

func (e *encoder) appendFixed(out []byte, width int) []byte {
	e.flat = e.flat[:0]
	for _, value := range e.inner {
		e.flat = appendHexBytes(e.flat, value)
	}
	fixed := make([]string, len(e.inner))
	for i := range fixed {
		fixed[i] = string(e.flat[i*width : (i+1)*width])
	}
	return e.appendTexts(out, fixed)
}

func unquote(value string) (string, bool) {
	if len(value) >= 2 && value[0] == '"' && value[len(value)-1] == '"' {
		return value[1 : len(value)-1], true
	}
	return value, false
}

// parseInt accepts only the spelling strconv.FormatInt gives back, so that
// formatting the number again restores the text: "007", "+7" and "-0" stay text
func parseInt(text string) (int64, bool) {
	digits := text
	negative := len(text) > 0 && text[0] == '-'
	if negative {
		digits = text[1:]
	}
	if digits == "" || len(digits) > 19 || (digits[0] == '0' && (len(digits) > 1 || negative)) {
		return 0, false
	}
	var value uint64
	for i := range len(digits) {
		digit := digits[i] - '0'
		if digit > 9 {
			return 0, false
		}
		value = value*10 + uint64(digit)
	}
	if negative && value <= 1<<63 {
		return int64(-value), true //nolint:gosec // two's complement negation of at most 2^63 is exact
	}
	if value > math.MaxInt64 {
		return 0, false
	}
	return int64(value), !negative
}

// isHex is lowercase hex of whole bytes, at least four of them
func isHex(text string) bool {
	if len(text) < 8 || len(text)%2 != 0 {
		return false
	}
	for i := range len(text) {
		if !isHexDigit(text[i]) {
			return false
		}
	}
	return true
}

func isUUID(text string) bool {
	if len(text) != 36 {
		return false
	}
	for i := range len(text) {
		if i == 8 || i == 13 || i == 18 || i == 23 {
			if text[i] != '-' {
				return false
			}
		} else if !isHexDigit(text[i]) {
			return false
		}
	}
	return true
}

func isHexDigit(c byte) bool {
	return (c >= '0' && c <= '9') || (c >= 'a' && c <= 'f')
}

func hexDigit(c byte) byte {
	if c <= '9' {
		return c - '0'
	}
	return c - 'a' + 10
}

// appendHexBytes decodes lowercase hex already checked, skipping a uuid's dashes
func appendHexBytes(out []byte, text string) []byte {
	var high byte
	odd := false
	for i := range len(text) {
		if text[i] == '-' {
			continue
		}
		if odd {
			out = append(out, high<<4|hexDigit(text[i]))
		} else {
			high = hexDigit(text[i])
		}
		odd = !odd
	}
	return out
}

// rowTimes gives a value column the times of its records, asked for only when
// it keeps a time behind them; nil for a column outside a block
type rowTimes func() []int64

func (d *decoder) values(c *cursor, count int, times rowTimes) []string {
	flags := c.readByte()
	values := d.typedValues(c, count, flags, times)
	if flags&valueQuoted != 0 {
		return quote(c, values)
	}
	return values
}

// quote puts back the quotes the encoder took off; each value is a copy, so
// the copies are charged first
func quote(c *cursor, values []string) []string {
	size := 0
	for _, value := range values {
		size += len(value) + 2
	}
	if !c.expand(size) {
		return nil
	}
	var column joiner
	column.text.Grow(size)
	for _, value := range values {
		column.text.WriteByte('"')
		column.text.WriteString(value)
		column.text.WriteByte('"')
		column.end()
	}
	return column.values()
}

func (d *decoder) typedValues(c *cursor, count int, flags byte, times rowTimes) []string {
	kind, quoted := flags&0x0f, flags&valueQuoted != 0
	var values []string
	switch {
	case flags&^(0x0f|valueQuoted|valueExceptions|valueTimed) != 0:
		c.fail("value flags")
	case flags&valueExceptions != 0:
		if kind != valueInteger || quoted || flags&valueTimed != 0 {
			c.fail("value exceptions")
			return nil
		}
		values = d.exceptions(c, count)
	case flags&valueTimed != 0:
		if kind != valueInteger {
			c.fail("timed values")
			return nil
		}
		values = formatInts(d.timedInts(c, count, times))
	case kind == valueInteger:
		values = formatInts(d.ints(c, count))
	case kind == valueStamped:
		values = d.stampedValues(c, count, times)
	case kind == valueUUID:
		values = formatHex(c, d.texts(c, count), 16, true)
	case kind == valueHex:
		width := c.count(maxBlockInput)
		values = formatHex(c, d.texts(c, count), width, false)
	case kind == valueText:
		values = d.texts(c, count)
	default:
		c.fail("value kind")
	}
	return values
}

// timedInts reads integers written as their distance behind their records' times
func (d *decoder) timedInts(c *cursor, count int, times rowTimes) []int64 {
	unit := c.readByte()
	distances := d.ints(c, count)
	at := timesOf(c, times, count)
	if c.err == nil && unit > maxStampDigits {
		c.fail("timed integers' unit")
	}
	if c.err != nil {
		return nil
	}
	for i, distance := range distances {
		number, ok := valueBehind(at[i], distance, int(unit))
		if !ok {
			c.fail("timed integer past 64 bits")
			return nil
		}
		distances[i] = number
	}
	return distances
}

// timesOf asks for the times of a column's records, one a value, and refuses
// a column that has none
func timesOf(c *cursor, times rowTimes, count int) []int64 {
	if c.err != nil {
		return nil
	}
	if times != nil {
		if at := times(); len(at) == count {
			return at
		}
	}
	c.fail("a time kept against record times that are not there")
	return nil
}

func formatInts(numbers []int64) []string {
	var column joiner
	var digits [20]byte
	for _, number := range numbers {
		column.text.Write(strconv.AppendInt(digits[:0], number, 10))
		column.end()
	}
	return column.values()
}

func formatHex(c *cursor, raw []string, width int, uuid bool) []string {
	if !c.expand(len(raw) * (2*width + 4)) {
		return nil
	}
	var column joiner
	column.text.Grow(len(raw) * (2*width + 4))
	digits := make([]byte, 2*width)
	for _, value := range raw {
		if len(value) != width || width == 0 {
			c.fail("fixed value width")
			return nil
		}
		hex.Encode(digits, []byte(value))
		if uuid {
			column.text.Write(digits[:8])
			for _, part := range [][]byte{digits[8:12], digits[12:16], digits[16:20], digits[20:]} {
				column.text.WriteByte('-')
				column.text.Write(part)
			}
		} else {
			column.text.Write(digits)
		}
		column.end()
	}
	return column.values()
}

// joiner builds a column's values as one string and gives each value as a
// part of it: a column costs one allocation, not one a value
type joiner struct {
	text strings.Builder
	ends []int
}

func (j *joiner) end() {
	j.ends = append(j.ends, j.text.Len())
}

func (j *joiner) values() []string {
	text := j.text.String()
	values := make([]string, len(j.ends))
	start := 0
	for i, end := range j.ends {
		values[i], start = text[start:end], end
	}
	return values
}

func (d *decoder) exceptions(c *cursor, count int) []string {
	exceptions := c.count(count)
	positions := d.directInts(c, exceptions)
	texts := d.texts(c, exceptions)
	numbers := d.ints(c, count-exceptions)
	if c.err != nil {
		return nil
	}
	formatted := formatInts(numbers)
	values := make([]string, 0, count)
	next := 0
	for i, position := range positions {
		gap := position - int64(len(values))
		if gap < 0 || gap > int64(len(numbers)-next) {
			c.fail("value exception position")
			return nil
		}
		values = append(values, formatted[next:next+int(gap)]...)
		next += int(gap)
		values = append(values, texts[i])
	}
	return append(values, formatted[next:]...)
}
