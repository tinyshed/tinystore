package core

import (
	"encoding/binary"
	"encoding/json"
	"math"
	"reflect"
	"strconv"
	"unicode/utf8"

	"github.com/tinyshed/tinystore/internal/wire"
)

// A bucket keeps a value as the core keeps a Rust type, so that every
// language reads the others' rows: text and bytes as they are, integers and
// booleans as integers, floats bit for bit in eight bytes, a set's keys with
// no value, and JSON for the rest.

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
		return []byte(r.String()), nil
	case asBytes:
		return append([]byte{}, r.Bytes()...), nil
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
		return bytes[:], nil
	case asNothing:
		return nil, nil
	}
	text, err := json.Marshal(value)
	if err != nil {
		return nil, Wrap(Invalid, err, "a value JSON cannot write")
	}
	return text, nil
}

// Decode is a value a row kept, as type T.
func (v Values[T]) Decode(raw wire.Raw) (T, error) {
	var value T
	r := reflect.ValueOf(&value).Elem()
	switch v.kind {
	case asText:
		bytes, ok := raw.([]byte)
		if raw != nil && (!ok || !utf8.Valid(bytes)) {
			return value, Errorf(Corrupt, "a stored value is %s, which is no UTF-8 text", described(raw))
		}
		r.SetString(string(bytes))
	case asBytes:
		if _, ok := raw.(int64); ok {
			return value, Errorf(Corrupt, "a stored value is %s where bytes were written", described(raw))
		}
		bytes, _ := raw.([]byte)
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
		bytes, ok := raw.([]byte)
		if !ok || len(bytes) != 8 {
			return value, Errorf(Corrupt, "a stored value is %s where a float's eight bytes were written", described(raw))
		}
		r.SetFloat(math.Float64frombits(binary.BigEndian.Uint64(bytes)))
	case asNothing:
	default:
		if raw == nil {
			return value, nil
		}
		bytes, ok := raw.([]byte)
		if !ok {
			return value, Errorf(Corrupt, "a stored value is %s where JSON was written", described(raw))
		}
		if err := json.Unmarshal(bytes, &value); err != nil {
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
