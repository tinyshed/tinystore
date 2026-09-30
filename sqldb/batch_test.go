package sqldb

import (
	"errors"
	"sync"
	"testing"
)

// Batches commit in the group commit their neighbours share, each whole or not
// at all: 64 batches of two notes and one whose second statement breaks a
// constraint take a few commits, and the broken one keeps neither of its notes.
func TestABatchCommitsTogetherAndFailsAlone(t *testing.T) {
	db := openNotes(t)
	ctx := t.Context()
	before, err := db.file.WriterCounters(ctx)
	if err != nil {
		t.Fatal(err)
	}

	const batches = 64
	errs := make([]error, batches+1)
	var wg sync.WaitGroup
	for n := range batches {
		wg.Go(func() {
			errs[n] = db.Batch(ctx, func(b *Batch) error {
				b.Exec(`insert into notes (id, title) values (?, 'first')`, 2*n+1)
				b.Exec(`insert into notes (id, title) values (?, 'second')`, 2*n+2)
				return nil
			})
		})
	}
	wg.Go(func() {
		errs[batches] = db.Batch(ctx, func(b *Batch) error {
			b.Exec(`insert into notes (id, title) values (1000, 'rolled back with its batch')`)
			b.Exec(`insert into notes (id, title) values (1001, null)`)
			return nil
		})
	})
	wg.Wait()

	if err = errors.Join(errs[:batches]...); err != nil {
		t.Fatalf("the batches answered %v", err)
	}
	var broken *ConstraintError
	if !errors.As(errs[batches], &broken) || broken.Kind != NotNullViolation {
		t.Fatalf("the broken batch answered %v", errs[batches])
	}
	if count, countErr := Scalar[int](ctx, db, `select count(*) from notes`); countErr != nil || count != 2*batches {
		t.Fatalf("%d notes after %d batches of two and a broken one: %v", count, batches, countErr)
	}
	after, err := db.file.WriterCounters(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if commits := after.Commits - before.Commits; commits > batches/4 {
		t.Fatalf("%d batches took %d commits", batches+1, commits)
	}
}

// A batch whose building fails, or whose argument no column holds, writes none
// of its statements; one with none commits nothing.
func TestABatchThatDoesNotBuildWritesNothing(t *testing.T) {
	db := openNotes(t)
	ctx := t.Context()
	changedItsMind := errors.New("the caller changed its mind")
	err := db.Batch(ctx, func(b *Batch) error {
		b.Exec(`insert into notes (id, title) values (1, 'never')`)
		return changedItsMind
	})
	if !errors.Is(err, changedItsMind) {
		t.Fatalf("a batch whose building failed: %v", err)
	}
	err = db.Batch(ctx, func(b *Batch) error {
		b.Exec(`insert into notes (id, title) values (2, 'never')`)
		b.Exec(`insert into notes (id, title) values (?, 'never')`, uint64(1)<<63)
		return nil
	})
	if err == nil {
		t.Fatal("a batch with an argument past int64 wrote")
	}
	if err = db.Batch(ctx, func(*Batch) error { return nil }); err != nil {
		t.Fatalf("an empty batch: %v", err)
	}
	if count, err := Scalar[int](ctx, db, `select count(*) from notes`); err != nil || count != 0 {
		t.Fatalf("%d notes after batches that wrote nothing: %v", count, err)
	}
}
