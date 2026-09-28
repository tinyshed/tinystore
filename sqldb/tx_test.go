package sqldb

import (
	"errors"
	"testing"

	"github.com/tinyshed/tinystore"
)

func TestTxCommitsOnNilAndRollsBackOnErrorOrPanic(t *testing.T) {
	db := openNotes(t)
	ctx := t.Context()
	insertTwo := func(tx *Tx) error {
		if _, err := tx.Exec(ctx, `insert into notes (title) values ('a')`); err != nil {
			return err
		}
		_, err := ExecScalar[int64](ctx, tx, `insert into notes (title) values ('b') returning id`)
		return err
	}

	failed := errors.New("changed my mind")
	if err := db.Tx(ctx, func(tx *Tx) error { return errors.Join(insertTwo(tx), failed) }); !errors.Is(err, failed) {
		t.Fatal(err)
	}
	func() {
		defer func() { _ = recover() }()
		_ = db.Tx(ctx, func(tx *Tx) error {
			_ = insertTwo(tx)
			panic("boom")
		})
	}()
	if count, _ := Scalar[int](ctx, db, `select count(*) from notes`); count != 0 {
		t.Fatalf("rolled back transactions left %d rows", count)
	}

	err := db.Tx(ctx, func(tx *Tx) error {
		if err := insertTwo(tx); err != nil {
			return err
		}
		seen, err := Scalar[int](ctx, tx, `select count(*) from notes`)
		if err == nil && seen != 2 {
			t.Errorf("the transaction saw %d of its own rows", seen)
		}
		return err
	})
	if err != nil {
		t.Fatal(err)
	}
	if count, _ := Scalar[int](ctx, db, `select count(*) from notes`); count != 2 {
		t.Fatalf("committed %d rows, want 2", count)
	}
}

// a View reads every statement from one snapshot, refuses a write, and a
// transaction used after its function returned is closed
func TestViewReadsOneSnapshotAndRefusesWrites(t *testing.T) {
	db := openNotes(t)
	ctx := t.Context()
	var kept *Tx
	err := db.View(ctx, func(tx *Tx) error {
		kept = tx
		before, err := Scalar[int](ctx, tx, `select count(*) from notes`)
		if err != nil {
			return err
		}
		if _, err = db.Exec(ctx, `insert into notes (title) values ('committed meanwhile')`); err != nil {
			return err
		}
		after, err := Scalar[int](ctx, tx, `select count(*) from notes`)
		if err == nil && after != before {
			t.Errorf("one View counted %d, then %d", before, after)
		}
		if _, err = tx.Exec(ctx, `delete from notes`); !errors.Is(err, tinystore.ErrInvalid) {
			t.Errorf("a write inside View: %v", err)
		}
		if _, _, err = ExecOne[note](ctx, tx, `delete from notes returning *`); !errors.Is(err, tinystore.ErrInvalid) {
			t.Errorf("an ExecOne inside View: %v", err)
		}
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
	if _, err = Scalar[int](ctx, kept, `select 1`); !errors.Is(err, tinystore.ErrClosed) {
		t.Fatalf("a transaction used after its function: %v", err)
	}
}
