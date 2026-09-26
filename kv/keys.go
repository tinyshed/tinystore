package kv

import (
	"fmt"
	"reflect"
	"strconv"
	"strings"

	"github.com/tinyshed/tinystore"
)

// the longest path a key may have, its owners included, as the file keeps it
const maxPath = 1 << 10

// the bytes a path writes around its names
const (
	ownerMark = 0x01
	keyMark   = 0x02
	nameEnd   = 0x00
	escaped   = 0xff
)

// keyText is a key's or an owner's text: a string or a []byte as it is, an
// integer as its decimal spelling, so that a handler holding an int64 and one
// holding the text of a URL name the same key:
//
//	"42" → 42    int64(42) → 42    []byte("42") → 42    uint8(7) → 7
func keyText(key any) (string, error) {
	var text string
	switch key := key.(type) {
	case string:
		text = key
	case []byte:
		text = string(key)
	case int:
		text = strconv.Itoa(key)
	case int64:
		text = strconv.FormatInt(key, 10)
	default:
		var ok bool
		if text, ok = reflectedText(key); !ok {
			return "", fmt.Errorf("%w: a key is a string, a []byte or an integer, not %T", tinystore.ErrInvalid, key)
		}
	}
	if text == "" {
		return "", fmt.Errorf("%w: an empty key", tinystore.ErrInvalid)
	}
	return text, nil
}

// reflectedText spells the named strings, byte slices and integers that
// keyText does not name
func reflectedText(key any) (string, bool) {
	value := reflect.ValueOf(key)
	switch value.Kind() {
	case reflect.String:
		return value.String(), true
	case reflect.Int, reflect.Int8, reflect.Int16, reflect.Int32, reflect.Int64:
		return strconv.FormatInt(value.Int(), 10), true
	case reflect.Uint, reflect.Uint8, reflect.Uint16, reflect.Uint32, reflect.Uint64, reflect.Uintptr:
		return strconv.FormatUint(value.Uint(), 10), true
	case reflect.Slice:
		if value.Type().Elem().Kind() == reflect.Uint8 {
			return string(value.Bytes()), true
		}
	}
	return "", false
}

// appendOwner writes an owner after its mark and before its end, and appendKey
// a key after a mark of its own, so that a branch is one range and a name may
// hold any byte:
//
//	Of("tenant-7", 42), "iPhone" → 01 tenant-7 00 · 01 42 00 · 02 iPhone
//	a 00 inside a name           → 00 FF
func appendOwner(path []byte, owner string) []byte {
	path = appendEscaped(append(path, ownerMark), owner)
	return append(path, nameEnd)
}

func appendKey(path []byte, key string) []byte {
	return appendEscaped(append(path, keyMark), key)
}

func appendEscaped(path []byte, name string) []byte {
	for i := range len(name) {
		path = append(path, name[i])
		if name[i] == nameEnd {
			path = append(path, escaped)
		}
	}
	return path
}

// keyOf reads a key back from its path, which starts with its branch's prefix
// of the given length
func keyOf(path []byte, prefix int) string {
	return strings.ReplaceAll(string(path[prefix+1:]), string([]byte{nameEnd, escaped}), string([]byte{nameEnd}))
}

// shownPath is owners and key as an error names them
func shownPath(owners []string, key string) string {
	return strings.Join(append(append([]string(nil), owners...), key), "/")
}
