package kv

import (
	"bytes"
	"encoding/binary"
	"encoding/json"
	"fmt"
	"math"
	"reflect"

	"github.com/tinyshed/tinystore"
)

// the largest value kept in its row: past it a row costs more than a spilled
// value and slows the lookups beside it, docs/reports/kv-mechanics-2026-09-26.md
const inlineLimit = 512

// the largest value: larger bytes are the blobs engine's
const maxValue = 1 << 20

// Codec writes a bucket's values when the bytes their type would get are not
// the ones wanted; see WithCodec.
type Codec[V any] interface {
	Encode(V) ([]byte, error)
	Decode([]byte) (V, error)
}

// codec is how a bucket keeps its values in a row: encode gives a []byte, an
// int64 or nil, and decode takes back what the row holds
type codec[V any] struct {
	encode func(V) (any, error)
	decode func(any) (V, error)
}

// codecFor chooses a value's representation by its type, named types by what
// they are named for:
//
//	[]byte, string                    their bytes
//	bool, int8…int64, uint8…uint32    an integer of the row
//	uint, uint64                      eight bytes, big-endian
//	float32, float64                  their bits, big-endian
//	struct{}                          nothing: a bucket of them is a set
//	anything else                     JSON
func codecFor[V any]() codec[V] {
	t := reflect.TypeFor[V]()
	switch kind := t.Kind(); {
	case kind == reflect.String, kind == reflect.Slice && t.Elem().Kind() == reflect.Uint8:
		return bytesCodec[V](t)
	case kind == reflect.Bool, isSigned(kind), isNarrowUnsigned(kind):
		return integerCodec[V](t)
	case kind == reflect.Uint, kind == reflect.Uint64, kind == reflect.Uintptr:
		return wideUnsignedCodec[V](t)
	case kind == reflect.Float32, kind == reflect.Float64:
		return floatCodec[V](t)
	case kind == reflect.Struct && t.Size() == 0:
		return nothingCodec[V]()
	}
	return jsonCodec[V]()
}

func isSigned(kind reflect.Kind) bool {
	return kind >= reflect.Int && kind <= reflect.Int64
}

func isNarrowUnsigned(kind reflect.Kind) bool {
	return kind >= reflect.Uint8 && kind <= reflect.Uint32
}

var int64Type = reflect.TypeFor[int64]()

func bytesCodec[V any](t reflect.Type) codec[V] {
	return codec[V]{
		encode: func(value V) (any, error) {
			of := reflect.ValueOf(value)
			if of.Kind() == reflect.String {
				return []byte(of.String()), nil
			}
			return append([]byte{}, of.Bytes()...), nil
		},
		decode: func(stored any) (V, error) {
			raw, ok := stored.([]byte)
			if !ok && stored != nil {
				return zeroAnd[V](corrupt("bytes", stored))
			}
			return as[V](reflect.ValueOf(raw).Convert(t))
		},
	}
}

func integerCodec[V any](t reflect.Type) codec[V] {
	return codec[V]{
		encode: func(value V) (any, error) {
			of := reflect.ValueOf(value)
			switch {
			case of.Kind() != reflect.Bool:
				return of.Convert(int64Type).Int(), nil
			case of.Bool():
				return int64(1), nil
			}
			return int64(0), nil
		},
		decode: func(stored any) (V, error) {
			n, ok := stored.(int64)
			switch {
			case !ok:
				return zeroAnd[V](corrupt("an integer", stored))
			case t.Kind() == reflect.Bool:
				return as[V](reflect.ValueOf(n != 0).Convert(t))
			case outOf(t, n):
				return zeroAnd[V](fmt.Errorf("%w: %d does not fit a %s", tinystore.ErrCorrupt, n, t))
			}
			return as[V](reflect.ValueOf(n).Convert(t))
		},
	}
}

// outOf says that n does not fit an integer of type t
func outOf(t reflect.Type, n int64) bool {
	if isSigned(t.Kind()) {
		return reflect.New(t).Elem().OverflowInt(n)
	}
	return n < 0 || reflect.New(t).Elem().OverflowUint(uint64(n))
}

func wideUnsignedCodec[V any](t reflect.Type) codec[V] {
	return codec[V]{
		encode: func(value V) (any, error) {
			return binary.BigEndian.AppendUint64(nil, reflect.ValueOf(value).Uint()), nil
		},
		decode: func(stored any) (V, error) {
			raw, ok := stored.([]byte)
			if !ok || len(raw) != 8 {
				return zeroAnd[V](corrupt("eight bytes", stored))
			}
			n := binary.BigEndian.Uint64(raw)
			if reflect.New(t).Elem().OverflowUint(n) {
				return zeroAnd[V](fmt.Errorf("%w: %d does not fit a %s", tinystore.ErrCorrupt, n, t))
			}
			return as[V](reflect.ValueOf(n).Convert(t))
		},
	}
}

// floatCodec keeps a float's bits; a named float32 type passes through
// float64 on its way, which quiets a signaling NaN and changes nothing else
func floatCodec[V any](t reflect.Type) codec[V] {
	return codec[V]{
		encode: func(value V) (any, error) {
			if f, ok := any(value).(float32); ok {
				return binary.BigEndian.AppendUint32(nil, math.Float32bits(f)), nil
			}
			of := reflect.ValueOf(value)
			if of.Kind() == reflect.Float32 {
				return binary.BigEndian.AppendUint32(nil, math.Float32bits(float32(of.Float()))), nil
			}
			return binary.BigEndian.AppendUint64(nil, math.Float64bits(of.Float())), nil
		},
		decode: func(stored any) (V, error) {
			raw, ok := stored.([]byte)
			switch {
			case ok && len(raw) == 4 && t.Kind() == reflect.Float32:
				f := math.Float32frombits(binary.BigEndian.Uint32(raw))
				if value, same := any(f).(V); same {
					return value, nil
				}
				return as[V](reflect.ValueOf(f).Convert(t))
			case ok && len(raw) == 8 && t.Kind() == reflect.Float64:
				return as[V](reflect.ValueOf(math.Float64frombits(binary.BigEndian.Uint64(raw))).Convert(t))
			}
			return zeroAnd[V](corrupt("a float's bits", stored))
		},
	}
}

func nothingCodec[V any]() codec[V] {
	return codec[V]{
		encode: func(V) (any, error) { return nil, nil },
		decode: func(stored any) (V, error) {
			if stored != nil {
				return zeroAnd[V](corrupt("nothing", stored))
			}
			var zero V
			return zero, nil
		},
	}
}

func jsonCodec[V any]() codec[V] {
	return codec[V]{
		encode: func(value V) (any, error) {
			encoded, err := json.Marshal(value)
			if err != nil {
				return nil, fmt.Errorf("%w: a value JSON cannot write: %w", tinystore.ErrInvalid, err)
			}
			return encoded, nil
		},
		decode: func(stored any) (V, error) {
			var value V
			raw, ok := stored.([]byte)
			if !ok {
				return zeroAnd[V](corrupt("JSON", stored))
			}
			if err := json.Unmarshal(raw, &value); err != nil {
				return zeroAnd[V](fmt.Errorf("%w: a stored value is not JSON any more: %w", tinystore.ErrCorrupt, err))
			}
			return value, nil
		},
	}
}

// customCodec keeps a value as the bytes an application's codec gives it
func customCodec[V any](custom Codec[V]) codec[V] {
	return codec[V]{
		encode: func(value V) (any, error) {
			encoded, err := custom.Encode(value)
			if err != nil {
				return nil, fmt.Errorf("%w: the bucket's codec: %w", tinystore.ErrInvalid, err)
			}
			return bytes.Clone(encoded), nil
		},
		decode: func(stored any) (V, error) {
			raw, ok := stored.([]byte)
			if !ok && stored != nil {
				return zeroAnd[V](corrupt("bytes", stored))
			}
			value, err := custom.Decode(raw)
			if err != nil {
				return zeroAnd[V](fmt.Errorf("%w: the bucket's codec: %w", tinystore.ErrCorrupt, err))
			}
			return value, nil
		},
	}
}

// as is a value converted to V's own type, as a V
func as[V any](value reflect.Value) (V, error) {
	converted, ok := value.Interface().(V)
	if !ok {
		return converted, fmt.Errorf("%w: a %s is not a %T", tinystore.ErrCorrupt, value.Type(), converted)
	}
	return converted, nil
}

func corrupt(want string, stored any) error {
	return fmt.Errorf("%w: a stored value is %T where %s was written", tinystore.ErrCorrupt, stored, want)
}

func zeroAnd[V any](err error) (V, error) {
	var zero V
	return zero, err
}

// stored is a value as its row keeps it: inline, or spilled to a row of its
// own when it is bytes over inlineLimit
type stored struct {
	inline any
	spill  []byte
}

// keep decides where an encoded value goes, and refuses one over 1 MiB
func keep(value any) (stored, error) {
	raw, isBytes := value.([]byte)
	switch {
	case isBytes && len(raw) > maxValue:
		return stored{}, fmt.Errorf("%w: a value of %d bytes, over 1 MiB: keep it in blobs and its key here",
			tinystore.ErrLimit, len(raw))
	case isBytes && len(raw) > inlineLimit:
		return stored{spill: raw}, nil
	}
	return stored{inline: value}, nil
}

// size is what a value weighs against a group of writes and the store's memory
func (s stored) size() int {
	if raw, ok := s.inline.([]byte); ok {
		return len(raw) + len(s.spill)
	}
	return len(s.spill) + 8
}
