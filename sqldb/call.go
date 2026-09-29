package sqldb

import (
	"context"
	"database/sql"
	"errors"
	"fmt"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/internal/sqlite"
)

// Handle is where a call runs: a *DB, or the *Tx that DB.Tx or DB.View gives.
type Handle interface {
	run(ctx context.Context, c *call) error
}

// call is one statement a typed function runs: its text and arguments,
// whether it may write, and what reads the rows it returns
type call struct {
	query    string
	args     []any
	encoded  bool // the arguments are already as their columns hold them, as Insert's are
	write    bool
	rows     func(*sql.Rows, *held) error // nil for an Exec, which returns result
	result   sql.Result
	bound    int64 // what All holds at most
	snapshot bool  // Each's: its snapshot lasts snapshotHold at most
}

// errPanicked rolls back the statement whose reading panicked; the panic goes
// on in its caller's goroutine
var errPanicked = errors.New("sqldb: a write panicked")

// errSnapshotHeld is why an Each or a View stopped
var errSnapshotHeld = fmt.Errorf("%w: a snapshot held five seconds, as long as the write-ahead log may wait; "+
	"page by key", tinystore.ErrLimit)

func (c *call) arguments() ([]any, error) {
	if c.encoded {
		return c.args, nil
	}
	return encodeArguments(c.args)
}

func (d *DB) run(ctx context.Context, c *call) error {
	args, err := c.arguments()
	if err == nil && c.write {
		err = d.write(ctx, c, args)
	} else if err == nil {
		err = d.read(ctx, c, args)
	}
	return d.explain(err)
}

// read runs a query on a reader without a transaction, a statement being its
// own snapshot
func (d *DB) read(ctx context.Context, c *call, args []any) error {
	leave, err := d.admit(ctx)
	if err != nil {
		return err
	}
	defer leave()

	held, err := d.hold(ctx, firstHeld, c.bound, true)
	if err != nil {
		return err
	}
	defer held.release()

	if c.snapshot {
		var cancel context.CancelFunc
		ctx, cancel = context.WithTimeoutCause(ctx, d.snapshot, errSnapshotHeld)
		defer cancel()
	}
	err = d.file.Lookup(ctx, func(r sqlite.Reader) error { return c.readFrom(ctx, r, args, held) })
	return heldTooLong(ctx, err)
}

// write runs a statement grouped with the writes other goroutines wait to
// commit, which may run it on theirs: a panic reading its rows rolls back its
// savepoint alone and goes on in its caller's goroutine
func (d *DB) write(ctx context.Context, c *call, args []any) error {
	leave, err := d.admitWrite(ctx)
	if err != nil {
		return err
	}
	defer leave()

	weight := weigh(args)
	first := int64(weight)
	if c.rows != nil {
		first += firstHeld
	}
	held, err := d.hold(ctx, first, c.bound, true)
	if err != nil {
		return err
	}
	defer held.release()
	held.used = int64(weight)

	var panicked any
	err = d.file.UpdateGroupedAs(ctx, c.query, weight, func(w sqlite.Writer) (err error) {
		defer func() {
			if recovered := recover(); recovered != nil {
				panicked, err = recovered, errPanicked
			}
		}()
		return c.writeTo(ctx, sqlite.UntilDeadline(w), args, held)
	})
	if panicked != nil {
		panic(panicked)
	}
	return err
}

func (c *call) readFrom(ctx context.Context, r sqlite.Reader, args []any, held *held) error {
	rows, err := r.QueryContext(ctx, c.query, args...) //nolint:rowserrcheck // every rows function checks Err
	if err != nil {
		return err
	}
	defer rows.Close()
	if err = c.rows(rows, held); err != nil {
		return err
	}
	return rows.Close()
}

func (c *call) writeTo(ctx context.Context, w sqlite.Writer, args []any, held *held) error {
	if c.rows != nil {
		return c.readFrom(ctx, w, args, held)
	}
	result, err := w.ExecContext(ctx, c.query, args...)
	c.result = result
	return err
}

// weigh is what a write's arguments hold, against a group's bound
func weigh(args []any) int {
	weight := 0
	for _, arg := range args {
		switch value := arg.(type) {
		case string:
			weight += len(value)
		case []byte:
			weight += len(value)
		case sql.NamedArg:
			weight += weigh([]any{value.Value})
		default:
			weight += 8
		}
	}
	return weight
}

// held is the store's memory one call holds for what it decodes: a first part
// reserved before it waits for a connection, then more only while it is free,
// since the call then holds a connection that a call holding memory may wait for
type held struct {
	runtime  *tinystore.Store // nil when the store has no Options.Memory
	parts    []*tinystore.Reservation
	capacity int64
	used     int64
	bound    int64 // All's own bound; zero for none
}

// hold reserves first bytes of the store's memory, waiting for them when wait
// says the call holds nothing another call may wait for
func (d *DB) hold(ctx context.Context, first, bound int64, wait bool) (*held, error) {
	h := &held{bound: bound}
	if !d.budgeted || first <= 0 {
		return h, nil
	}
	var reserved *tinystore.Reservation
	var err error
	if wait {
		reserved, err = d.runtime.Reserve(ctx, first)
	} else {
		reserved, err = d.runtime.ReserveNow(first)
	}
	if err != nil {
		return nil, err
	}
	h.runtime, h.parts, h.capacity = d.runtime, []*tinystore.Reservation{reserved}, first
	return h, nil
}

// take counts bytes more of what the call holds, as All decodes a row
func (h *held) take(bytes int64) error {
	h.used += bytes
	if h.bound > 0 && h.used > h.bound {
		return fmt.Errorf("%w: All holds %d MiB of rows at most; Each reads them a row at a time",
			tinystore.ErrLimit, h.bound>>20)
	}
	return h.fit()
}

// holdRow counts one row alone, as Each hands them out
func (h *held) holdRow(bytes int64) error {
	h.used = bytes
	return h.fit()
}

// fit reserves what the call holds past its reservations, twice as much as it
// has while that is free, so that a large result reserves a few times
func (h *held) fit() error {
	if h.runtime == nil || h.used <= h.capacity {
		return nil
	}
	grow := max(h.used-h.capacity, h.capacity)
	reserved, err := h.runtime.ReserveNow(grow)
	if err != nil {
		grow = h.used - h.capacity
		if reserved, err = h.runtime.ReserveNow(grow); err != nil {
			return fmt.Errorf("rows of %d bytes, past what the store's memory has free: %w", h.used, err)
		}
	}
	h.parts = append(h.parts, reserved)
	h.capacity += grow
	return nil
}

func (h *held) release() {
	for _, reserved := range h.parts {
		reserved.Release()
	}
}
