package blobs

import (
	"fmt"
	"reflect"
	"strconv"
	"strings"
	"unicode"
	"unicode/utf8"

	"github.com/tinyshed/tinystore"
)

// ownerText is an owner's segment: a string as it is, an integer as its
// decimal spelling, as kv spells its owners:
//
//	"42" → 42    int64(42) → 42    UserID(7) → 7    "a/b" → refused
func ownerText(owner any) (string, error) {
	var text string
	switch owner := owner.(type) {
	case string:
		text = owner
	case int:
		text = strconv.Itoa(owner)
	case int64:
		text = strconv.FormatInt(owner, 10)
	default:
		var ok bool
		if text, ok = reflectedText(owner); !ok {
			return "", fmt.Errorf("%w: an owner is a string or an integer, not %T", tinystore.ErrInvalid, owner)
		}
	}
	if strings.Contains(text, "/") {
		return "", fmt.Errorf("%w: an owner holding /: %q", tinystore.ErrInvalid, text)
	}
	return text, checkSegment(text)
}

// reflectedText spells the named strings and the integers ownerText does not name
func reflectedText(owner any) (string, bool) {
	value := reflect.ValueOf(owner)
	switch value.Kind() {
	case reflect.String:
		return value.String(), true
	case reflect.Int, reflect.Int8, reflect.Int16, reflect.Int32, reflect.Int64:
		return strconv.FormatInt(value.Int(), 10), true
	case reflect.Uint, reflect.Uint8, reflect.Uint16, reflect.Uint32, reflect.Uint64, reflect.Uintptr:
		return strconv.FormatUint(value.Uint(), 10), true
	}
	return "", false
}

// checkSegment refuses what a path's segment cannot be: empty, . or .., not
// UTF-8, or holding a control character
func checkSegment(segment string) error {
	switch {
	case segment == "":
		return fmt.Errorf("%w: an empty segment", tinystore.ErrInvalid)
	case segment == "." || segment == "..":
		return fmt.Errorf("%w: a segment %q", tinystore.ErrInvalid, segment)
	case !utf8.ValidString(segment):
		return fmt.Errorf("%w: a segment that is not UTF-8", tinystore.ErrInvalid)
	case strings.IndexFunc(segment, unicode.IsControl) >= 0:
		return fmt.Errorf("%w: a segment holding a control character", tinystore.ErrInvalid)
	}
	return nil
}

// pathOf is a key's whole path under a folder of depth segments, refused when
// it is not a path:
//
//	"users/42/", "photos/1.jpg"  → "users/42/photos/1.jpg"
//	"users/42/", "photos//1.jpg" → an empty segment
func pathOf(folder string, depth int, key string) (string, error) {
	segments := depth
	for segment := range strings.SplitSeq(key, "/") {
		if err := checkSegment(segment); err != nil {
			return "", err
		}
		segments++
	}
	path := folder + key
	switch {
	case segments > maxSegments:
		return "", fmt.Errorf("%w: a path of %d segments, over %d", tinystore.ErrInvalid, segments, maxSegments)
	case len(path) > maxPath:
		return "", fmt.Errorf("%w: a path of %d bytes, over 1 KiB", tinystore.ErrInvalid, len(path))
	}
	return path, nil
}

// checkPrefix refuses a Query's Prefix or After that no path could start with
func checkPrefix(prefix string) error {
	switch {
	case len(prefix) > maxPath:
		return fmt.Errorf("%w: a query of %d bytes, over 1 KiB", tinystore.ErrInvalid, len(prefix))
	case !utf8.ValidString(prefix) || strings.IndexFunc(prefix, unicode.IsControl) >= 0:
		return fmt.Errorf("%w: a query that is not UTF-8 or holds a control character", tinystore.ErrInvalid)
	}
	return nil
}

// prefixEnd is the first text past every text that starts with prefix: the
// prefix with its last byte raised by one, which cannot overflow because no
// path's last byte is past 0xf4.
//
//	"users/4/" → "users/40", so "users/42/…" lies past it
//	""         → "\xff", past every path
func prefixEnd(prefix string) string {
	if prefix == "" {
		return "\xff"
	}
	last := len(prefix) - 1
	return prefix[:last] + string([]byte{prefix[last] + 1})
}
