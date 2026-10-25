package core

import (
	"encoding/binary"
	"encoding/json"
	"math"
	"reflect"
	"strconv"
	"unicode/utf8"

	"tinyshed.org/tinystore/internal/wire"
)

// A bucket keeps a value as the core keeps a Rust type, so that every
// language reads the others' rows: integers and booleans as integers, a set's
// keys with no value, and a row of bytes that says by its first byte what
// wrote it: 1 text, 2 bytes, 3 JSON, 4 a float64's eight bytes, 5 a uint64's
// eight, as Rust's u64 and usize, 6 a float32's four, as Rust's f32. A row
// read as another kind than wrote it is corrupt, never a plausible wrong value.
//
// Before 0.1.1 a uint64 was an integer row and a float32 a float64's: such a
// row still reads when it holds the value exactly, so that a bucket an older
// release wrote keeps reading.

// The first byte of a row of bytes: the kind of value that wrote it.
const (
	kindText  byte = 1
	kindBytes byte = 2
	kindJSON  byte = 3
	kindFloat byte = 4
	kindU64   byte = 5
	kindF32   byte = 6
)

var kindNames = map[byte]string{1: "text", 2: "bytes", 3: "JSON", 4: "a float", 5: "a u64", 6: "an f32"}

func tagged(kind byte, body []byte) []byte {
	return append([]byte{kind}, body...)
}

// bodyOf is the body of a row that kind wrote; a row another kind wrote is
// corrupt here, never read as this one.
func bodyOf(raw wire.Raw, kind byte, wanted string) ([]byte, error) {
	bytes, ok := raw.([]byte)
	if !ok {
		return nil, Errorf(Corrupt, "a stored value is %s where %s was written", described(raw), wanted)
	}
	if len(bytes) == 0 {
		return nil, Errorf(Corrupt, "%s from a row of no bytes", wanted)
	}
	if bytes[0] != kind {
		written, known := kindNames[bytes[0]]
		if !known {
			written = "bytes of no kind"
		}
		return nil, Errorf(Corrupt, "%s from a row of %s", wanted, written)
	}
	return bytes[1:], nil
}

type valueKind int

const (
	asJSON valueKind = iota
	asText
	asBytes
	asInt
	asUint
	asU64
	asBool
	asFloat
	asF32
	asNothing
)

// Values turns a handle's values of type T into what a row keeps, and back.
type Values[T any] struct {
	kind valueKind
}

// ValuesOf is how a handle of type T keeps its values.
func ValuesOf[T any]() Values[T] {
	t := reflect.TypeFor[T]()
	switch {
	case t.Kind() == reflect.String:
		return Values[T]{asText}
	case t.Kind() == reflect.Slice && t.Elem().Kind() == reflect.Uint8:
		return Values[T]{asBytes}
	case t.Kind() == reflect.Bool:
		return Values[T]{asBool}
	case t.Kind() >= reflect.Int && t.Kind() <= reflect.Int64:
		return Values[T]{asInt}
	case t.Kind() == reflect.Uint || t.Kind() == reflect.Uint64:
		return Values[T]{asU64}
	case t.Kind() >= reflect.Uint8 && t.Kind() <= reflect.Uint32:
		return Values[T]{asUint}
	case t.Kind() == reflect.Float32:
		return Values[T]{asF32}
	case t.Kind() == reflect.Float64:
		return Values[T]{asFloat}
	case t.Kind() == reflect.Struct && t.NumField() == 0:
		return Values[T]{asNothing}
	}
	return Values[T]{asJSON}
}

// JSONValues keeps every value as JSON, whatever its type: a job's value, a kept answer.
func JSONValues[T any]() Values[T] { return Values[T]{asJSON} }

// IsSet says whether the handle keeps keys alone, a value of struct{}.
func (v Values[T]) IsSet() bool { return v.kind == asNothing }

// Encode is a value as a row keeps it.
func (v Values[T]) Encode(value T) (wire.Raw, error) {
	r := reflect.ValueOf(&value).Elem()
	switch v.kind {
	case asText:
		return tagged(kindText, []byte(r.String())), nil
	case asBytes:
		return tagged(kindBytes, r.Bytes()), nil
	case asBool:
		if r.Bool() {
			return int64(1), nil
		}
		return int64(0), nil
	case asInt:
		return r.Int(), nil
	case asUint:
		return int64(r.Uint()), nil
	case asU64:
		return tagged(kindU64, binary.BigEndian.AppendUint64(nil, r.Uint())), nil
	case asFloat:
		return tagged(kindFloat, binary.BigEndian.AppendUint64(nil, math.Float64bits(r.Float()))), nil
	case asF32:
		// converted float32 to float32, which keeps a signalling NaN's bits; r.Float() would quiet it
		f := r.Convert(reflect.TypeFor[float32]()).Interface().(float32)
		return tagged(kindF32, binary.BigEndian.AppendUint32(nil, math.Float32bits(f))), nil
	case asNothing:
		return nil, nil
	}
	text, err := json.Marshal(value)
	if err != nil {
		return nil, Wrap(Invalid, err, "a value JSON cannot write")
	}
	return tagged(kindJSON, text), nil
}

// Decode is a value a row kept, as type T.
func (v Values[T]) Decode(raw wire.Raw) (T, error) {
	var value T
	r := reflect.ValueOf(&value).Elem()
	switch v.kind {
	case asText:
		if raw == nil {
			return value, nil
		}
		text, err := bodyOf(raw, kindText, "text")
		if err != nil {
			return value, err
		}
		if !utf8.Valid(text) {
			return value, Errorf(Corrupt, "a stored value is text that is not UTF-8")
		}
		r.SetString(string(text))
	case asBytes:
		if raw == nil {
			return value, nil
		}
		bytes, err := bodyOf(raw, kindBytes, "bytes")
		if err != nil {
			return value, err
		}
		r.SetBytes(append([]byte{}, bytes...))
	case asBool, asInt, asUint:
		n, ok := raw.(int64)
		if !ok {
			return value, Errorf(Corrupt, "a stored value is %s where an integer was written", described(raw))
		}
		switch {
		case v.kind == asBool:
			r.SetBool(n != 0)
		case v.kind == asInt && !r.OverflowInt(n):
			r.SetInt(n)
		case v.kind == asUint && n >= 0 && !r.OverflowUint(uint64(n)):
			r.SetUint(uint64(n))
		default:
			return value, Errorf(Corrupt, "a stored value is the integer %d, which a %s does not hold", n, r.Type())
		}
	case asU64:
		n, err := u64Of(raw)
		if err != nil {
			return value, err
		}
		r.SetUint(n)
	case asFloat:
		bits, err := fixedOf(raw, kindFloat, "a float", 8)
		if err != nil {
			return value, err
		}
		r.SetFloat(math.Float64frombits(binary.BigEndian.Uint64(bits)))
	case asF32:
		f, err := f32Of(raw)
		if err != nil {
			return value, err
		}
		r.Set(reflect.ValueOf(f).Convert(r.Type()))
	case asNothing:
	default:
		if raw == nil {
			return value, nil
		}
		text, err := bodyOf(raw, kindJSON, "JSON")
		if err != nil {
			return value, err
		}
		if err := json.Unmarshal(text, &value); err != nil {
			return value, Wrap(Corrupt, err, "a stored value does not read as %s", reflect.TypeFor[T]())
		}
	}
	return value, nil
}

// fixedOf is the body of a row that kind wrote, of size bytes.
func fixedOf(raw wire.Raw, kind byte, wanted string, size int) ([]byte, error) {
	body, err := bodyOf(raw, kind, wanted)
	if err == nil && len(body) != size {
		err = Errorf(Corrupt, "%s from %d bytes rather than %d", wanted, len(body), size)
	}
	return body, err
}

// u64Of reads a uint64's row, or the integer an older release wrote for one.
func u64Of(raw wire.Raw) (uint64, error) {
	if n, ok := raw.(int64); ok {
		if n < 0 {
			return 0, Errorf(Corrupt, "a stored value is the integer %d, which a uint64 does not hold", n)
		}
		return uint64(n), nil
	}
	bits, err := fixedOf(raw, kindU64, "a u64", 8)
	if err != nil {
		return 0, err
	}
	return binary.BigEndian.Uint64(bits), nil
}

// f32Of reads a float32's row, or the float64 an older release wrote for one
// when it holds a float32 exactly.
func f32Of(raw wire.Raw) (float32, error) {
	if bytes, ok := raw.([]byte); ok && len(bytes) > 0 && bytes[0] == kindFloat {
		bits, err := fixedOf(raw, kindFloat, "an f32", 8)
		if err != nil {
			return 0, err
		}
		wide := binary.BigEndian.Uint64(bits)
		f := float32(math.Float64frombits(wide))
		if math.Float64bits(float64(f)) != wide {
			return 0, Errorf(Corrupt, "an f32 from a row of a float an f32 does not hold")
		}
		return f, nil
	}
	bits, err := fixedOf(raw, kindF32, "an f32", 4)
	if err != nil {
		return 0, err
	}
	return math.Float32frombits(binary.BigEndian.Uint32(bits)), nil
}

func described(raw wire.Raw) string {
	switch r := raw.(type) {
	case nil:
		return "nothing"
	case int64:
		return "an integer"
	case []byte:
		return strconv.Itoa(len(r)) + " bytes"
	}
	return "a value"
}

// ToJSON is a value's JSON text.
func ToJSON(value any) (string, error) {
	text, err := json.Marshal(value)
	if err != nil {
		return "", Wrap(Invalid, err, "a value JSON cannot write")
	}
	return string(text), nil
}

// FromJSON reads a value of type T from JSON text; one that no longer reads is corrupt.
func FromJSON[T any](text string) (T, error) {
	var value T
	if text == "" {
		return value, nil
	}
	if err := json.Unmarshal([]byte(text), &value); err != nil {
		return value, Wrap(Corrupt, err, "a stored value does not read as %s", reflect.TypeFor[T]())
	}
	return value, nil
}

// CalendarOf is an aggregate's calendar and its time zone as the wire
// carries them: absent when neither is given, so that the store refuses one
// given without the other.
func CalendarOf(unit, zone string) (*string, *string) {
	var calendar, timeZone *string
	if unit != "" {
		calendar = &unit
	}
	if zone != "" {
		timeZone = &zone
	}
	return calendar, timeZone
}
