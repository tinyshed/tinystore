package spike

import (
	"encoding/binary"
	"encoding/hex"
	"math"
	"strconv"

	"github.com/klauspost/compress/fse"
	"github.com/klauspost/compress/zstd"
)

// a value column keeps each JSON fragment byte for byte:
//
//	"9cbaf3d1-0c27-47bc-8fed-cb6e0763b9b2"  → quoted uuid, 16 bytes
//	"154"                                   → quoted integer 154
//	1920, 1366, null                        → integers with one exception
const (
	v2Text byte = iota
	v2Integer
	v2Hex
	v2UUID
)

const (
	v2QuotedFlag   = 1 << 4
	v2ExceptedFlag = 1 << 5
	v2Stored       = 0
	v2Compressed   = 1
)

type v2Encoder struct {
	blockEvents int
	blockBytes  int
	writer      *zstd.Encoder
	fse         fse.Scratch
	symbols     []byte
	rice        []byte
	blob        []byte
	flat        []byte
	numbers     []int64
	lengths     []int64
	inner       []string
	counts      map[int64]int
	words       map[string]int
}

type v2Decoder struct {
	reader *zstd.Decoder
	fse    fse.Scratch
}

func newV2Encoder() (*v2Encoder, error) {
	writer, err := zstd.NewWriter(nil, zstd.WithEncoderConcurrency(1), zstd.WithEncoderLevel(zstd.SpeedDefault),
		zstd.WithWindowSize(recordByteLimit), zstd.WithLowerEncoderMem(true), zstd.WithEncoderCRC(false))
	if err != nil {
		return nil, err
	}
	return &v2Encoder{
		blockEvents: recordEventLimit, blockBytes: recordByteLimit,
		writer: writer, counts: map[int64]int{}, words: map[string]int{},
	}, nil
}

func newV2Decoder() (*v2Decoder, error) {
	reader, err := zstd.NewReader(nil, zstd.WithDecoderConcurrency(1),
		zstd.WithDecoderMaxMemory(recordWorkLimit), zstd.WithDecoderMaxWindow(recordByteLimit))
	if err != nil {
		return nil, err
	}
	return &v2Decoder{reader: reader}, nil
}

func v2Unquote(value string) (string, bool) {
	if len(value) >= 2 && value[0] == '"' && value[len(value)-1] == '"' {
		return value[1 : len(value)-1], true
	}
	return value, false
}

// v2ParseInt accepts only the spelling strconv.FormatInt gives back
func v2ParseInt(text string) (int64, bool) {
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
		return int64(-value), true
	}
	if value > math.MaxInt64 {
		return 0, false
	}
	return int64(value), !negative
}

func v2IsHex(text string) bool {
	if len(text) < 8 || len(text)%2 != 0 {
		return false
	}
	for i := range len(text) {
		if c := text[i]; (c < '0' || c > '9') && (c < 'a' || c > 'f') {
			return false
		}
	}
	return true
}

func v2IsUUID(text string) bool {
	if len(text) != 36 {
		return false
	}
	for i := range len(text) {
		c := text[i]
		if i == 8 || i == 13 || i == 18 || i == 23 {
			if c != '-' {
				return false
			}
		} else if (c < '0' || c > '9') && (c < 'a' || c > 'f') {
			return false
		}
	}
	return true
}

func v2HexDigit(c byte) byte {
	if c <= '9' {
		return c - '0'
	}
	return c - 'a' + 10
}

// v2AppendHex decodes lowercase hex that was already checked, skipping uuid dashes
func v2AppendHex(out []byte, text string) []byte {
	var high byte
	odd := false
	for i := range len(text) {
		if text[i] == '-' {
			continue
		}
		if odd {
			out = append(out, high<<4|v2HexDigit(text[i]))
		} else {
			high = v2HexDigit(text[i])
		}
		odd = !odd
	}
	return out
}

func (e *v2Encoder) appendValues(out []byte, values []string) []byte {
	e.inner = e.inner[:0]
	quoted := len(values) > 0
	for _, value := range values {
		inner, ok := v2Unquote(value)
		quoted = quoted && ok
		e.inner = append(e.inner, inner)
	}
	if !quoted {
		e.inner = append(e.inner[:0], values...)
	}
	flags := byte(0)
	if quoted {
		flags = v2QuotedFlag
	}
	if typed, ok := e.appendTyped(out, flags); ok {
		return typed
	}
	// the text kind is zero, so the flags alone name it
	return e.appendText(append(out, flags), e.inner)
}

func (e *v2Encoder) appendTyped(out []byte, flags byte) ([]byte, bool) {
	integers, uuids, hexes := 0, 0, 0
	for _, value := range e.inner {
		if _, ok := v2ParseInt(value); ok {
			integers++
		}
		if v2IsUUID(value) {
			uuids++
		}
		if v2IsHex(value) && len(value) == len(e.inner[0]) {
			hexes++
		}
	}
	switch n := len(e.inner); {
	case n > 0 && integers == n:
		return e.appendIntegers(append(out, flags|v2Integer)), true
	case n > 0 && uuids == n:
		return e.appendFixed(append(out, flags|v2UUID), 16), true
	case n > 0 && hexes == n:
		out = binary.AppendUvarint(append(out, flags|v2Hex), uint64(len(e.inner[0])/2))
		return e.appendFixed(out, len(e.inner[0])/2), true
	case flags == 0 && n >= 8 && integers*8 >= n*7:
		return e.appendExceptions(append(out, v2Integer|v2ExceptedFlag)), true
	}
	return out, false
}

func (e *v2Encoder) appendIntegers(out []byte) []byte {
	e.numbers = e.numbers[:0]
	for _, value := range e.inner {
		number, _ := v2ParseInt(value)
		e.numbers = append(e.numbers, number)
	}
	return e.appendInts(out, e.numbers)
}

func (e *v2Encoder) appendExceptions(out []byte) []byte {
	var positions []int64
	var texts []string
	numbers := make([]int64, 0, len(e.inner))
	for i, value := range e.inner {
		if number, ok := v2ParseInt(value); ok {
			numbers = append(numbers, number)
		} else {
			positions, texts = append(positions, int64(i)), append(texts, value)
		}
	}
	out = e.appendNestedInts(binary.AppendUvarint(out, uint64(len(positions))), positions)
	out = e.appendText(out, texts)
	return e.appendInts(out, numbers)
}

func (e *v2Encoder) appendFixed(out []byte, width int) []byte {
	e.flat = e.flat[:0]
	for _, value := range e.inner {
		e.flat = v2AppendHex(e.flat, value)
	}
	fixed := make([]string, len(e.inner))
	for i := range fixed {
		fixed[i] = string(e.flat[i*width : (i+1)*width])
	}
	return e.appendText(out, fixed)
}

// appendText keeps a dictionary when half the values repeat, else lengths and one blob
func (e *v2Encoder) appendText(out []byte, values []string) []byte {
	clear(e.words)
	var order []string
	for _, value := range values {
		if _, ok := e.words[value]; !ok {
			if len(order)*2 >= len(values) && len(order) > 16 {
				return e.appendRaw(append(out, 0), values)
			}
			e.words[value] = len(order)
			order = append(order, value)
		}
	}
	ids := make([]int64, len(values))
	for i, value := range values {
		ids[i] = int64(e.words[value])
	}
	out = binary.AppendUvarint(append(out, 1), uint64(len(order)))
	out = e.appendRaw(out, order)
	return e.appendNestedInts(out, ids)
}

func (e *v2Encoder) appendRaw(out []byte, values []string) []byte {
	e.lengths, e.blob = e.lengths[:0], e.blob[:0]
	for _, value := range values {
		e.lengths = append(e.lengths, int64(len(value)))
		e.blob = append(e.blob, value...)
	}
	out = e.appendNestedInts(out, e.lengths)
	return e.appendBlob(out, e.blob)
}

// random bytes are not handed to zstd: its frame would only add bytes
func (e *v2Encoder) appendBlob(out, blob []byte) []byte {
	if len(blob) >= 64 && v2ByteEntropy(blob) < 7.5 {
		compressed := e.writer.EncodeAll(blob, nil)
		if len(compressed)+4 < len(blob) {
			out = binary.AppendUvarint(append(out, v2Compressed), uint64(len(compressed)))
			return append(out, compressed...)
		}
	}
	return append(append(out, v2Stored), blob...)
}

func v2ByteEntropy(blob []byte) float64 {
	var counts [256]int
	for _, b := range blob {
		counts[b]++
	}
	entropy := 0.0
	for _, count := range counts {
		if count > 0 {
			p := float64(count) / float64(len(blob))
			entropy -= p * math.Log2(p)
		}
	}
	return entropy
}

func (d *v2Decoder) values(cursor *recordCursor, count int) []string {
	flags := byte(cursor.number(0x3f))
	kind, quoted := flags&0x0f, flags&v2QuotedFlag != 0
	var values []string
	switch {
	case flags&v2ExceptedFlag != 0:
		if kind != v2Integer || quoted {
			cursor.fail("value exceptions")
			return nil
		}
		values = d.exceptions(cursor, count)
	case kind == v2Integer:
		values = v2FormatInts(d.ints(cursor, count, false))
	case kind == v2UUID:
		values = v2FormatHex(cursor, d.text(cursor, count), 16, true)
	case kind == v2Hex:
		width := cursor.number(recordByteLimit)
		values = v2FormatHex(cursor, d.text(cursor, count), width, false)
	case kind == v2Text:
		values = d.text(cursor, count)
	default:
		cursor.fail("value kind")
	}
	if quoted {
		for i, value := range values {
			values[i] = `"` + value + `"`
		}
	}
	return values
}

func v2FormatInts(numbers []int64) []string {
	values := make([]string, len(numbers))
	for i, number := range numbers {
		values[i] = strconv.FormatInt(number, 10)
	}
	return values
}

func v2FormatHex(cursor *recordCursor, raw []string, width int, uuid bool) []string {
	values := make([]string, len(raw))
	for i, value := range raw {
		if len(value) != width || width == 0 {
			cursor.fail("fixed value width")
			return nil
		}
		text := hex.EncodeToString([]byte(value))
		if uuid {
			text = text[:8] + "-" + text[8:12] + "-" + text[12:16] + "-" + text[16:20] + "-" + text[20:]
		}
		values[i] = text
	}
	return values
}

func (d *v2Decoder) exceptions(cursor *recordCursor, count int) []string {
	exceptions := cursor.number(count)
	positions := d.ints(cursor, exceptions, true)
	texts := d.text(cursor, exceptions)
	numbers := d.ints(cursor, count-exceptions, false)
	if cursor.err != nil {
		return nil
	}
	values := make([]string, 0, count)
	for i, position := range positions {
		if position < int64(len(values)) || position >= int64(count) || (i > 0 && positions[i-1] >= position) {
			cursor.fail("value exception position")
			return nil
		}
		for int64(len(values)) < position {
			values = append(values, strconv.FormatInt(numbers[len(values)-i], 10))
		}
		values = append(values, texts[i])
	}
	for len(values) < count {
		values = append(values, strconv.FormatInt(numbers[len(values)-len(positions)], 10))
	}
	return values
}

func (d *v2Decoder) text(cursor *recordCursor, count int) []string {
	switch cursor.number(1) {
	case 0:
		return d.raw(cursor, count)
	default:
		words := d.raw(cursor, cursor.number(count))
		ids := d.ints(cursor, count, true)
		if cursor.err != nil {
			return nil
		}
		values := make([]string, count)
		for i, id := range ids {
			if id < 0 || id >= int64(len(words)) {
				cursor.fail("text dictionary reference")
				return nil
			}
			values[i] = words[id]
		}
		return values
	}
}

func (d *v2Decoder) raw(cursor *recordCursor, count int) []string {
	lengths := d.ints(cursor, count, true)
	total := 0
	for _, length := range lengths {
		if length < 0 || length > recordWorkLimit || total+int(length) > recordWorkLimit {
			cursor.fail("text length")
			return nil
		}
		total += int(length)
	}
	blob := d.blob(cursor, total)
	if cursor.err != nil {
		return nil
	}
	values := make([]string, count)
	for i, length := range lengths {
		values[i], blob = string(blob[:length]), blob[length:]
	}
	return values
}

func (d *v2Decoder) blob(cursor *recordCursor, size int) []byte {
	if cursor.number(1) == v2Stored {
		return cursor.take(size)
	}
	compressed := cursor.take(cursor.number(recordWorkLimit))
	if cursor.err != nil || !cursor.reserveText(size) {
		return nil
	}
	blob, err := d.reader.DecodeAll(compressed, make([]byte, 0, size))
	if err != nil || len(blob) != size {
		cursor.fail("compressed text")
		return nil
	}
	return blob
}
