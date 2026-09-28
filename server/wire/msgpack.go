package wire

import (
	"encoding/binary"
	"errors"
	"fmt"
	"iter"
	"math"
	"slices"
	"unicode/utf8"
)

// the type bytes of MessagePack this profile writes or reads; fixint, fixmap,
// fixarray, fixstr and the negative fixint are ranges of their own
const (
	mpNil     = 0xc0
	mpFalse   = 0xc2
	mpTrue    = 0xc3
	mpBin8    = 0xc4
	mpBin16   = 0xc5
	mpBin32   = 0xc6
	mpFloat64 = 0xcb
	mpUint8   = 0xcc
	mpUint16  = 0xcd
	mpUint32  = 0xce
	mpUint64  = 0xcf
	mpInt8    = 0xd0
	mpInt16   = 0xd1
	mpInt32   = 0xd2
	mpInt64   = 0xd3
	mpStr8    = 0xd9
	mpStr16   = 0xda
	mpStr32   = 0xdb
	mpArray16 = 0xdc
	mpArray32 = 0xdd
	mpMap16   = 0xde
	mpMap32   = 0xdf

	fixMap   = 0x80
	fixArray = 0x90
	fixStr   = 0xa0
	negFix   = 0xe0
)

// maxDepth is how deeply maps and arrays may nest in a message
const maxDepth = 8

// An encoder writes the shortest form of every integer, length and count and
// every float in eight bytes, so that a message compares byte for byte.

func AppendNil(dst []byte) []byte {
	return append(dst, mpNil)
}

func AppendBool(dst []byte, v bool) []byte {
	if v {
		return append(dst, mpTrue)
	}
	return append(dst, mpFalse)
}

// AppendUint writes v in the unsigned family's shortest form:
//
//	7 → 07    300 → cd 01 2c    70000 → ce 00 01 11 70
func AppendUint(dst []byte, v uint64) []byte {
	switch {
	case v <= 0x7f:
		return append(dst, byte(v))
	case v <= math.MaxUint8:
		return append(dst, mpUint8, byte(v))
	case v <= math.MaxUint16:
		return binary.BigEndian.AppendUint16(append(dst, mpUint16), uint16(v))
	case v <= math.MaxUint32:
		return binary.BigEndian.AppendUint32(append(dst, mpUint32), uint32(v))
	}
	return binary.BigEndian.AppendUint64(append(dst, mpUint64), v)
}

// AppendInt writes a value from 0 as AppendUint does, and a negative one in
// the signed family's shortest form:
//
//	-1 → ff    -33 → d0 df    -300 → d1 fe d4
func AppendInt(dst []byte, v int64) []byte {
	switch {
	case v >= 0:
		return AppendUint(dst, uint64(v))
	case v >= -32:
		return append(dst, low(v))
	case v >= math.MinInt8:
		return append(dst, mpInt8, low(v))
	case v >= math.MinInt16:
		return binary.BigEndian.AppendUint16(append(dst, mpInt16), uint16(v)) //nolint:gosec // the signed bits
	case v >= math.MinInt32:
		return binary.BigEndian.AppendUint32(append(dst, mpInt32), uint32(v)) //nolint:gosec // the signed bits
	}
	return binary.BigEndian.AppendUint64(append(dst, mpInt64), uint64(v)) //nolint:gosec // the signed bits
}

// AppendFloat writes a float 64 with its bits: -0 → cb 80 00 00 00 00 00 00 00.
func AppendFloat(dst []byte, v float64) []byte {
	return binary.BigEndian.AppendUint64(append(dst, mpFloat64), math.Float64bits(v))
}

// AppendStr writes text, which the caller keeps valid UTF-8.
func AppendStr(dst []byte, s string) []byte {
	dst = appendLength(dst, len(s), fixStr, 31, mpStr8, mpStr16, mpStr32)
	return append(dst, s...)
}

func AppendBin(dst []byte, b []byte) []byte {
	dst = appendLength(dst, len(b), 0, -1, mpBin8, mpBin16, mpBin32)
	return append(dst, b...)
}

func AppendArray(dst []byte, n int) []byte {
	return appendCount(dst, n, fixArray, mpArray16, mpArray32)
}

func AppendMap(dst []byte, n int) []byte {
	return appendCount(dst, n, fixMap, mpMap16, mpMap32)
}

// low is the one byte a value in its case's range writes: a fixint, a length
// or a count up to 255, or a small negative's two's complement
func low[T int | int64](v T) byte {
	return byte(v)
}

// appendLength writes a str's or a bin's length: in the fix byte up to
// fixMost, which a bin lacks, then in one, two or four bytes
func appendLength(dst []byte, n int, fix byte, fixMost int, one, two, four byte) []byte {
	switch {
	case n <= fixMost:
		return append(dst, fix|low(n))
	case n <= math.MaxUint8:
		return append(dst, one, low(n))
	case n <= math.MaxUint16:
		return binary.BigEndian.AppendUint16(append(dst, two), uint16(n))
	}
	return binary.BigEndian.AppendUint32(append(dst, four), uint32(n)) //nolint:gosec // a body within its bound
}

func appendCount(dst []byte, n int, fix, two, four byte) []byte {
	switch {
	case n <= 15:
		return append(dst, fix|low(n))
	case n <= math.MaxUint16:
		return binary.BigEndian.AppendUint16(append(dst, two), uint16(n))
	}
	return binary.BigEndian.AppendUint32(append(dst, four), uint32(n)) //nolint:gosec // a body within its bound
}

// Map appends a message a field at a time, keys ascending, counting the fields
// as they are added, so that a field the message lacks is simply not written.
// Its count is one byte, set by End, which makes room for a map 16's three
// when sixteen fields or more were written.
type Map struct {
	buf   []byte
	start int
	n     int
}

func BeginMap(dst []byte) Map {
	return Map{buf: append(dst, fixMap), start: len(dst)}
}

// Key writes a key and counts its field; its value is appended to Buf next.
func (m *Map) Key(key uint64) {
	m.n++
	m.buf = AppendUint(m.buf, key)
}

func (m *Map) Uint(key, v uint64) {
	m.Key(key)
	m.buf = AppendUint(m.buf, v)
}

func (m *Map) Int(key uint64, v int64) {
	m.Key(key)
	m.buf = AppendInt(m.buf, v)
}

func (m *Map) Float(key uint64, v float64) {
	m.Key(key)
	m.buf = AppendFloat(m.buf, v)
}

func (m *Map) Bool(key uint64, v bool) {
	m.Key(key)
	m.buf = AppendBool(m.buf, v)
}

func (m *Map) Str(key uint64, s string) {
	m.Key(key)
	m.buf = AppendStr(m.buf, s)
}

func (m *Map) Bin(key uint64, b []byte) {
	m.Key(key)
	m.buf = AppendBin(m.buf, b)
}

// Buf is the message so far, for a value Key has made room for; the caller
// hands back what it appended with SetBuf.
func (m *Map) Buf() []byte {
	return m.buf
}

func (m *Map) SetBuf(buf []byte) {
	m.buf = buf
}

// End sets the count and returns the message.
func (m *Map) End() []byte {
	if m.n <= 15 {
		m.buf[m.start] = fixMap | low(m.n)
		return m.buf
	}
	m.buf = slices.Insert(m.buf, m.start+1, 0, 0)
	m.buf[m.start] = mpMap16
	binary.BigEndian.PutUint16(m.buf[m.start+1:], uint16(m.n)) //nolint:gosec // a message's fields, far fewer
	return m.buf
}

// ErrMessage is a body that the profile does not allow, or that is not the
// message its frame and method ask for.
var ErrMessage = errors.New("invalid message")

// Type is the kind of the next value a Decoder holds.
type Type uint8

const (
	TypeInvalid Type = iota
	TypeNil
	TypeBool
	TypeInt
	TypeFloat
	TypeStr
	TypeBin
	TypeArray
	TypeMap
)

var typeNames = [...]string{
	"an invalid type", "nil", "a bool", "an integer", "a float", "a str", "a bin",
	"an array", "a map",
}

func (t Type) String() string {
	return typeNames[t]
}

// typeOf is the type a value's first byte says
func typeOf(b byte) Type {
	switch {
	case b <= 0x7f, b >= negFix, b >= mpUint8 && b <= mpInt64:
		return TypeInt
	case b&0xf0 == fixMap, b == mpMap16, b == mpMap32:
		return TypeMap
	case b&0xf0 == fixArray, b == mpArray16, b == mpArray32:
		return TypeArray
	case b&0xe0 == fixStr, b >= mpStr8 && b <= mpStr32:
		return TypeStr
	case b >= mpBin8 && b <= mpBin32:
		return TypeBin
	case b == mpNil:
		return TypeNil
	case b == mpFalse, b == mpTrue:
		return TypeBool
	case b == mpFloat64:
		return TypeFloat
	}
	return TypeInvalid // a float 32, an extension, or the byte never used
}

// Decoder reads one message and checks the profile as it goes. Its first
// error sticks and every read after it returns a zero value, so a message's
// decoder reads each field and asks End once. What it returns of the body
// aliases the body.
type Decoder struct {
	body  []byte
	at    int
	depth int
	err   error
}

func NewDecoder(body []byte) Decoder {
	return Decoder{body: body}
}

func (d *Decoder) Err() error {
	return d.err
}

// End is the message's error, or one when bytes follow its value: a message
// is one value.
func (d *Decoder) End() error {
	if d.err == nil && d.at != len(d.body) {
		d.fail("%d bytes after the message", len(d.body)-d.at)
	}
	return d.err
}

// Fail refuses the message for a reason of its own, as a field out of range.
func (d *Decoder) Fail(format string, args ...any) {
	d.fail(format, args...)
}

func (d *Decoder) fail(format string, args ...any) {
	if d.err == nil {
		d.err = fmt.Errorf("%w: "+format, append([]any{ErrMessage}, args...)...)
	}
}

// Type is the next value's type, without reading it.
func (d *Decoder) Type() Type {
	if d.err != nil || d.at >= len(d.body) {
		return TypeInvalid
	}
	return typeOf(d.body[d.at])
}

func (d *Decoder) next() byte {
	if d.err != nil {
		return mpNil
	}
	if d.at >= len(d.body) {
		d.fail("the body ends inside a value")
		return mpNil
	}
	b := d.body[d.at]
	d.at++
	return b
}

func (d *Decoder) take(n int) []byte {
	if d.err != nil {
		return nil
	}
	if n > len(d.body)-d.at {
		d.fail("%d bytes asked for where %d are left", n, len(d.body)-d.at)
		return nil
	}
	taken := d.body[d.at : d.at+n : d.at+n]
	d.at += n
	return taken
}

func (d *Decoder) u8() uint64 {
	if b := d.take(1); b != nil {
		return uint64(b[0])
	}
	return 0
}

func (d *Decoder) u16() uint64 {
	if b := d.take(2); b != nil {
		return uint64(binary.BigEndian.Uint16(b))
	}
	return 0
}

func (d *Decoder) u32() uint64 {
	if b := d.take(4); b != nil {
		return uint64(binary.BigEndian.Uint32(b))
	}
	return 0
}

func (d *Decoder) u64() uint64 {
	if b := d.take(8); b != nil {
		return binary.BigEndian.Uint64(b)
	}
	return 0
}

// integer reads any encoding of an integer: negative says v holds a negative
// value's bits, and otherwise v is the value, a uint64 past int64 included
func (d *Decoder) integer(want string) (v uint64, negative bool) {
	b := d.next()
	switch {
	case d.err != nil:
		return 0, false
	case b <= 0x7f:
		return uint64(b), false
	case b >= negFix:
		return uint64(int64(int8(b))), true //nolint:gosec // the signed bits
	case b == mpUint8:
		return d.u8(), false
	case b == mpUint16:
		return d.u16(), false
	case b == mpUint32:
		return d.u32(), false
	case b == mpUint64:
		return d.u64(), false
	case b == mpInt8:
		v = uint64(int64(int8(d.u8()))) //nolint:gosec // the signed bits
	case b == mpInt16:
		v = uint64(int64(int16(d.u16()))) //nolint:gosec // the signed bits
	case b == mpInt32:
		v = uint64(int64(int32(d.u32()))) //nolint:gosec // the signed bits
	case b == mpInt64:
		v = d.u64()
	default:
		d.fail("%s where %s belongs", typeOf(b), want)
		return 0, false
	}
	return v, int64(v) < 0 //nolint:gosec // the signed bits
}

// Uint reads an unsigned integer in any encoding of its value.
func (d *Decoder) Uint() uint64 {
	v, negative := d.integer("an unsigned integer")
	if negative {
		d.fail("%d where an unsigned integer belongs", int64(v)) //nolint:gosec // the signed bits
		return 0
	}
	return v
}

// Int reads an integer in any encoding of its value.
func (d *Decoder) Int() int64 {
	v, negative := d.integer("an integer")
	if !negative && v > math.MaxInt64 {
		d.fail("%d where an int64 belongs", v)
		return 0
	}
	return int64(v) //nolint:gosec // the signed bits
}

// Float reads a float 64 with its bits, or an integer a float 64 holds
// exactly, since a JavaScript encoder writes 3.0 as 3.
func (d *Decoder) Float() float64 {
	switch d.Type() {
	case TypeFloat:
		d.at++
		return math.Float64frombits(d.u64())
	case TypeInt:
		return d.exactFloat()
	}
	d.fail("%s where a float belongs", d.Type())
	return 0
}

// the bound past which a uint64 no longer converts back from a float
const twoTo64 = 1 << 64

func (d *Decoder) exactFloat() float64 {
	v, negative := d.integer("a float")
	if negative {
		i := int64(v) //nolint:gosec // the signed bits
		if f := float64(i); int64(f) == i {
			return f
		}
		d.fail("%d, which a float 64 does not hold exactly", i)
		return 0
	}
	if f := float64(v); f < twoTo64 && uint64(f) == v {
		return f
	}
	d.fail("%d, which a float 64 does not hold exactly", v)
	return 0
}

func (d *Decoder) Bool() bool {
	switch b := d.next(); {
	case d.err != nil:
		return false
	case b == mpTrue:
		return true
	case b != mpFalse:
		d.fail("%s where a bool belongs", typeOf(b))
	}
	return false
}

// Nil reads a nil if one is next and says so; any other value is left for
// the field's own reader. A field allows nil only where it says it does.
func (d *Decoder) Nil() bool {
	if d.Type() == TypeNil {
		d.at++
		return true
	}
	return false
}

// StrBytes reads a str, which must be valid UTF-8, aliasing the body.
func (d *Decoder) StrBytes() []byte {
	b := d.next()
	var n uint64
	switch {
	case d.err != nil:
		return nil
	case b&0xe0 == fixStr:
		n = uint64(b & 0x1f)
	case b == mpStr8:
		n = d.u8()
	case b == mpStr16:
		n = d.u16()
	case b == mpStr32:
		n = d.u32()
	default:
		d.fail("%s where a str belongs", typeOf(b))
		return nil
	}
	text := d.take(int(n))
	if d.err == nil && !utf8.Valid(text) {
		d.fail("a str that is not UTF-8")
		return nil
	}
	return text
}

func (d *Decoder) Str() string {
	return string(d.StrBytes())
}

// Bin reads a bin, aliasing the body; an empty one is not nil.
func (d *Decoder) Bin() []byte {
	b := d.next()
	var n uint64
	switch {
	case d.err != nil:
		return nil
	case b == mpBin8:
		n = d.u8()
	case b == mpBin16:
		n = d.u16()
	case b == mpBin32:
		n = d.u32()
	default:
		d.fail("%s where a bin belongs", typeOf(b))
		return nil
	}
	return d.take(int(n)) //nolint:gosec // at most four bytes of length
}

// count reads a map's or an array's count and refuses one past the bytes
// left, each element taking at least a byte and a map's entry two
func (d *Decoder) count(kind Type, fix, two, four byte, each int) int {
	b := d.next()
	var n uint64
	switch {
	case d.err != nil:
		return 0
	case b&0xf0 == fix:
		n = uint64(b & 0x0f)
	case b == two:
		n = d.u16()
	case b == four:
		n = d.u32()
	default:
		d.fail("%s where %s belongs", typeOf(b), kind)
		return 0
	}
	if left := uint64(len(d.body) - d.at); d.err == nil && n*uint64(each) > left { //nolint:gosec // never negative
		d.fail("%s of %d elements in %d bytes", kind, n, left)
		return 0
	}
	return int(n)
}

func (d *Decoder) enter(kind Type) bool {
	if d.depth == maxDepth {
		d.fail("%s nested deeper than %d", kind, maxDepth)
		return false
	}
	d.depth++
	return true
}

// Fields reads a map with unsigned keys, a message or a field of one,
// yielding each key with its value next: the loop reads the value, or leaves
// it to be skipped, well formed and within bounds. A key twice is invalid.
func (d *Decoder) Fields() iter.Seq[uint64] {
	return func(yield func(uint64) bool) {
		n := d.count(TypeMap, fixMap, mpMap16, mpMap32, 2)
		if d.err != nil || !d.enter(TypeMap) {
			return
		}
		defer func() { d.depth-- }()
		var seen keys
		for range n {
			key := d.Uint()
			if d.err == nil && !seen.add(key) {
				d.fail("key %d twice", key)
			}
			if d.err != nil {
				return
			}
			at := d.at
			if !yield(key) {
				return
			}
			if d.at == at {
				d.Skip()
			}
		}
	}
}

// Names reads a map of names, an error's what or a series' labels, yielding
// each name with its value next, as Fields does.
func (d *Decoder) Names() iter.Seq[string] {
	return func(yield func(string) bool) {
		n := d.count(TypeMap, fixMap, mpMap16, mpMap32, 2)
		if d.err != nil || !d.enter(TypeMap) {
			return
		}
		defer func() { d.depth-- }()
		var seen names
		for range n {
			name := d.Str()
			if d.err == nil && !seen.add(name) {
				d.fail("name %q twice", name)
			}
			if d.err != nil {
				return
			}
			at := d.at
			if !yield(name) {
				return
			}
			if d.at == at {
				d.Skip()
			}
		}
	}
}

// Items reads an array, yielding each index with its element next.
func (d *Decoder) Items() iter.Seq[int] {
	return func(yield func(int) bool) {
		n := d.count(TypeArray, fixArray, mpArray16, mpArray32, 1)
		if d.err != nil || !d.enter(TypeArray) {
			return
		}
		defer func() { d.depth-- }()
		for i := range n {
			at := d.at
			if d.err != nil || !yield(i) {
				return
			}
			if d.at == at {
				d.Skip()
			}
		}
	}
}

// Skip reads past one value, checking that it is well formed and within
// bounds as if it were read.
func (d *Decoder) Skip() {
	if d.err != nil {
		return
	}
	switch d.Type() {
	case TypeNil, TypeBool:
		d.at++
	case TypeInt:
		d.integer("an integer")
	case TypeFloat:
		d.Float()
	case TypeStr:
		d.StrBytes()
	case TypeBin:
		d.Bin()
	case TypeArray:
		for range d.Items() {
			d.Skip()
		}
	case TypeMap:
		d.skipMap()
	default:
		if d.at >= len(d.body) {
			d.fail("the body ends where a value belongs")
			return
		}
		d.fail("%s", d.describe())
	}
}

// skipMap reads past a map, whose keys are unsigned integers or names as a
// message's maps are, each once
func (d *Decoder) skipMap() {
	at := d.at
	if d.count(TypeMap, fixMap, mpMap16, mpMap32, 2) > 0 && d.Type() == TypeStr {
		d.at = at
		for range d.Names() {
			d.Skip()
		}
		return
	}
	d.at = at
	for range d.Fields() {
		d.Skip()
	}
}

// describe names an invalid type byte
func (d *Decoder) describe() string {
	switch b := d.body[d.at]; b {
	case 0xca:
		return "a float 32, which the profile does not allow"
	case 0xc1:
		return "the type byte 0xc1, which MessagePack never uses"
	default:
		return fmt.Sprintf("the extension type %#x, which the profile does not allow", b)
	}
}

// keys is the keys a map has shown: those under 64 in bits, the rest in a map
// that only a message with unknown keys pays for
type keys struct {
	low  uint64
	high map[uint64]struct{}
}

func (k *keys) add(key uint64) bool {
	if key < 64 {
		bit := uint64(1) << key
		fresh := k.low&bit == 0
		k.low |= bit
		return fresh
	}
	if _, seen := k.high[key]; seen {
		return false
	}
	if k.high == nil {
		k.high = map[uint64]struct{}{}
	}
	k.high[key] = struct{}{}
	return true
}

// names is the names a map has shown: a few in a row, the rest in a map
type names struct {
	few  [8]string
	n    int
	many map[string]struct{}
}

func (s *names) add(name string) bool {
	if s.many != nil {
		if _, seen := s.many[name]; seen {
			return false
		}
		s.many[name] = struct{}{}
		return true
	}
	if slices.Contains(s.few[:s.n], name) {
		return false
	}
	if s.n < len(s.few) {
		s.few[s.n] = name
		s.n++
		return true
	}
	s.many = map[string]struct{}{name: {}}
	for _, earlier := range s.few {
		s.many[earlier] = struct{}{}
	}
	return true
}
