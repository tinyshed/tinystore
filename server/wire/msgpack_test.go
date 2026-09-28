package wire_test

import (
	"bytes"
	"cmp"
	"errors"
	"math"
	"reflect"
	"slices"
	"testing"

	"github.com/tinyshed/tinystore/server/wire"
)

func TestIntegersAreWrittenShortestAndReadInAnyForm(t *testing.T) {
	unsigned := []struct {
		v    uint64
		size int
	}{
		{0, 1},
		{0x7f, 1},
		{0x80, 2},
		{0xff, 2},
		{0x100, 3},
		{0xffff, 3},
		{0x10000, 5},
		{math.MaxUint32, 5},
		{math.MaxUint32 + 1, 9},
		{math.MaxUint64, 9},
	}
	for _, c := range unsigned {
		written := wire.AppendUint(nil, c.v)
		d := wire.NewDecoder(written)
		if got := d.Uint(); got != c.v || len(written) != c.size || d.End() != nil {
			t.Errorf("%d: %x read as %d, %v", c.v, written, got, d.Err())
		}
	}

	signed := []struct {
		v    int64
		size int
	}{
		{-1, 1},
		{-32, 1},
		{-33, 2},
		{math.MinInt8, 2},
		{math.MinInt8 - 1, 3},
		{math.MinInt16, 3},
		{math.MinInt16 - 1, 5},
		{math.MinInt32, 5},
		{math.MinInt32 - 1, 9},
		{math.MinInt64, 9},
		{5, 1},
		{math.MaxInt64, 9},
	}
	for _, c := range signed {
		written := wire.AppendInt(nil, c.v)
		d := wire.NewDecoder(written)
		if got := d.Int(); got != c.v || len(written) != c.size || d.End() != nil {
			t.Errorf("%d: %x read as %d, %v", c.v, written, got, d.Err())
		}
	}
}

func TestFloatsKeepTheirBits(t *testing.T) {
	for _, bits := range []uint64{
		0x8000000000000000, // -0
		0x7ff8000000000001, // a quiet NaN whose payload is 1
		0x7ff0000000000001, // a NaN that signals
		0xfff8000000000000, // a negative NaN
		0x7ff0000000000000, // +Inf
		0xfff0000000000000, // -Inf
		0x0000000000000001, // the smallest subnormal
		0x3ff0000000000000, // 1
	} {
		written := wire.AppendFloat(nil, math.Float64frombits(bits))
		d := wire.NewDecoder(written)
		if got := math.Float64bits(d.Float()); got != bits || d.End() != nil {
			t.Errorf("%016x: read %016x, %v", bits, got, d.Err())
		}
	}
}

func TestAFloatTakesOnlyTheIntegersItHoldsExactly(t *testing.T) {
	for _, v := range []int64{0, 3, -3, 1 << 53, -(1 << 53), math.MinInt64, 1 << 62} {
		d := wire.NewDecoder(wire.AppendInt(nil, v))
		if got := d.Float(); got != float64(v) || d.End() != nil {
			t.Errorf("%d read as %v, %v", v, got, d.Err())
		}
	}
	for _, v := range []uint64{1<<53 + 1, math.MaxUint64, math.MaxInt64} {
		d := wire.NewDecoder(wire.AppendUint(nil, v))
		d.Float()
		if !errors.Is(d.End(), wire.ErrMessage) {
			t.Errorf("%d taken as a float", v)
		}
	}
}

func TestLengthsAreWrittenShortest(t *testing.T) {
	for _, n := range []int{0, 31, 32, 255, 256, 65535, 65536} {
		text := string(bytes.Repeat([]byte("a"), n))
		written := wire.AppendStr(nil, text)
		d := wire.NewDecoder(written)
		if got := d.Str(); got != text || d.End() != nil {
			t.Errorf("a str of %d: %v", n, d.Err())
		}
		wantHead := map[bool]int{n <= 31: 1, n > 31 && n <= 255: 2, n > 255 && n <= 65535: 3, n > 65535: 5}[true]
		if len(written)-n != wantHead {
			t.Errorf("a str of %d took a head of %d bytes", n, len(written)-n)
		}

		raw := bytes.Repeat([]byte{7}, n)
		d = wire.NewDecoder(wire.AppendBin(nil, raw))
		if got := d.Bin(); !bytes.Equal(got, raw) || d.End() != nil {
			t.Errorf("a bin of %d: %v", n, d.Err())
		}
	}
}

func TestAMapCountsTheFieldsItWasGiven(t *testing.T) {
	m := wire.BeginMap([]byte("before"))
	m.Uint(1, 300)
	m.Str(3, "three")
	m.Bool(9, true)
	written := m.End()
	if !bytes.HasPrefix(written, []byte("before")) {
		t.Fatalf("what came before was lost: %q", written)
	}
	d := wire.NewDecoder(written[len("before"):])
	var keys []uint64
	for key := range d.Fields() {
		keys = append(keys, key)
		d.Skip()
	}
	if d.End() != nil || !slices.Equal(keys, []uint64{1, 3, 9}) {
		t.Fatalf("keys %v, %v", keys, d.Err())
	}
}

// the generic reading a fuzzer compares: a float by its bits, a str and a bin
// apart, a map as its pairs in the order written
type (
	floatBits uint64
	strValue  string
	binValue  string
	mapValue  [][2]any
)

func readAny(d *wire.Decoder) any {
	switch d.Type() {
	case wire.TypeNil:
		d.Nil()
		return nil
	case wire.TypeBool:
		return d.Bool()
	case wire.TypeInt:
		probe := *d
		if v := probe.Int(); probe.Err() == nil {
			*d = probe
			if v < 0 {
				return v
			}
			return uint64(v)
		}
		return d.Uint()
	case wire.TypeFloat:
		return floatBits(math.Float64bits(d.Float()))
	case wire.TypeStr:
		return strValue(d.Str())
	case wire.TypeBin:
		return binValue(d.Bin())
	case wire.TypeArray:
		items := []any{}
		for range d.Items() {
			items = append(items, readAny(d))
		}
		return items
	case wire.TypeMap:
		return readAnyMap(d)
	}
	d.Skip()
	return nil
}

func readAnyMap(d *wire.Decoder) mapValue {
	pairs := mapValue{}
	probe := *d
	named := false
	for range probe.Names() {
		named = true
		break
	}
	if named && probe.Err() == nil {
		for name := range d.Names() {
			pairs = append(pairs, [2]any{strValue(name), readAny(d)})
		}
		return pairs
	}
	for key := range d.Fields() {
		pairs = append(pairs, [2]any{key, readAny(d)})
	}
	return pairs
}

// writeAny writes what readAny read, canonically
func writeAny(dst []byte, value any) []byte {
	switch value := value.(type) {
	case nil:
		return wire.AppendNil(dst)
	case bool:
		return wire.AppendBool(dst, value)
	case uint64:
		return wire.AppendUint(dst, value)
	case int64:
		return wire.AppendInt(dst, value)
	case floatBits:
		return wire.AppendFloat(dst, math.Float64frombits(uint64(value)))
	case strValue:
		return wire.AppendStr(dst, string(value))
	case binValue:
		return wire.AppendBin(dst, []byte(value))
	case []any:
		dst = wire.AppendArray(dst, len(value))
		for _, item := range value {
			dst = writeAny(dst, item)
		}
		return dst
	case mapValue:
		pairs := slices.Clone(value)
		slices.SortFunc(pairs, func(a, b [2]any) int {
			if ka, ok := a[0].(uint64); ok {
				return cmp.Compare(ka, b[0].(uint64))
			}
			return cmp.Compare(a[0].(strValue), b[0].(strValue))
		})
		dst = wire.AppendMap(dst, len(pairs))
		for _, pair := range pairs {
			dst = writeAny(writeAny(dst, pair[0]), pair[1])
		}
		return dst
	}
	panic("a value readAny does not make")
}

func FuzzMessages(f *testing.F) {
	for _, vector := range readVectors(f).Values {
		f.Add(unhex(f, vector.Hex))
	}
	for _, vector := range readVectors(f).Refused {
		f.Add(unhex(f, vector.Hex))
	}
	f.Add(wire.Welcome{Protocol: 1, Server: "0.1", Instance: make([]byte, 16), Engines: []string{"kv"}}.Append(nil))
	f.Fuzz(func(t *testing.T, body []byte) {
		for _, message := range []interface{ Decode([]byte) error }{
			&wire.Hello{}, &wire.Welcome{}, &wire.GoAway{}, &wire.Error{},
		} {
			_ = message.Decode(body)
		}

		d := wire.NewDecoder(body)
		value := readAny(&d)
		if d.End() != nil {
			return
		}
		canonical := writeAny(nil, value)
		again := wire.NewDecoder(canonical)
		if reread := readAny(&again); again.End() != nil || !reflect.DeepEqual(reread, sortedPairs(value)) {
			t.Fatalf("%x read as %v, written %x, read again as %v (%v)", body, value, canonical, reread, again.Err())
		}
		if len(canonical) > len(body) {
			t.Fatalf("the canonical %x is longer than %x", canonical, body)
		}
	})
}

// sortedPairs is value with every map's pairs in the canonical order
func sortedPairs(value any) any {
	switch value := value.(type) {
	case []any:
		items := make([]any, len(value))
		for i, item := range value {
			items[i] = sortedPairs(item)
		}
		return items
	case mapValue:
		pairs := make(mapValue, len(value))
		for i, pair := range value {
			pairs[i] = [2]any{pair[0], sortedPairs(pair[1])}
		}
		slices.SortFunc(pairs, func(a, b [2]any) int {
			if ka, ok := a[0].(uint64); ok {
				return cmp.Compare(ka, b[0].(uint64))
			}
			return cmp.Compare(a[0].(strValue), b[0].(strValue))
		})
		return pairs
	}
	return value
}

// a message of sixteen fields or more takes a map 16's header, as a canonical
// encoder writes it
func TestAMapOfSixteenFieldsTakesAMap16(t *testing.T) {
	m := wire.BeginMap([]byte{0xaa})
	for key := range uint64(17) {
		m.Uint(key, key)
	}
	written := m.End()
	if written[0] != 0xaa || written[1] != 0xde || written[2] != 0 || written[3] != 17 {
		t.Fatalf("a map of 17 fields begins %x", written[:4])
	}
	d := wire.NewDecoder(written[1:])
	count := uint64(0)
	for key := range d.Fields() {
		if value := d.Uint(); value != key {
			t.Fatalf("field %d holds %d", key, value)
		}
		count++
	}
	if d.End() != nil || count != 17 {
		t.Fatalf("%d fields read, %v", count, d.Err())
	}
}
