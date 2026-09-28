package sqldb

import (
	"context"
	"database/sql"
	"fmt"
	"sync/atomic"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/internal/sqlite"
)

// Tx is one transaction of the database, given to the function DB.Tx or
// DB.View runs and used by the goroutine that runs it; every typed call takes
// it where it takes a DB.
type Tx struct {
	db       *DB
	writer   sqlite.Writer // nil in a View
	reader   sqlite.Reader
	snapshot context.Context // a View's, whose end every statement inside it shares
	open     atomic.Bool
}

// Tx runs work in one writer transaction of its own, on the caller's
// goroutine: nil commits, an error rolls back, and a panic rolls back and goes
// on. A call on the DB inside its own Tx waits for the Tx, which waits for it,
// until the call's context ends; sqldb logs a write that has waited ten
// seconds, naming the transaction.
func (d *DB) Tx(ctx context.Context, work func(tx *Tx) error) error {
	from := caller()
	leave, err := d.admitWrite(ctx)
	if err != nil {
		return err
	}
	defer leave()

	var workErr error
	err = d.file.UpdatePrepared(ctx, func(w sqlite.Writer) error {
		d.holder.Store(&holder{site: from, since: time.Now()})
		defer d.holder.Store(nil)
		tx := &Tx{db: d, writer: w, reader: w}
		tx.open.Store(true)
		defer tx.open.Store(false)
		workErr = work(tx)
		return workErr
	})
	if workErr != nil && err == workErr { //nolint:errorlint // the work's own error, returned as it came
		return err
	}
	return d.explain(err)
}

// View runs read against one snapshot of the database, held at most five
// seconds, since the write-ahead log grows with the oldest reader; a write
// inside it is tinystore.ErrInvalid.
func (d *DB) View(ctx context.Context, read func(tx *Tx) error) error {
	leave, err := d.admit(ctx)
	if err != nil {
		return err
	}
	defer leave()

	ctx, cancel := context.WithTimeoutCause(ctx, d.snapshot, errSnapshotHeld)
	defer cancel()
	var readErr error
	err = d.file.ViewPrepared(ctx, func(r sqlite.Reader) error {
		tx := &Tx{db: d, reader: r, snapshot: ctx}
		tx.open.Store(true)
		defer tx.open.Store(false)
		readErr = read(tx)
		return readErr
	})
	if readErr != nil && err == readErr { //nolint:errorlint // as Tx's
		return err
	}
	return d.explain(heldTooLong(ctx, err))
}

// Exec runs one statement of the transaction.
func (t *Tx) Exec(ctx context.Context, query string, args ...any) (sql.Result, error) {
	c := &call{query: query, args: args, write: true}
	err := t.run(ctx, c)
	return c.result, err
}

// run runs a call on the transaction's connection, which it holds already, so
// it takes only the memory that is free rather than wait for any
func (t *Tx) run(ctx context.Context, c *call) error {
	switch {
	case !t.open.Load():
		return fmt.Errorf("%w: sql %q: a transaction used after its function returned", tinystore.ErrClosed, t.db.name)
	case c.write && t.writer == nil:
		return fmt.Errorf("%w: sql %q: a write inside View; Tx writes", tinystore.ErrInvalid, t.db.name)
	}
	args, err := c.arguments()
	if err != nil {
		return t.db.explain(err)
	}
	if deadline, bounded := t.snapshotEnds(); bounded {
		var cancel context.CancelFunc
		ctx, cancel = context.WithDeadlineCause(ctx, deadline, errSnapshotHeld)
		defer cancel()
	}
	first := int64(weigh(args))
	if c.rows != nil {
		first += firstHeld
	}
	held, err := t.db.hold(ctx, first, c.bound, false)
	if err != nil {
		return t.db.explain(fmt.Errorf("inside Tx or View, which hold what the calls holding memory wait for: %w", err))
	}
	defer held.release()

	if c.write {
		err = c.writeTo(ctx, t.writer, args, held)
	} else {
		err = c.readFrom(ctx, t.reader, args, held)
	}
	return t.db.explain(heldTooLong(ctx, err))
}

// snapshotEnds is when a View's snapshot ends: database/sql rolls its
// transaction back then, and a statement on its connection after that would
// read outside the snapshot, so every statement inside ends with it
func (t *Tx) snapshotEnds() (time.Time, bool) {
	if t.snapshot == nil {
		return time.Time{}, false
	}
	return t.snapshot.Deadline()
}
