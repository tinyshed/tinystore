package records

import (
	"math"
	"strings"
)

// a text column is a dictionary when at most half its values are distinct,
// and raw text otherwise
const (
	textRaw byte = iota
	textDictionary
)

// raw text keeps a length column only when it must:
//
//	"GET /a", "POST /b"   → lines "GET /a\nPOST /b\n"
//	16-byte trace ids     → lengths 16 16 …, one width of no bits, then the bytes
const (
	rawLengths byte = iota
	rawLines
)

// a blob goes through zstd once, and not at all when its bytes look random
const (
	blobStored byte = iota
	blobCompressed
)

func (e *encoder) appendTexts(out []byte, values []string) []byte {
	clear(e.words)
	var order []string
	for _, value := range values {
		if _, ok := e.words[value]; ok {
			continue
		}
		if len(order)*2 >= len(values) && len(order) > 16 {
			return e.appendRaw(append(out, textRaw), values)
		}
		e.words[value] = len(order)
		order = append(order, value)
	}
	ids := make([]int64, len(values))
	for i, value := range values {
		ids[i] = int64(e.words[value])
	}
	out = appendCount(append(out, textDictionary), len(order))
	out = e.appendRaw(out, order)
	return e.appendDirectInts(out, ids)
}

func (e *encoder) appendRaw(out []byte, values []string) []byte {
	e.lengths, e.blob = e.lengths[:0], e.blob[:0]
	sameLength, lines := true, true
	for _, value := range values {
		sameLength = sameLength && len(value) == len(values[0])
		lines = lines && strings.IndexByte(value, '\n') < 0
	}
	if lines && !sameLength {
		for _, value := range values {
			e.blob = append(append(e.blob, value...), '\n')
		}
		out = appendCount(append(out, rawLines), len(e.blob))
		return e.appendBlob(out, e.blob)
	}
	for _, value := range values {
		e.lengths = append(e.lengths, int64(len(value)))
		e.blob = append(e.blob, value...)
	}
	out = e.appendDirectInts(append(out, rawLengths), e.lengths)
	return e.appendBlob(out, e.blob)
}

// appendBlob hands zstd only text that is not random: its frame would add bytes
func (e *encoder) appendBlob(out, blob []byte) []byte {
	if len(blob) >= 64 && byteEntropy(blob) < 7.5 {
		compressed := e.zstd.EncodeAll(blob, nil)
		if len(compressed)+4 < len(blob) {
			out = appendCount(append(out, blobCompressed), len(compressed))
			return append(out, compressed...)
		}
	}
	return append(append(out, blobStored), blob...)
}

// byteEntropy is bits a byte: 8 for random bytes, about 4.5 for English text
func byteEntropy(blob []byte) float64 {
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

func (d *decoder) texts(c *cursor, count int) []string {
	switch c.readByte() {
	case textRaw:
		return d.raw(c, count)
	case textDictionary:
		words := d.raw(c, c.count(count))
		ids := d.directInts(c, count)
		if c.err != nil {
			return nil
		}
		values := make([]string, count)
		for i, id := range ids {
			if id < 0 || id >= int64(len(words)) {
				c.fail("text dictionary reference")
				return nil
			}
			values[i] = words[id]
		}
		return values
	}
	c.fail("text column layout")
	return nil
}

func (d *decoder) raw(c *cursor, count int) []string {
	switch c.readByte() {
	case rawLines:
		return d.lines(c, count)
	case rawLengths:
		return d.lengths(c, count)
	}
	c.fail("raw text layout")
	return nil
}

func (d *decoder) lengths(c *cursor, count int) []string {
	lengths := d.directInts(c, count)
	total := 0
	for _, length := range lengths {
		if length < 0 || length > int64(maxExpandedText-total) {
			c.fail("text length")
			return nil
		}
		total += int(length)
	}
	blob := d.blob(c, total)
	if c.err != nil {
		return nil
	}
	text, values := string(blob), make([]string, count)
	for i, length := range lengths {
		values[i], text = text[:length], text[length:]
	}
	return values
}

func (d *decoder) lines(c *cursor, count int) []string {
	blob := d.blob(c, c.count(maxExpandedText))
	if c.err != nil {
		return nil
	}
	text, values := string(blob), make([]string, 0, count)
	for len(values) < count {
		at := strings.IndexByte(text, '\n')
		if at < 0 {
			break
		}
		values, text = append(values, text[:at]), text[at+1:]
	}
	if len(values) != count || len(text) != 0 {
		c.fail("text lines")
		return nil
	}
	return values
}

// blob reads size bytes, stored or compressed; decompressed bytes count
// against the block's bound before they are allocated
func (d *decoder) blob(c *cursor, size int) []byte {
	switch c.readByte() {
	case blobStored:
		return c.take(size)
	case blobCompressed:
		compressed := c.take(c.count(maxExpandedText))
		if c.err != nil || !c.expand(size) {
			return nil
		}
		blob, err := d.zstd.DecodeAll(compressed, make([]byte, 0, size))
		if err != nil || len(blob) != size {
			c.fail("compressed text")
			return nil
		}
		return blob
	}
	c.fail("text blob layout")
	return nil
}
