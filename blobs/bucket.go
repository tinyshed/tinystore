package blobs

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/internal/sqlite"
)

// Bucket is a handle on the objects of one bucket, or of the folder of it
// that Of names; it may be used from any number of goroutines.
type Bucket struct {
	store      *Store
	id         int64
	name       string
	folder     string // the folder's path, ending in /, or empty for the bucket's root
	depth      int    // the folder's segments
	defaultTTL time.Duration
	maxSize    int64
	err        error // an owner Of refused, which every call of the handle returns
}

// OpenBucket opens the bucket name of blobs.db, creating it the first time.
// A bucket holds bytes of any kind, so it has no type; its options are the
// program's, and may change between runs.
func OpenBucket(ctx context.Context, objects *Store, name string, options ...BucketOption) (*Bucket, error) {
	settings, err := collectBucket(options)
	if err != nil {
		return nil, fmt.Errorf("blobs: bucket %q: %w", name, err)
	}

	id, err := objects.claimBucket(ctx, name)
	if err != nil {
		return nil, err
	}
	return &Bucket{store: objects, id: id, name: name, defaultTTL: settings.ttl, maxSize: settings.maxSize}, nil
}

const (
	selectBucket = `select id from buckets where name = ?1`
	insertBucket = `insert into buckets (name) values (?1) returning id`
)

// claimBucket finds a bucket by name or creates it
func (s *Store) claimBucket(ctx context.Context, name string) (int64, error) {
	if !validName.MatchString(name) {
		return 0, fmt.Errorf("%w: blobs: a bucket name is [a-z0-9][a-z0-9_-]{0,63}, not %q", tinystore.ErrInvalid, name)
	}
	release, err := s.admit(ctx)
	if err != nil {
		return 0, err
	}
	defer release()

	var id int64
	err = s.file.UpdatePrepared(ctx, func(w sqlite.Writer) error {
		scanErr := sqlite.QueryRow(ctx, w, selectBucket, name).Scan(&id)
		if errors.Is(scanErr, sql.ErrNoRows) {
			return sqlite.QueryRow(ctx, w, insertBucket, name).Scan(&id)
		}
		return scanErr
	})
	if err != nil {
		return 0, fmt.Errorf("blobs: open bucket %q: %w", name, err)
	}
	return id, nil
}

// Of is the folder of this handle that owners name, a segment each: a string
// or an integer, an integer by its decimal text, so that Of("users", 42) is
// the folder users/42/, which never meets users/4/. An owner holding / is
// tinystore.ErrInvalid, returned by every call of the handle. A folder exists
// while it holds objects; nothing creates or removes one.
func (b *Bucket) Of(owners ...any) *Bucket {
	below := *b
	for _, owner := range owners {
		text, err := ownerText(owner)
		if err == nil && (below.depth+1 > maxSegments || len(below.folder)+len(text)+1 > maxPath) {
			err = fmt.Errorf("%w: a folder past %d segments or 1 KiB", tinystore.ErrInvalid, maxSegments)
		}
		if err != nil {
			if below.err == nil {
				below.err = &KeyError{Bucket: b.name, Path: below.folder + fmt.Sprint(owner), Err: err}
			}
			continue
		}
		below.folder += text + "/"
		below.depth++
	}
	return &below
}

// call is one operation on one key, its facts gathered before it waits for
// anything: the whole path, the key under the handle and the options
type call struct {
	path     string
	key      string
	settings callSettings
}

// begin checks a call's key and its options, refusing one the call does not take
func (b *Bucket) begin(key, name string, options []Option, takes map[string]bool) (call, error) {
	c := call{path: b.folder + key, key: key, settings: callSettings{size: -1}}
	if b.err != nil {
		return c, b.err
	}
	path, err := pathOf(b.folder, b.depth, key)
	if err != nil {
		return c, err
	}
	c.path = path
	c.settings, err = collect(options, name, takes)
	return c, err
}

// fail names the bucket and path a call failed on; a cancellation, a closed
// store and a KeyError already made pass as they are
func (b *Bucket) fail(c call, err error) error {
	if err == nil {
		return nil
	}
	var named *KeyError
	switch {
	case errors.Is(err, context.Canceled), errors.Is(err, context.DeadlineExceeded),
		errors.Is(err, tinystore.ErrClosed), errors.As(err, &named):
		return err
	}
	return &KeyError{Bucket: b.name, Path: c.path, Err: err}
}

// hidden is true of an object a marked Clear hid: its revision is the mark's
// or older, and the mark's folder holds it. The first test does not depend on
// the row, so SQLite runs it once a statement, and a bucket without marks
// pays one seek; ?1 is the statement's bucket and o the object's row.
const hidden = `(exists (select 1 from cleared where bucket = ?1) and exists (
		select 1 from cleared as m where m.bucket = o.bucket and m.revision >= o.revision
			and substr(o.path, 1, length(m.prefix)) = m.prefix))`
