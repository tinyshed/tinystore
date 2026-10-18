package wire

import (
	"math"
	"slices"
	"strconv"
	"unicode/utf8"
)

// Message is what protocol.go writes for each message of the schema: a
// reader and a writer written out a field at a time.
type Message interface {
	Write(w *Writer)
	Read(r *Reader)
}

// Encode is a message's bytes.
func Encode(m Message) []byte {
	var w Writer
	m.Write(&w)
	return w.Bytes()
}

// Decode reads a body that is the message and nothing after it.
func Decode(body []byte, m Message) error {
	r := NewReader(body)
	m.Read(r)
	return r.End()
}

// Raw is a kv value as its row keeps it: nil, an int64 or a []byte.
type Raw = any

// Cell is a value as SQLite keeps it: nil, an int64, a float64, a string or a
// []byte; a bool a client sends is the integer SQLite keeps for it.
type Cell = any

// Key writes a kv key's text: a str, or a bin where its bytes are not UTF-8.
func (w *Writer) Key(key string) {
	if utf8.ValidString(key) {
		w.Str(key)
	} else {
		w.Bin([]byte(key))
	}
}

// Key reads a kv key's text: a str, a bin's bytes, or an integer's decimal
// spelling.
func (r *Reader) Key() string {
	switch r.Type() {
	case TypeBin:
		return string(r.Bin())
	case TypeInteger:
		v, negative := r.integer("a key")
		if negative {
			return strconv.FormatInt(int64(v), 10)
		}
		return strconv.FormatUint(v, 10)
	}
	return r.Str()
}

// Raw writes a kv value: nil, an integer or bytes.
func (w *Writer) Raw(v Raw) {
	switch v := v.(type) {
	case nil:
		w.Nil()
	case int64:
		w.Int(v)
	case []byte:
		w.Bin(v)
	default:
		panic("wire: a kv value is nil, an int64 or a []byte")
	}
}

// Raw reads a kv value: nil, an int64 or a []byte.
func (r *Reader) Raw() Raw {
	switch r.Type() {
	case TypeNil:
		r.Nil()
		return nil
	case TypeBin:
		return r.Bin()
	}
	return r.Int()
}

// RawIsZero says whether a kv value writes as the profile's zero: nil, the
// integer 0 or no bytes, which a field leaves out.
func RawIsZero(v Raw) bool {
	switch v := v.(type) {
	case nil:
		return true
	case int64:
		return v == 0
	case []byte:
		return len(v) == 0
	}
	return false
}

// Cell writes a value as SQLite keeps it, a bool as the bool it is.
func (w *Writer) Cell(v Cell) {
	switch v := v.(type) {
	case nil:
		w.Nil()
	case bool:
		w.Bool(v)
	case int64:
		w.Int(v)
	case float64:
		w.Float(v)
	case string:
		w.Str(v)
	case []byte:
		w.Bin(v)
	default:
		panic("wire: a cell is nil, a bool, an int64, a float64, a string or a []byte")
	}
}

// Cell reads a value as SQLite keeps it.
func (r *Reader) Cell() Cell {
	switch r.Type() {
	case TypeNil:
		r.Nil()
		return nil
	case TypeBool:
		return r.Bool()
	case TypeFloat:
		return r.Float()
	case TypeStr:
		return r.Str()
	case TypeBin:
		return r.Bin()
	}
	return r.Int()
}

// CellIsZero says whether a cell writes as the profile's zero: nil, false,
// the integer 0, a float whose bits are all 0, no text or no bytes.
func CellIsZero(v Cell) bool {
	switch v := v.(type) {
	case nil:
		return true
	case bool:
		return !v
	case int64:
		return v == 0
	case float64:
		return math.Float64bits(v) == 0
	case string:
		return v == ""
	case []byte:
		return len(v) == 0
	}
	return false
}

// SortedNames is a map of names' keys in the byte order of their UTF-8,
// which a map of names is written in.
func SortedNames[T any](named map[string]T) []string {
	keys := make([]string, 0, len(named))
	for key := range named {
		keys = append(keys, key)
	}
	slices.Sort(keys)
	return keys
}
