package blobs

import (
	"database/sql"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"strings"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/internal/sqlite"
)

// Object is an object as the file knows it.
type Object struct {
	// Key is the object's path under the handle that returned it.
	Key  string
	Size int64
	// ETag is quoted, as an HTTP header carries it, and derives from the
	// bytes, so two objects with the same bytes have one ETag. It is compared
	// with ETags and nothing else.
	ETag        string
	ContentType string
	// Modified is the store's clock at the commit of this version.
	Modified time.Time
	// Expires is zero for an object that does not expire.
	Expires time.Time
	Meta    map[string]string
}

// Query asks Scan for a page of every key under a handle's folder, its
// sub-folders' included, in the byte order of their paths.
type Query struct {
	// Prefix keeps only the keys that start with it.
	Prefix string
	// After keeps only the keys past it, when it is not empty.
	After string
	Limit int // objects a page returns: 100 when zero, at most 1000
}

// Page is one page of a Scan, from one snapshot. More says the limit ended it
// before the folder did, and Next is the query that reads on.
type Page struct {
	Objects []Object
	More    bool
	Next    Query
}

// Usage is the objects under a folder and their bytes, counted from their
// rows: a Copy counts its bytes although none was copied.
type Usage struct {
	Objects int64
	Bytes   int64
}

// KeyError is a call refused because of one object: its bucket and its whole
// path, owners included. errors.Is finds the store's sentinel in it.
type KeyError struct {
	Bucket, Path string
	Err          error
}

func (e *KeyError) Error() string {
	return fmt.Sprintf("blobs: bucket %q, key %q: %v", e.Bucket, e.Path, e.Err)
}

func (e *KeyError) Unwrap() error { return e.Err }

// ErrOutcomeUnknown is a write whose group's commit failed: the object may or
// may not be there, and its caller should Stat it before writing again.
var ErrOutcomeUnknown = sqlite.ErrOutcomeUnknown

// the bytes of a content's SHA-256 that its objects keep and their ETag spells
const etagBytes = 16

// etagOf spells an ETag from the bytes an object keeps of its content's hash,
// quoted as a header carries it:
//
//	9f 86 d0 81 … c5 5a d0 15 → "9f86d081884c7d659a2feaa0c55ad015"
func etagOf(kept []byte) string {
	return `"` + hex.EncodeToString(kept) + `"`
}

// matches says whether an If-Match list names an ETag: * names any live
// object, and a weak ETag names none, since If-Match compares strongly
//
//	`"9f86…"`        → the object whose ETag it is
//	`"a1", "9f86…"`  → either
//	`*`              → any live object
//	`W/"9f86…"`      → none
func matches(list, etag string) bool {
	for candidate := range strings.SplitSeq(list, ",") {
		candidate = strings.TrimSpace(candidate)
		if candidate == "*" || candidate == etag {
			return true
		}
	}
	return false
}

// metaText is meta as a row keeps it: a JSON object of strings, null for none
func metaText(meta map[string]string) (sql.NullString, error) {
	if len(meta) == 0 {
		return sql.NullString{}, nil
	}
	text, err := json.Marshal(meta)
	return sql.NullString{String: string(text), Valid: err == nil}, err
}

func metaOf(text sql.NullString) (map[string]string, error) {
	if !text.Valid {
		return nil, nil
	}
	var meta map[string]string
	if err := json.Unmarshal([]byte(text.String), &meta); err != nil {
		return nil, fmt.Errorf("%w: meta that no longer reads: %w", tinystore.ErrCorrupt, err)
	}
	return meta, nil
}

type objectRow struct {
	path        string
	size        int64
	etag        []byte
	contentType string
	modified    int64
	expires     sql.NullInt64
	meta        sql.NullString
}

// fields are where a read scans an object's columns, in the order the
// statements that read objects name them
func (r *objectRow) fields() []any {
	return []any{&r.path, &r.size, &r.etag, &r.contentType, &r.modified, &r.expires, &r.meta}
}

func (r objectRow) object(folder string) (Object, error) {
	meta, err := metaOf(r.meta)
	if err == nil && len(r.etag) != etagBytes {
		err = fmt.Errorf("%w: an ETag of %d bytes", tinystore.ErrCorrupt, len(r.etag))
	}
	object := Object{
		Key: strings.TrimPrefix(r.path, folder), Size: r.size, ETag: etagOf(r.etag),
		ContentType: r.contentType, Modified: time.UnixMilli(r.modified), Meta: meta,
	}
	if r.expires.Valid {
		object.Expires = time.UnixMilli(r.expires.Int64)
	}
	return object, err
}
