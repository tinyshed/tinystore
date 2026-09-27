package kv

import (
	"cmp"
	"context"
	"database/sql"
	"errors"
	"fmt"
	"slices"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/internal/sqlite"
)

// branch is where a handle's keys live and where its calls run; a bucket of
// values and a set of counters each hold one, and Of and WithTx copy it
type branch struct {
	state  *Store
	id     int64
	name   string
	ttl    time.Duration
	owners []string
	prefix []byte
	tx     *Tx
	err    error
}

// under is the branch that owners name below this one, one after another
func (b *branch) under(owners []any) branch {
	below := *b
	below.owners = slices.Clone(b.owners)
	below.prefix = slices.Clone(b.prefix)
	for _, owner := range owners {
		text, err := keyText(owner)
		if err != nil {
			refused := &KeyError{Bucket: b.name, Path: shownPath(below.owners, fmt.Sprint(owner)), Err: err}
			below.err = cmp.Or[error](below.err, refused)
			continue
		}
		below.owners = append(below.owners, text)
		below.prefix = appendOwner(below.prefix, text)
	}
	return below
}

// call is one operation's facts, gathered before it waits for anything
type call struct {
	path    []byte
	key     string
	now     int64 // unix milliseconds: the store's clock, read once a call
	options callOptions
}

// begin checks a call's key and options and reads the clock for it
func (b *branch) begin(key any, options []Option) (call, error) {
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
func (b *branch) fail(c call, err error) error {
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
func (b *branch) read(ctx context.Context, work func(sqlite.Reader) error) error {
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

// reserve holds bytes of the store's memory for a call of this handle; inside
// Tx it waits for nothing, since the writes holding memory wait for its writer
func (b *branch) reserve(ctx context.Context, bytes int) (*tinystore.Reservation, error) {
	if b.tx != nil && b.tx.writes != nil {
		return b.state.reserveNow(bytes)
	}
	return b.state.reserve(ctx, bytes)
}

// write runs a write where this handle writes: its transaction, or a group
// that shares one commit with the writes beside it
func (b *branch) write(ctx context.Context, bytes int, work func(sqlite.Writer) error) error {
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
	reserved, err := b.state.reserve(ctx, bytes)
	if err != nil {
		return err
	}
	defer reserved.Release()
	return b.state.file.UpdateGrouped(ctx, bytes, work)
}

// expiresFor is the expiry a write gives: the call's own, the one a live key
// has, or the branch's default for a key the write creates
func (b *branch) expiresFor(c call, existing cell) sql.NullInt64 {
	switch {
	case c.options.hasExpiry():
		return sql.NullInt64{Int64: c.options.expiry(c.now), Valid: true}
	case existing.live(c.now):
		return existing.expires
	case b.ttl > 0:
		return sql.NullInt64{Int64: c.now + b.ttl.Milliseconds(), Valid: true}
	}
	return sql.NullInt64{}
}
