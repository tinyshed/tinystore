package kv

import (
	"fmt"
	"strconv"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/internal/sqlite"
)

// Version is the revision of kv.db that wrote a value. It never repeats in a
// file, not after a delete, an expiry or a reopen, so an old IfVersion cannot
// pass against a key deleted and written again. Two versions are equal or not;
// a version marshals to text, so that it can travel to a page and back.
type Version struct {
	revision int64
}

func (v Version) String() string {
	if v.revision <= 0 {
		return ""
	}
	return strconv.FormatInt(v.revision, 36)
}

func (v Version) MarshalText() ([]byte, error) {
	return []byte(v.String()), nil
}

func (v *Version) UnmarshalText(text []byte) error {
	if len(text) == 0 {
		*v = Version{}
		return nil
	}
	revision, err := strconv.ParseInt(string(text), 36, 64)
	if err != nil || revision <= 0 {
		return fmt.Errorf("%w: kv: a version reads %q", tinystore.ErrInvalid, text)
	}
	v.revision = revision
	return nil
}

// Entry is a value with what the file knows of it. ExpiresAt is zero for a
// key that never expires; Key is the key's text, without the branch's owners.
type Entry[V any] struct {
	Key       string
	Value     V
	Version   Version
	ExpiresAt time.Time
}

// Query asks Scan for a page of a branch's own keys, in the byte order of
// their text, after the key After when it is not empty.
type Query struct {
	After string
	Limit int // keys a page returns: 100 when zero, at most 1000
}

// Page is one page of a Scan, from one snapshot. More says the limit, or the
// page's 4 MiB of values, ended it before the branch did, and Next is the
// query that reads on.
type Page[V any] struct {
	Entries []Entry[V]
	More    bool
	Next    Query
}

// KeyError is a call refused because of one key: its bucket and the path of
// owners and key it named. errors.Is finds the store's sentinel in it.
type KeyError struct {
	Bucket, Path string
	Err          error
}

func (e *KeyError) Error() string {
	return fmt.Sprintf("kv: bucket %q, key %q: %v", e.Bucket, e.Path, e.Err)
}

func (e *KeyError) Unwrap() error { return e.Err }

// ErrOutcomeUnknown is a write whose group's commit failed: it may or may not
// be in the file, and its caller reads it back before writing again.
var ErrOutcomeUnknown = sqlite.ErrOutcomeUnknown

func expiryTime(expires int64, valid bool) time.Time {
	if !valid {
		return time.Time{}
	}
	return time.UnixMilli(expires)
}
