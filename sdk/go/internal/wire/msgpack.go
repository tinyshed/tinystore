// Package wire is the protocol's bytes for the Go SDK: the MessagePack profile
// of docs/wire.md, and every message and method, which crates/protocol writes
// into protocol.go from protocol/*.wire.
//
// A reader keeps the first rule a body broke and answers zero values after
// it, so that a message's reader, written out a field at a time, reads on to
// its end and asks once whether the body was what it said:
//
//	r := wire.NewReader(body)
//	var call wire.KvCall
//	call.Read(r)
//	err := r.End()
package wire

import (
	"encoding/binary"
	"fmt"
	"math"
	"unicode/utf8"
)

// MaxDepth is how deeply maps and arrays may nest in a message.
const MaxDepth = 8

// Type is what a value is, as its first byte says.
type Type int

const (
	TypeInvalid Type = iota
	TypeNil
	TypeBool
	TypeInteger
	TypeFloat
	TypeStr
	TypeBin
	TypeArray
	TypeMap
)

// TypeOf is the type a value's first byte begins: a float 32, an extension
// or the byte never used are Invalid, which the profile refuses.
func TypeOf(b byte) Type {
	switch {
	case b <= 0x7f || b >= 0xe0:
		return TypeInteger
	case b <= 0x8f:
		return TypeMap
	case b <= 0x9f:
		return TypeArray
	case b <= 0xbf:
		return TypeStr
	}
	switch b {
	case 0xc0:
		return TypeNil
	case 0xc2, 0xc3:
		return TypeBool
	case 0xc4, 0xc5, 0xc6:
		return TypeBin
	case 0xcb:
		return TypeFloat
	case 0xcc, 0xcd, 0xce, 0xcf, 0xd0, 0xd1, 0xd2, 0xd3:
		return TypeInteger
	case 0xd9, 0xda, 0xdb:
		return TypeStr
	case 0xdc, 0xdd:
		return TypeArray
	case 0xde, 0xdf:
		return TypeMap
	}
	return TypeInvalid
}

var typeNames = [...]string{"an invalid byte", "nil", "a bool", "an integer", "a float", "a str", "a bin", "an array", "a map"}

func (t Type) String() string { return typeNames[t] }

// Writer writes one message, each integer, length and count in its shortest
// form and every float in eight bytes, so that a vector compares byte for
// byte.
type Writer struct {
	buf []byte
}

// Bytes is what was written.
func (w *Writer) Bytes() []byte { return w.buf }

func (w *Writer) Nil() { w.buf = append(w.buf, 0xc0) }

func (w *Writer) Bool(v bool) {
	if v {
		w.buf = append(w.buf, 0xc3)
	} else {
		w.buf = append(w.buf, 0xc2)
	}
}

// Uint writes a value from 0 in the unsigned family's shortest form:
// 7 → 07, 300 → cd 01 2c.
func (w *Writer) Uint(v uint64) {
	switch {
	case v <= 0x7f:
		w.buf = append(w.buf, byte(v))
	case v <= 0xff:
		w.buf = append(w.buf, 0xcc, byte(v))
	case v <= 0xffff:
		w.buf = binary.BigEndian.AppendUint16(append(w.buf, 0xcd), uint16(v))
	case v <= 0xffff_ffff:
		w.buf = binary.BigEndian.AppendUint32(append(w.buf, 0xce), uint32(v))
	default:
		w.buf = binary.BigEndian.AppendUint64(append(w.buf, 0xcf), v)
	}
}

// Int writes a value from 0 as Uint does, a negative one in the signed
// family's shortest form: -1 → ff, -33 → d0 df.
func (w *Writer) Int(v int64) {
	switch {
	case v >= 0:
		w.Uint(uint64(v))
	case v >= -32:
		w.buf = append(w.buf, byte(v))
	case v >= math.MinInt8:
		w.buf = append(w.buf, 0xd0, byte(v))
	case v >= math.MinInt16:
		w.buf = binary.BigEndian.AppendUint16(append(w.buf, 0xd1), uint16(v))
	case v >= math.MinInt32:
		w.buf = binary.BigEndian.AppendUint32(append(w.buf, 0xd2), uint32(v))
	default:
		w.buf = binary.BigEndian.AppendUint64(append(w.buf, 0xd3), uint64(v))
	}
}

// Float writes a float 64 with its bits: -0 → cb 80 00 00 00 00 00 00 00.
func (w *Writer) Float(v float64) {
	w.buf = binary.BigEndian.AppendUint64(append(w.buf, 0xcb), math.Float64bits(v))
}

// Str writes text, which is UTF-8.
func (w *Writer) Str(s string) {
	w.length(len(s), 0xa0, 31, 0xd9, 0xda, 0xdb)
	w.buf = append(w.buf, s...)
}

func (w *Writer) Bin(b []byte) {
	w.length(len(b), 0, -1, 0xc4, 0xc5, 0xc6)
	w.buf = append(w.buf, b...)
}

func (w *Writer) Array(n int) { w.count(n, 0x90, 0xdc, 0xdd) }

func (w *Writer) Map(n int) { w.count(n, 0x80, 0xde, 0xdf) }

// OpenMap opens a map whose count is known once its entries are written: a
// byte is kept for it.
func (w *Writer) OpenMap() int {
	w.buf = append(w.buf, 0x80)
	return len(w.buf) - 1
}

// CloseMap writes the count of the map OpenMap opened at head, widening its
// head past 15.
func (w *Writer) CloseMap(head, n int) {
	if n <= 15 {
		w.buf[head] = 0x80 | byte(n)
		return
	}
	var wide []byte
	if n <= 0xffff {
		wide = binary.BigEndian.AppendUint16([]byte{0xde}, uint16(n))
	} else {
		wide = binary.BigEndian.AppendUint32([]byte{0xdf}, uint32(n))
	}
	w.buf = append(w.buf[:head], append(wide, w.buf[head+1:]...)...)
}

// length writes a str's or a bin's head: a fixed form up to fixMost, then
// one, two or four bytes of length.
func (w *Writer) length(n int, fix byte, fixMost int, one, two, four byte) {
	switch {
	case n <= fixMost:
		w.buf = append(w.buf, fix|byte(n))
	case n <= 0xff:
		w.buf = append(w.buf, one, byte(n))
	case n <= 0xffff:
		w.buf = binary.BigEndian.AppendUint16(append(w.buf, two), uint16(n))
	default:
		w.buf = binary.BigEndian.AppendUint32(append(w.buf, four), uint32(n))
	}
}

func (w *Writer) count(n int, fix, two, four byte) {
	switch {
	case n <= 15:
		w.buf = append(w.buf, fix|byte(n))
	case n <= 0xffff:
		w.buf = binary.BigEndian.AppendUint16(append(w.buf, two), uint16(n))
	default:
		w.buf = binary.BigEndian.AppendUint32(append(w.buf, four), uint32(n))
	}
}

// Reader reads one body under the profile: bounds before allocation,
// nesting eight deep at most, a str that is UTF-8, a key twice refused. Its
// first refusal is kept, and every read after it answers a zero value.
type Reader struct {
	body  []byte
	at    int
	depth int
	err   error
}

func NewReader(body []byte) *Reader { return &Reader{body: body} }

// Err is the first rule the body broke, nil while it broke none.
func (r *Reader) Err() error { return r.err }

// End is Err, or a refusal of bytes left after the value.
func (r *Reader) End() error {
	if r.err == nil && r.at != len(r.body) {
		r.Fail("%d bytes after the value", len(r.body)-r.at)
	}
	return r.err
}

// Fail keeps a refusal, the first one only.
func (r *Reader) Fail(format string, args ...any) {
	if r.err == nil {
		r.err = fmt.Errorf("wire: "+format, args...)
	}
}

// Type is the next value's type, nothing read; Invalid at the body's end.
func (r *Reader) Type() Type {
	if r.err != nil || r.at >= len(r.body) {
		return TypeInvalid
	}
	return TypeOf(r.body[r.at])
}

func (r *Reader) next() byte {
	if r.err != nil {
		return 0
	}
	if r.at >= len(r.body) {
		r.Fail("the body ends inside a value")
		return 0
	}
	b := r.body[r.at]
	r.at++
	return b
}

// take is where n bytes begin, refused when fewer are left.
func (r *Reader) take(n int) int {
	if r.err != nil {
		return 0
	}
	if n < 0 || n > len(r.body)-r.at {
		r.Fail("a length of %d past the %d bytes left", n, len(r.body)-r.at)
		return 0
	}
	at := r.at
	r.at += n
	return at
}

func (r *Reader) refuse(want string, b byte) {
	r.Fail("%s where %s belongs", TypeOf(b), want)
}

// integer reads any form of an integer: a value from 0 as its uint64, a
// negative one as its int64 with negative set.
func (r *Reader) integer(want string) (value uint64, negative bool) {
	b := r.next()
	if r.err != nil {
		return 0, false
	}
	switch {
	case b <= 0x7f:
		return uint64(b), false
	case b >= 0xe0:
		return uint64(int64(int8(b))), true
	}
	switch b {
	case 0xcc:
		return uint64(r.body[r.take(1)]), false
	case 0xcd:
		return uint64(binary.BigEndian.Uint16(r.slice(2))), false
	case 0xce:
		return uint64(binary.BigEndian.Uint32(r.slice(4))), false
	case 0xcf:
		return binary.BigEndian.Uint64(r.slice(8)), false
	case 0xd0:
		return signed(int64(int8(r.body[r.take(1)])))
	case 0xd1:
		return signed(int64(int16(binary.BigEndian.Uint16(r.slice(2)))))
	case 0xd2:
		return signed(int64(int32(binary.BigEndian.Uint32(r.slice(4)))))
	case 0xd3:
		return signed(int64(binary.BigEndian.Uint64(r.slice(8))))
	}
	r.refuse(want, b)
	return 0, false
}

func signed(v int64) (uint64, bool) { return uint64(v), v < 0 }

// slice is the next n bytes, zeros once the body broke a rule.
func (r *Reader) slice(n int) []byte {
	at := r.take(n)
	if r.err != nil {
		return make([]byte, n)
	}
	return r.body[at : at+n]
}

// Uint reads an integer from 0, in any form.
func (r *Reader) Uint() uint64 {
	v, negative := r.integer("an unsigned integer")
	if negative {
		r.Fail("a negative integer where an unsigned one belongs")
		return 0
	}
	return v
}

// Int reads a signed 64-bit integer, in any form.
func (r *Reader) Int() int64 {
	v, negative := r.integer("an integer")
	if !negative && v > math.MaxInt64 {
		r.Fail("%d, past an int 64, where an integer belongs", v)
		return 0
	}
	return int64(v)
}

// Float reads a float 64, or an integer a float 64 holds exactly, as
// JavaScript writes 3.0 as 3.
func (r *Reader) Float() float64 {
	if r.Type() == TypeInteger {
		v, negative := r.integer("a float")
		f := float64(int64(v))
		if !negative {
			f = float64(v)
		}
		if (!negative && (f >= 1<<64 || uint64(f) != v)) || (negative && int64(f) != int64(v)) {
			r.Fail("an integer a float does not hold exactly")
			return 0
		}
		return f
	}
	b := r.next()
	if r.err != nil {
		return 0
	}
	if b != 0xcb {
		r.refuse("a float", b)
		return 0
	}
	return math.Float64frombits(binary.BigEndian.Uint64(r.slice(8)))
}

func (r *Reader) Bool() bool {
	b := r.next()
	switch {
	case r.err != nil:
		return false
	case b == 0xc3:
		return true
	case b == 0xc2:
		return false
	}
	r.refuse("a bool", b)
	return false
}

// Nil reads a nil if one is next, and says whether it was.
func (r *Reader) Nil() bool {
	if r.Type() != TypeNil {
		return false
	}
	r.at++
	return true
}

func (r *Reader) Str() string {
	b := r.next()
	if r.err != nil {
		return ""
	}
	var n int
	switch {
	case b&0xe0 == 0xa0:
		n = int(b & 0x1f)
	case b == 0xd9:
		n = int(r.body[r.take(1)])
	case b == 0xda:
		n = int(binary.BigEndian.Uint16(r.slice(2)))
	case b == 0xdb:
		n = int(binary.BigEndian.Uint32(r.slice(4)))
	default:
		r.refuse("a str", b)
		return ""
	}
	text := r.slice(n)
	if r.err != nil {
		return ""
	}
	if !utf8.Valid(text) {
		r.Fail("a str that is not UTF-8")
		return ""
	}
	return string(text)
}

// Bin reads bytes, a copy that outlives the body.
func (r *Reader) Bin() []byte {
	b := r.next()
	if r.err != nil {
		return nil
	}
	var n int
	switch b {
	case 0xc4:
		n = int(r.body[r.take(1)])
	case 0xc5:
		n = int(binary.BigEndian.Uint16(r.slice(2)))
	case 0xc6:
		n = int(binary.BigEndian.Uint32(r.slice(4)))
	default:
		r.refuse("a bin", b)
		return nil
	}
	return append([]byte{}, r.slice(n)...)
}

// Array reads an array's count and enters it; Leave once its elements are
// read. A count past the bytes left is refused before anything is made of
// it, since every element takes a byte at least.
func (r *Reader) Array() int { return r.enter("an array", 0x90, 0xdc, 0xdd, 1) }

// Map reads a map's count and enters it, each entry taking two bytes at least.
func (r *Reader) Map() int { return r.enter("a map", 0x80, 0xde, 0xdf, 2) }

func (r *Reader) Leave() { r.depth-- }

func (r *Reader) enter(want string, fix, two, four byte, each int) int {
	b := r.next()
	if r.err != nil {
		r.depth++
		return 0
	}
	var n int
	switch {
	case b&0xf0 == fix:
		n = int(b & 0x0f)
	case b == two:
		n = int(binary.BigEndian.Uint16(r.slice(2)))
	case b == four:
		n = int(binary.BigEndian.Uint32(r.slice(4)))
	default:
		r.refuse(want, b)
	}
	r.depth++
	switch {
	case r.err != nil:
		return 0
	case n*each > len(r.body)-r.at:
		r.Fail("%s of %d elements in %d bytes", want, n, len(r.body)-r.at)
		return 0
	case r.depth > MaxDepth:
		r.Fail("%s nested deeper than %d", want, MaxDepth)
		return 0
	}
	return n
}

// Seen is the keys a message's reader met, for a key twice to be refused:
// a schema's keys are a byte, and any other is kept apart.
type Seen struct {
	small [4]uint64
	large map[uint64]struct{}
}

// Field reads a message's next key, refusing one it met already.
func (r *Reader) Field(seen *Seen) uint64 {
	key := r.Uint()
	if r.err != nil {
		return 0
	}
	twice := false
	if key < 256 {
		twice = seen.small[key/64]&(1<<(key%64)) != 0
		seen.small[key/64] |= 1 << (key % 64)
	} else {
		if seen.large == nil {
			seen.large = map[uint64]struct{}{}
		}
		_, twice = seen.large[key]
		seen.large[key] = struct{}{}
	}
	if twice {
		r.Fail("key %d twice", key)
		return 0
	}
	return key
}

// Name reads a map of names' next name, refusing one it met already.
func (r *Reader) Name(seen map[string]struct{}) string {
	name := r.Str()
	if r.err != nil {
		return ""
	}
	if _, twice := seen[name]; twice {
		r.Fail("name %q twice", name)
		return ""
	}
	seen[name] = struct{}{}
	return name
}

// Skip reads past one value, checking it is well formed as if it were read.
func (r *Reader) Skip() {
	switch r.Type() {
	case TypeNil:
		r.at++
	case TypeBool:
		r.Bool()
	case TypeInteger:
		r.integer("an integer")
	case TypeFloat:
		r.Float()
	case TypeStr:
		r.Str()
	case TypeBin:
		r.Bin()
	case TypeArray:
		for n := r.Array(); n > 0; n-- {
			r.Skip()
		}
		r.Leave()
	case TypeMap:
		r.skipMap()
	default:
		if r.err == nil && r.at >= len(r.body) {
			r.Fail("the body ends where a value belongs")
		} else if r.err == nil {
			r.Fail("%#02x, which the profile does not take", r.body[r.at])
		}
	}
}

// skipMap reads past a map whose keys are small unsigned integers or names,
// as a message's maps are.
func (r *Reader) skipMap() {
	n := r.Map()
	if n > 0 && r.Type() == TypeStr {
		seen := map[string]struct{}{}
		for ; n > 0; n-- {
			r.Name(seen)
			r.Skip()
		}
	} else {
		var seen Seen
		for ; n > 0; n-- {
			r.Field(&seen)
			r.Skip()
		}
	}
	r.Leave()
}
