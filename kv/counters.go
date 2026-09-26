package kv

import (
	"context"
	"database/sql"
	"errors"
	"fmt"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/internal/sqlite"
)

// the kind of bucket that holds counters
const kindCounters = "counters"

// Counters holds an int64 by key. Of and WithTx give handles on the same
// counters, and a handle may be used from any number of goroutines.
type Counters struct {
	branch
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

	return &Counters{branch: branch{state: state, id: id, name: name, ttl: said.ttl}}, nil
}

// Of is the branch of these counters that owners name, as Bucket.Of names one.
func (c *Counters) Of(owners ...any) *Counters {
	return &Counters{branch: c.under(owners)}
}

// WithTx is these counters inside tx, as Bucket.WithTx is a bucket.
func (c *Counters) WithTx(tx *Tx) *Counters {
	bound := *c
	bound.tx = tx
	return &bound
}

// the sum and the larger of a counter and n, in the writer: an expired counter
// starts again from n with a new expiry, a live one keeps its own, and a sum
// past the int64 range writes nothing, since SQLite would turn it into a REAL
const (
	addCounter = `insert into cells (bucket, path, version, expires, value) values (?1, ?2, ?3, ?4, ?5)
		on conflict (bucket, path) do update set
			value   = iif(cells.expires <= ?6, excluded.value, cells.value + excluded.value),
			expires = iif(cells.expires <= ?6, excluded.expires, cells.expires),
			version = excluded.version
		where cells.expires <= ?6 or typeof(cells.value + excluded.value) = 'integer'
		returning value`
	maxCounter = `insert into cells (bucket, path, version, expires, value) values (?1, ?2, ?3, ?4, max(?5, 0))
		on conflict (bucket, path) do update set
			value   = iif(cells.expires <= ?6, excluded.value, max(cells.value, ?5)),
			expires = iif(cells.expires <= ?6, excluded.expires, cells.expires),
			version = excluded.version
		returning value`
	selectCounter = `select value from cells where bucket = ?1 and path = ?2 and (expires is null or expires > ?3)`
	deleteCounter = `delete from cells where bucket = ?1 and path = ?2`
)

// Add adds n to the counter under key and returns what it holds now. An
// absent or expired counter starts from zero with the DefaultTTL of the
// counters, and a live one keeps its expiry, so a window does not slide. A sum
// past the int64 range is ErrLimit and changes nothing.
func (c *Counters) Add(ctx context.Context, key any, n int64) (int64, error) {
	return c.change(ctx, key, addCounter, n)
}

// Max keeps the larger of the counter under key and n, an absent counter
// counting as zero, and returns what it holds now.
func (c *Counters) Max(ctx context.Context, key any, n int64) (int64, error) {
	return c.change(ctx, key, maxCounter, n)
}

// Get is what the counter under key holds; an absent or expired one holds 0.
func (c *Counters) Get(ctx context.Context, key any) (int64, error) {
	cl, err := c.begin(key, nil)
	if err != nil {
		return 0, c.fail(cl, err)
	}

	var held int64
	err = c.read(ctx, func(r sqlite.Reader) error {
		var value any
		readErr := sqlite.QueryRow(ctx, r, selectCounter, c.id, cl.path, cl.now).Scan(&value)
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
		writeErr = sqlite.QueryRow(ctx, w, statement, c.id, cl.path, version, expires, n, cl.now).Scan(&value)
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

// counted is a counter's row value, which only an int64 can be
func counted(value any) (int64, error) {
	n, ok := value.(int64)
	if !ok {
		return 0, corrupt("an integer", value)
	}
	return n, nil
}
