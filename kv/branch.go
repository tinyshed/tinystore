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
	hidden int64 // packed prefix lengths for the branch's ancestor clear marks
	tx     *Tx
	err    error
}

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
		below.markDepth()
	}
	return below
}

// markDepth packs the length of the branch's prefix in the bits of its depth,
// or says that it lies deeper than they reach
//
//	Of("tenant-7", 42) → prefixes of 10 and 14 bytes → 10 | 14<<10 = 14346
func (b *branch) markDepth() {
	if depth := len(b.owners); depth <= hiddenLevels && len(b.prefix) < 1<<lengthBits {
		b.hidden |= int64(len(b.prefix)) << ((depth - 1) * lengthBits)
		return
	}
	b.hidden |= 1 << deeperBit
}

// call is one operation's facts, gathered before it waits for anything
type call struct {
	path    []byte
	key     string
	now     int64 // unix milliseconds: the store's clock, read once a call
	options callOptions
	hidden  int64 // packed prefix lengths for clear-mark checks
}

// args appends the packed prefix lengths used by clear-mark checks
func (c call) args(own ...any) []any {
	return append(own, c.hidden)
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
	return call{path: path, key: text, now: b.state.now().UnixMilli(), options: collected, hidden: b.hidden}, err
}

// fail names the bucket and key a call failed on; a cancellation, a closed
// store and a KeyError already made pass as they are
func (b *branch) fail(c call, err error) error {
	if err == nil {
		return nil
	}
	var named *KeyError
	switch {
	case errors.Is(err, context.Canceled), errors.Is(err, context.DeadlineExceeded),
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

// reserve holds bytes of the store's memory for a call of this handle. Inside
// Tx or View it waits for nothing: the calls holding memory wait for the
// writer or the reader it holds.
func (b *branch) reserve(ctx context.Context, bytes int) (*tinystore.Reservation, error) {
	if b.tx != nil {
		return b.state.reserveNow(bytes)
	}
	return b.state.reserve(ctx, bytes)
}

// place is a write's place among the writes waiting for the writer, and the
// store's memory it holds, nil for none
type place struct {
	reserved *tinystore.Reservation
	out      func()
}

// keep gives back all but bytes of the memory the place holds
func (p place) keep(bytes int) {
	if p.reserved != nil {
		p.reserved.Shrink(int64(bytes))
	}
}

func (p place) leave() {
	if p.reserved != nil {
		p.reserved.Release()
	}
	p.out()
}

// enter takes a write's place and the memory it will hold before the write
// makes anything, so that a write waiting for its turn holds nothing the store
// has not counted; inside Tx the place is the transaction's
func (b *branch) enter(ctx context.Context, held int) (place, error) {
	out, err := b.takeTurn(ctx)
	if err != nil {
		return place{}, err
	}
	if held <= 0 {
		return place{out: out}, nil
	}
	reserved, err := b.reserve(ctx, held)
	if err != nil {
		out()
		return place{}, err
	}
	return place{reserved: reserved, out: out}, nil
}

// takeTurn lets a write in among the writes waiting for the writer; inside Tx
// the turn is the transaction's, and a View has none to give
func (b *branch) takeTurn(ctx context.Context) (out func(), err error) {
	if b.tx == nil {
		return b.state.admitWrite(ctx)
	}
	if _, err = b.tx.writer(b.state); err != nil {
		return nil, err
	}
	return func() {}, nil
}

// write runs a write where this handle writes, holding held bytes of the
// store's memory for what it reads
func (b *branch) write(ctx context.Context, held int, work func(sqlite.Writer) error) error {
	entered, err := b.enter(ctx, held)
	if err != nil {
		return err
	}
	defer entered.leave()
	return b.commit(ctx, 0, work)
}

// commit runs a write that holds its place: in the handle's transaction, or in
// a group that shares one commit with the writes beside it, bytes weighing it
// against the group's bound
func (b *branch) commit(ctx context.Context, bytes int, work func(sqlite.Writer) error) error {
	if b.tx != nil {
		writer, err := b.tx.writer(b.state)
		if err != nil {
			return err
		}
		return work(writer)
	}
	return b.state.file.UpdateGrouped(ctx, bytes, work)
}

const (
	savepointAlone = `savepoint alone`
	releaseAlone   = `release alone`
	rollbackAlone  = `rollback to alone`
)

// commitAlone is commit for a write that must fail alone: a group gives each
// write a savepoint, and inside Tx it takes one of its own
func (b *branch) commitAlone(ctx context.Context, work func(sqlite.Writer) error) error {
	if b.tx == nil {
		return b.commit(ctx, 0, work)
	}
	return b.commit(ctx, 0, func(w sqlite.Writer) error {
		if _, err := w.ExecContext(ctx, savepointAlone); err != nil {
			return err
		}
		if failed := work(w); failed != nil {
			_, rollbackErr := w.ExecContext(ctx, rollbackAlone)
			_, releaseErr := w.ExecContext(ctx, releaseAlone)
			return errors.Join(failed, rollbackErr, releaseErr)
		}
		_, err := w.ExecContext(ctx, releaseAlone)
		return err
	})
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
