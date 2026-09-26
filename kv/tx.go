package kv

import (
	"context"
	"database/sql"
	"fmt"
	"sync/atomic"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/internal/sqlite"
)

// Tx is one transaction of kv.db, given to the function passed to Store.Tx or
// Store.View and used by the goroutine that runs it; a bucket works in it
// through WithTx.
type Tx struct {
	state    *Store
	writes   sqlite.Writer // nil in a View
	snapshot sqlite.Reader
	open     atomic.Bool
}

// Tx runs work in one writer transaction over any buckets of kv.db: nil
// commits, an error or a panic rolls back. It never spans two engines.
func (s *Store) Tx(ctx context.Context, work func(*Tx) error) error {
	release, err := s.admitWrite(ctx)
	if err != nil {
		return err
	}
	defer release()

	return s.file.UpdatePrepared(ctx, func(w sqlite.Writer) error {
		tx := &Tx{state: s, writes: w}
		tx.open.Store(true)
		defer tx.open.Store(false)
		return work(tx)
	})
}

// View runs read against one snapshot of kv.db, held at most five seconds, as
// a records read is; a write inside it is ErrInvalid.
func (s *Store) View(ctx context.Context, read func(*Tx) error) error {
	release, err := s.admit(ctx)
	if err != nil {
		return err
	}
	defer release()

	ctx, cancel := context.WithTimeout(ctx, viewTimeout)
	defer cancel()
	return s.file.View(ctx, func(snapshot *sql.Tx) error {
		tx := &Tx{state: s, snapshot: snapshot}
		tx.open.Store(true)
		defer tx.open.Store(false)
		return read(tx)
	})
}

// reader is where a read inside the transaction runs: the writer, which sees
// the transaction's own writes, or a View's snapshot
func (t *Tx) reader(state *Store) (sqlite.Reader, error) {
	if err := t.usable(state); err != nil {
		return nil, err
	}
	if t.writes != nil {
		return t.writes, nil
	}
	return t.snapshot, nil
}

func (t *Tx) writer(state *Store) (sqlite.Writer, error) {
	if err := t.usable(state); err != nil {
		return nil, err
	}
	if t.writes == nil {
		return nil, fmt.Errorf("%w: kv: a write inside View; Tx writes", tinystore.ErrInvalid)
	}
	return t.writes, nil
}

func (t *Tx) usable(state *Store) error {
	switch {
	case t.state != state:
		return fmt.Errorf("%w: kv: a transaction of another store", tinystore.ErrInvalid)
	case !t.open.Load():
		return fmt.Errorf("%w: kv: a transaction used after its function returned", tinystore.ErrClosed)
	}
	return nil
}
