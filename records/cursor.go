package records

import (
	"encoding/binary"
	"fmt"

	"github.com/tinyshed/tinystore"
)

// cursor reads one row's bytes and keeps the first invariant they break;
// every read after it returns zero values, so a decoder checks once at the end
type cursor struct {
	data   []byte
	err    error
	budget *expansion
	text   string // data as a string, when strings may be parts of it rather than copies
}

// expansion is what one row may materialize beyond its own bytes: the text it
// decompresses, and the copies it makes to put quotes and hex digits back. It
// is charged before anything is allocated, whatever the counts claim.
type expansion struct {
	used, limit int
}

func (c *cursor) fail(invariant string) {
	if c.err == nil {
		c.err = corrupt(invariant)
	}
}

// corrupt names the invariant stored bytes break, not the offset they broke it at
func corrupt(invariant string) error {
	return fmt.Errorf("%w: records: %s", tinystore.ErrCorrupt, invariant)
}

func (c *cursor) uvarint() uint64 {
	if c.err != nil {
		return 0
	}
	value, n := binary.Uvarint(c.data)
	if n <= 0 {
		c.fail("unsigned integer")
		return 0
	}
	c.data = c.data[n:]
	return value
}

func (c *cursor) varint() int64 {
	if c.err != nil {
		return 0
	}
	value, n := binary.Varint(c.data)
	if n <= 0 {
		c.fail("signed integer")
		return 0
	}
	c.data = c.data[n:]
	return value
}

// count reads a count or length that may not exceed limit
func (c *cursor) count(limit int) int {
	value := c.uvarint()
	if limit < 0 || value > unsigned(limit) {
		c.fail("count or length over its bound")
		return 0
	}
	return int(value) //nolint:gosec // at most limit, an int
}

func (c *cursor) readByte() byte {
	if c.err != nil || len(c.data) == 0 {
		c.fail("truncated bytes")
		return 0
	}
	value := c.data[0]
	c.data = c.data[1:]
	return value
}

func (c *cursor) take(size int) []byte {
	if c.err != nil || size > len(c.data) {
		c.fail("truncated bytes")
		return nil
	}
	value := c.data[:size]
	c.data = c.data[size:]
	return value
}

func (c *cursor) string(limit int) string {
	size := c.count(limit)
	at := len(c.text) - len(c.data)
	value := c.take(size)
	if c.err != nil || c.text == "" {
		return string(value)
	}
	return c.text[at : at+size]
}

func (c *cursor) strings(countLimit, lengthLimit int) []string {
	values := make([]string, c.count(countLimit))
	for i := range values {
		values[i] = c.string(lengthLimit)
	}
	return values
}

func (c *cursor) expand(size int) bool {
	switch {
	case c.err != nil:
		return false
	case c.budget == nil || size < 0 || size > c.budget.limit-c.budget.used:
		c.fail("expanded text over its bound")
		return false
	}
	c.budget.used += size
	return true
}

func (c *cursor) finish() error {
	if c.err == nil && len(c.data) != 0 {
		c.fail("trailing bytes")
	}
	return c.err
}

func appendString(out []byte, value string) []byte {
	out = binary.AppendUvarint(out, uint64(len(value)))
	return append(out, value...)
}

func appendStrings(out []byte, values []string) []byte {
	out = binary.AppendUvarint(out, uint64(len(values)))
	for _, value := range values {
		out = appendString(out, value)
	}
	return out
}

func appendCount(out []byte, count int) []byte {
	return binary.AppendUvarint(out, unsigned(count))
}
