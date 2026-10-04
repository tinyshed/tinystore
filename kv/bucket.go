package kv

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"reflect"
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
	branch
	codec   codec[V]
	sliding time.Duration
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

	if settings.sliding > 0 {
		state.scheduleRenewals()
	}
	opened := branch{state: state, id: id, name: name, ttl: settings.ttl}
	return &Bucket[V]{branch: opened, codec: settings.codec, sliding: settings.sliding}, nil
}

// settled is a bucket's options, its codec chosen and a Sliding term given
// to new keys as their lifetime
type settled[V any] struct {
	codec   codec[V]
	ttl     time.Duration
	sliding time.Duration
}

func settle[V any](options []BucketOption) (settled[V], error) {
	var said settings
	for _, option := range options {
		option.bucketOption(&said)
	}
	if said.err == nil && said.sliding > 0 && said.ttl > 0 {
		said.err = fmt.Errorf("%w: Sliding gives new keys its term, and DefaultTTL beside it another",
			tinystore.ErrInvalid)
	}
	if said.err != nil {
		return settled[V]{}, said.err
	}
	chosen := settled[V]{codec: codecFor[V](), ttl: max(said.ttl, said.sliding), sliding: said.sliding}
	if said.codec != nil {
		custom, ok := said.codec.(Codec[V])
		if !ok {
			return settled[V]{}, fmt.Errorf("%w: a codec of %T for values of %s",
				tinystore.ErrInvalid, said.codec, reflect.TypeFor[V]())
		}
		chosen.codec = customCodec(custom)
	}
	return chosen, nil
}

const (
	selectBucket = `select id, kind from _tinystore_kv_buckets as buckets where name = ?1`
	insertBucket = `insert into _tinystore_kv_buckets as buckets (name, kind) values (?1, ?2) returning id`
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
		scanErr := sqlite.QueryRowByKey(ctx, w, selectBucket, name).Scan(&id, &found)
		switch {
		case errors.Is(scanErr, sql.ErrNoRows):
			return sqlite.QueryRowByKey(ctx, w, insertBucket, name, kind).Scan(&id)
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
	return &Bucket[V]{branch: b.under(owners), codec: b.codec, sliding: b.sliding}
}

// WithTx is this bucket inside tx: its calls run in tx's transaction and see
// its writes, and after the function tx was given returns they are ErrClosed.
func (b *Bucket[V]) WithTx(tx *Tx) *Bucket[V] {
	bound := *b
	bound.tx = tx
	return &bound
}
