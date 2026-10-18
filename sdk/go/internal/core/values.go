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
// wrote it: 1 text, 2 bytes, 3 JSON, 4 a float's eight bytes. A row read as
// another kind than wrote it is corrupt, never a plausible wrong value.

// The first byte of a row of bytes: the kind of value that wrote it.
const (
	kindText  byte = 1
	kindBytes byte = 2
	kindJSON  byte = 3
	kindFloat byte = 4
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
	asBool
	asFloat
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
	case t.Kind() >= reflect.Uint && t.Kind() <= reflect.Uint64:
		return Values[T]{asUint}
	case t.Kind() == reflect.Float32 || t.Kind() == reflect.Float64:
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
		if r.Uint() > math.MaxInt64 {
			return nil, Errorf(Invalid, "%d is past what an int64 holds", r.Uint())
		}
		return int64(r.Uint()), nil
	case asFloat:
		var bytes [8]byte
		binary.BigEndian.PutUint64(bytes[:], math.Float64bits(r.Float()))
		return tagged(kindFloat, bytes[:]), nil
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
		switch v.kind {
		case asBool:
			r.SetBool(n != 0)
		case asInt:
			r.SetInt(n)
		default:
			r.SetUint(uint64(n))
		}
	case asFloat:
		bits, err := bodyOf(raw, kindFloat, "a float")
		if err != nil {
			return value, err
		}
		if len(bits) != 8 {
			return value, Errorf(Corrupt, "a float from %d bytes rather than 8", len(bits))
		}
		r.SetFloat(math.Float64frombits(binary.BigEndian.Uint64(bits)))
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
