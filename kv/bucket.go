package kv

import (
	"cmp"
	"context"
	"database/sql"
	"errors"
	"fmt"
	"reflect"
	"slices"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/internal/sqlite"
)

// the kind of bucket that holds values; a name keeps its kind for good, since
// its rows would mean something else as another
const kindValues = "values"

// Bucket holds values of type V by key. Of and WithTx give handles on the same
// bucket, and a handle may be used from any number of goroutines.
type Bucket[V any] struct {
	state  *Store
	id     int64
	name   string
	codec  codec[V]
	ttl    time.Duration
	owners []string
	prefix []byte
	tx     *Tx
	err    error
}

// OpenBucket opens the bucket name of kv.db for values of type V, creating it
// the first time. A name that holds counters is ErrInvalid.
func OpenBucket[V any](ctx context.Context, state *Store, name string, options ...BucketOption) (*Bucket[V], error) {
	settings, err := settle[V](options)
	if err != nil {
		return nil, fmt.Errorf("kv: bucket %q: %w", name, err)
	}

	id, err := state.claimBucket(ctx, name, kindValues)
	if err != nil {
		return nil, err
	}

	return &Bucket[V]{state: state, id: id, name: name, codec: settings.codec, ttl: settings.ttl}, nil
}

// settled is a bucket's options, its codec chosen
type settled[V any] struct {
	codec codec[V]
	ttl   time.Duration
}

func settle[V any](options []BucketOption) (settled[V], error) {
	var settings bucketSettings
	for _, option := range options {
		option(&settings)
	}
	if settings.err != nil {
		return settled[V]{}, settings.err
	}
	chosen := settled[V]{codec: codecFor[V](), ttl: settings.ttl}
	if settings.codec != nil {
		custom, ok := settings.codec.(Codec[V])
		if !ok {
			return settled[V]{}, fmt.Errorf("%w: a codec of %T for values of %s",
				tinystore.ErrInvalid, settings.codec, reflect.TypeFor[V]())
		}
		chosen.codec = customCodec(custom)
	}
	return chosen, nil
}

const (
	selectBucket = `select id, kind from buckets where name = ?1`
	insertBucket = `insert into buckets (name, kind) values (?1, ?2) returning id`
)

// claimBucket finds a bucket by name or creates it, and refuses one of
// another kind
func (s *Store) claimBucket(ctx context.Context, name, kind string) (int64, error) {
	if !validName.MatchString(name) {
		return 0, fmt.Errorf("%w: kv: a bucket name is [a-z0-9][a-z0-9_-]{0,63}, not %q", tinystore.ErrInvalid, name)
	}
	release, err := s.admit(ctx)
	if err != nil {
		return 0, err
	}
	defer release()

	var id int64
	err = s.file.UpdatePrepared(ctx, func(w sqlite.Writer) error {
		var found string
		scanErr := sqlite.QueryRow(ctx, w, selectBucket, name).Scan(&id, &found)
		switch {
		case errors.Is(scanErr, sql.ErrNoRows):
			return sqlite.QueryRow(ctx, w, insertBucket, name, kind).Scan(&id)
		case scanErr == nil && found != kind:
			return fmt.Errorf("%w: kv: bucket %q holds %s, not %s", tinystore.ErrInvalid, name, found, kind)
		}
		return scanErr
	})
	return id, err
}

// Of is the branch of this bucket that owners name, one after another: a
// string, a []byte or an integer each, an integer by its decimal text. A
// branch exists while it holds keys; there is nothing to create or drop.
func (b *Bucket[V]) Of(owners ...any) *Bucket[V] {
	branch := *b
	branch.owners = slices.Clone(b.owners)
	branch.prefix = slices.Clone(b.prefix)
	for _, owner := range owners {
		text, err := keyText(owner)
		if err != nil {
			refused := &KeyError{Bucket: b.name, Path: shownPath(branch.owners, fmt.Sprint(owner)), Err: err}
			branch.err = cmp.Or[error](branch.err, refused)
			continue
		}
		branch.owners = append(branch.owners, text)
		branch.prefix = appendOwner(branch.prefix, text)
	}
	return &branch
}

// WithTx is this bucket inside tx: its calls run in tx's transaction and see
// its writes, and after the function tx was given returns they are ErrClosed.
func (b *Bucket[V]) WithTx(tx *Tx) *Bucket[V] {
	bound := *b
	bound.tx = tx
	return &bound
}

// call is one operation's facts, gathered before it waits for anything
type call struct {
	path    []byte
	key     string
	now     int64 // unix milliseconds: the store's clock, read once a call
	options callOptions
}

// begin checks a call's key and options and reads the clock for it
func (b *Bucket[V]) begin(key any, options []Option) (call, error) {
	if b.err != nil {
		return call{}, b.err
	}
	text, err := keyText(key)
	if err != nil {
		return call{key: fmt.Sprint(key)}, err
	}
	path := appendKey(slices.Clone(b.prefix), text)
	if len(path) > maxPath {
		return call{key: text}, fmt.Errorf("%w: a path of %d bytes, over 1 KiB", tinystore.ErrInvalid, len(path))
	}
	collected, err := collect(options)
	return call{path: path, key: text, now: b.state.now().UnixMilli(), options: collected}, err
}

// fail names the bucket and key a call failed on; a cancellation, a closed
// store and a KeyError already made pass as they are
func (b *Bucket[V]) fail(c call, err error) error {
	var named *KeyError
	switch {
	case err == nil, errors.Is(err, context.Canceled), errors.Is(err, context.DeadlineExceeded),
		errors.Is(err, tinystore.ErrClosed), errors.As(err, &named):
		return err
	}
	return &KeyError{Bucket: b.name, Path: shownPath(b.owners, c.key), Err: err}
}

// read runs statements where this handle reads: its transaction, or a reader
// without one, each statement its own snapshot
func (b *Bucket[V]) read(ctx context.Context, work func(sqlite.Reader) error) error {
	if b.tx != nil {
		reader, err := b.tx.reader(b.state)
		if err != nil {
			return err
		}
		return work(reader)
	}
	release, err := b.state.admit(ctx)
	if err != nil {
		return err
	}
	defer release()
	return b.state.file.Lookup(ctx, work)
}

// write runs a write where this handle writes: its transaction, or a group
// that shares one commit with the writes beside it
func (b *Bucket[V]) write(ctx context.Context, bytes int, work func(sqlite.Writer) error) error {
	if b.tx != nil {
		writer, err := b.tx.writer(b.state)
		if err != nil {
			return err
		}
		return work(writer)
	}
	release, err := b.state.admitWrite(ctx)
	if err != nil {
		return err
	}
	defer release()
	unreserve, err := b.state.reserve(ctx, bytes)
	if err != nil {
		return err
	}
	defer unreserve()
	return b.state.file.UpdateGrouped(ctx, bytes, work)
}
