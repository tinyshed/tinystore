package kv

import (
	"context"
	"fmt"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/internal/sqlite"
)

// Change is a key a batch of the database the store lives in writes with its
// rows, which Written, Deleted and Cleared give out: see Options.In and
// sqldb's Batch.Add. A program does not call its methods.
type Change interface {
	Bytes() (int, error)
	Apply(ctx context.Context, file *sqlite.File, w sqlite.Writer) error
	Done(err error)
}

// Written is Set as a change of a batch of the database the store lives in,
// so that the key commits with the batch's rows or not at all:
//
//	err := db.Batch(ctx, func(b *sqldb.Batch) error {
//		b.Exec(`insert into users (id, email) values (?, ?)`, id, email)
//		b.Add(sessions.Written(ctx, token, Session{User: id}))
//		return nil
//	})
//
// The bucket's store must be opened In that database. The change takes free
// memory for its value and holds it until the batch ends. Memory that is not
// free is ErrLimit when the batch weighs the change.
func (b *Bucket[V]) Written(ctx context.Context, key any, value V, options ...Option) Change {
	c, err := b.beginChange(key, options)
	if err != nil {
		return &keyChange{err: b.fail(c, err)}
	}
	entered, kept, err := b.keepValue(ctx, value, 0, b.enterChange)
	if err != nil {
		return &keyChange{err: b.fail(c, err)}
	}
	return &keyChange{
		state: b.state, bytes: kept.size(), leave: entered.leave,
		apply: func(ctx context.Context, w sqlite.Writer) error {
			_, setErr := b.setCell(ctx, w, c, kept)
			return b.fail(c, setErr)
		},
	}
}

// Deleted is Delete as a change of a batch of the database the store lives in.
func (b *Bucket[V]) Deleted(ctx context.Context, key any, options ...Option) Change {
	c, err := b.beginChange(key, options)
	if err != nil {
		return &keyChange{err: b.fail(c, err)}
	}
	entered, err := b.enterChange(ctx, 0)
	if err != nil {
		return &keyChange{err: b.fail(c, err)}
	}
	return &keyChange{
		state: b.state, leave: entered.leave,
		apply: func(ctx context.Context, w sqlite.Writer) error { return b.fail(c, b.deleteIn(ctx, w, c)) },
	}
}

// Cleared is Clear as a change of a batch of the database the store lives in.
func (b *Bucket[V]) Cleared(ctx context.Context) Change {
	if err := b.checkChange(); err != nil {
		return &keyChange{err: b.fail(call{}, err)}
	}
	entered, err := b.enterChange(ctx, 0)
	if err != nil {
		return &keyChange{err: b.fail(call{}, err)}
	}
	return &keyChange{
		state: b.state, leave: entered.leave,
		apply: func(ctx context.Context, w sqlite.Writer) error { return b.fail(call{}, b.clearIn(ctx, w)) },
	}
}

func (b *branch) beginChange(key any, options []Option) (call, error) {
	if err := b.checkChange(); err != nil {
		return call{}, err
	}
	return b.begin(key, options)
}

func (b *branch) checkChange() error {
	switch {
	case b.err != nil:
		return b.err
	case b.tx != nil:
		return fmt.Errorf("%w: kv: a change of a bucket inside a Tx", tinystore.ErrInvalid)
	}
	return nil
}

// A batch owns one write slot. Its changes hold the engine open and take
// only free memory: waiting here could wait for the batch's own resources.
func (b *branch) enterChange(ctx context.Context, bytes int) (place, error) {
	leave, err := b.state.admit(ctx)
	if err != nil {
		return place{}, err
	}
	entered := place{out: leave}
	if bytes > 0 {
		entered.reserved, err = b.state.runtime.ReserveNow(int64(bytes))
		if err != nil {
			leave()
			return place{}, fmt.Errorf("kv: prepare a change: %w", err)
		}
	}
	return entered, nil
}

// keyChange is one Written, Deleted or Cleared: the write it makes, and the
// place and memory it holds until its batch ends
type keyChange struct {
	state *Store
	bytes int
	apply func(context.Context, sqlite.Writer) error
	leave func() // nil once let go
	err   error
}

func (c *keyChange) Bytes() (int, error) {
	if c.err != nil {
		return 0, c.err
	}
	return c.bytes, nil
}

// Apply writes the key with the batch's writer, in a file that must be the one
// the bucket's store keeps its buckets in
func (c *keyChange) Apply(ctx context.Context, file *sqlite.File, w sqlite.Writer) error {
	if c.err != nil {
		return c.err
	}
	if file != c.state.file {
		return fmt.Errorf("%w: kv: the batch's database is not the one the bucket lives in; open its store with "+
			"Options.In", tinystore.ErrInvalid)
	}
	return c.apply(ctx, w)
}

// Done lets go of the write's place and memory, whatever the batch did.
func (c *keyChange) Done(error) {
	if c.leave != nil {
		c.leave()
		c.leave = nil
	}
}
