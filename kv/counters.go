package kv

import (
	"context"
	"database/sql"
	"errors"
	"fmt"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/internal/sqlite"
)

const kindCounters = "counters"

// Counters holds an int64 by key. Of and WithTx give handles on the same
// counters, and a handle may be used from any number of goroutines.
type Counters struct {
	branch
	memory *memory // nil unless LoseAtMost
}

// OpenCounters opens the counters name of kv.db, creating them the first
// time. A name that holds values is ErrInvalid.
func OpenCounters(ctx context.Context, state *Store, name string, options ...CounterOption) (*Counters, error) {
	var said settings
	for _, option := range options {
		option.counterOption(&said)
	}
	if said.err != nil {
		return nil, fmt.Errorf("kv: counters %q: %w", name, said.err)
	}

	id, err := state.claimBucket(ctx, name, kindCounters)
	if err != nil {
		return nil, err
	}

	held, err := state.memoryFor(name, id, said.loseAtMost)
	if err != nil {
		return nil, err
	}

	return &Counters{state: state, id: id, name: name, ttl: said.ttl, memory: held}, nil
}

// Of is the branch of these counters that owners name, as Bucket.Of names one.
func (c *Counters) Of(owners ...any) *Counters {
	return &Counters{branch: c.under(owners), memory: c.memory}
}

// WithTx binds these counters to tx; LoseAtMost counters refuse calls through it.
func (c *Counters) WithTx(tx *Tx) *Counters {
	bound := *c
	bound.tx = tx
	return &bound
}

// The sum and the larger of a counter and n, in the writer. A counter expired
// or hidden by a Clear starts again from n with a new expiry, and a live one
// keeps its own.
//
// A sum past the int64 range writes nothing, since SQLite would turn it into a
// REAL.
var (
	counterGone = `(cells.expires <= ?6 or ` + hidden("cells", 7) + `)`
	addCounter  = `insert into cells (bucket, path, version, expires, value) values (?1, ?2, ?3, ?4, ?5)
		on conflict (bucket, path) do update set
			value   = iif(` + counterGone + `, excluded.value, cells.value + excluded.value),
			expires = iif(` + counterGone + `, excluded.expires, cells.expires),
			version = excluded.version
		where ` + counterGone + ` or typeof(cells.value + excluded.value) = 'integer'
		returning value`
	maxCounter = `insert into cells (bucket, path, version, expires, value) values (?1, ?2, ?3, ?4, max(?5, 0))
		on conflict (bucket, path) do update set
			value   = iif(` + counterGone + `, excluded.value, max(cells.value, ?5)),
			expires = iif(` + counterGone + `, excluded.expires, cells.expires),
			version = excluded.version
		returning value`
	selectCounter = `select value from cells
		where bucket = ?1 and path = ?2 and (expires is null or expires > ?3) and not ` + hidden("cells", 4)
)

const deleteCounter = `delete from cells where bucket = ?1 and path = ?2`

// Add adds n to the counter under key and returns what it holds now. An
// absent or expired counter starts from zero with the DefaultTTL of the
// counters, and a live one keeps its expiry, so a window does not slide. A sum
// past the int64 range is ErrLimit and changes nothing.
func (c *Counters) Add(ctx context.Context, key any, n int64) (int64, error) {
	if c.memory != nil {
		return c.changeInMemory(ctx, key, addTo(n))
	}
	return c.change(ctx, key, addCounter, n)
}

// Max keeps the larger of the counter under key and n, an absent counter
// counting as zero, and returns what it holds now.
func (c *Counters) Max(ctx context.Context, key any, n int64) (int64, error) {
	if c.memory != nil {
		return c.changeInMemory(ctx, key, func(held int64) (int64, error) { return max(held, n), nil })
	}
	return c.change(ctx, key, maxCounter, n)
}

// Get is what the counter under key holds; an absent or expired one holds 0.
func (c *Counters) Get(ctx context.Context, key any) (int64, error) {
	if c.memory != nil {
		return c.getInMemory(ctx, key)
	}
	cl, err := c.begin(key, nil)
	if err != nil {
		return 0, c.fail(cl, err)
	}

	var held int64
	err = c.read(ctx, func(r sqlite.Reader) error {
		var value any
		readErr := sqlite.QueryRowByKey(ctx, r, selectCounter, cl.args(c.id, cl.path, cl.now)...).Scan(&value)
		if errors.Is(readErr, sql.ErrNoRows) {
			return nil
		}
		if readErr != nil {
			return readErr
		}
		held, readErr = counted(value)
		return readErr
	})
	return held, c.fail(cl, err)
}

// Delete removes the counter under key; an absent one is not an error.
func (c *Counters) Delete(ctx context.Context, key any) error {
	if c.memory != nil {
		return c.deleteInMemory(ctx, key)
	}
	cl, err := c.begin(key, nil)
	if err != nil {
		return c.fail(cl, err)
	}

	err = c.write(ctx, 0, func(w sqlite.Writer) error {
		_, deleteErr := w.ExecContext(ctx, deleteCounter, c.id, cl.path)
		return deleteErr
	})
	return c.fail(cl, err)
}

// change runs one of the counter statements for key with n, taking the file's
// next revision as a Set does
func (c *Counters) change(ctx context.Context, key any, statement string, n int64) (int64, error) {
	cl, err := c.begin(key, nil)
	if err != nil {
		return 0, c.fail(cl, err)
	}

	var held int64
	expires := c.expiresFor(cl, cell{})
	err = c.write(ctx, 0, func(w sqlite.Writer) error {
		version, writeErr := c.state.nextRevision(ctx, w)
		if writeErr != nil {
			return writeErr
		}
		var value any
		writeErr = sqlite.QueryRowByKey(ctx, w, statement, cl.args(c.id, cl.path, version, expires, n, cl.now)...).
			Scan(&value)
		if errors.Is(writeErr, sql.ErrNoRows) {
			return fmt.Errorf("%w: adding %d passes the int64 range", tinystore.ErrLimit, n)
		}
		if writeErr != nil {
			return writeErr
		}
		held, writeErr = counted(value)
		return writeErr
	})
	return held, c.fail(cl, err)
}

// addTo is an Add in memory, refusing a sum past the int64 range as the
// file's statement does
func addTo(n int64) func(int64) (int64, error) {
	return func(held int64) (int64, error) {
		sum := held + n
		if (n > 0 && sum < held) || (n < 0 && sum > held) {
			return 0, fmt.Errorf("%w: adding %d passes the int64 range", tinystore.ErrLimit, n)
		}
		return sum, nil
	}
}

var errRelaxedInTx = fmt.Errorf("%w: kv: LoseAtMost counters live in memory between flushes and join no "+
	"transaction; open them without LoseAtMost to use them in Tx", tinystore.ErrInvalid)

// beginInMemory is begin for LoseAtMost counters, which refuse a transaction,
// and lets the call in while the store is open
func (c *Counters) beginInMemory(ctx context.Context, key any) (call, func(), error) {
	cl, err := c.begin(key, nil)
	if err == nil && c.tx != nil {
		err = errRelaxedInTx
	}
	if err != nil {
		return cl, nil, c.fail(cl, err)
	}
	release, err := c.state.admit(ctx)
	return cl, release, err
}

func (c *Counters) changeInMemory(ctx context.Context, key any, next func(int64) (int64, error)) (int64, error) {
	cl, release, err := c.beginInMemory(ctx, key)
	if err != nil {
		return 0, err
	}
	defer release()

	created := c.expiresFor(cl, cell{})
	held, err := c.memory.change(ctx, cl, created.Int64, next)
	return held, c.fail(cl, err)
}

func (c *Counters) getInMemory(ctx context.Context, key any) (int64, error) {
	cl, release, err := c.beginInMemory(ctx, key)
	if err != nil {
		return 0, err
	}
	defer release()

	held, err := c.memory.get(ctx, cl)
	return held, c.fail(cl, err)
}

func (c *Counters) deleteInMemory(ctx context.Context, key any) error {
	cl, release, err := c.beginInMemory(ctx, key)
	if err != nil {
		return err
	}
	defer release()

	return c.fail(cl, c.memory.forget(ctx, cl))
}

// counted is a counter's row value, which only an int64 can be
func counted(value any) (int64, error) {
	n, ok := value.(int64)
	if !ok {
		return 0, corrupt("an integer", value)
	}
	return n, nil
}
