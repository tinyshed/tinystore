package sqldb

import (
	"context"
	"errors"
	"strings"
	"testing"
	"time"

	"github.com/tinyshed/tinystore"
)

// A read and a write hold the store's memory before they decode or write. A
// read waits for it before it takes a reader, a write heavier than the whole
// budget is refused at once, and inside Tx a call takes only what is free.
func TestStoreMemoryBoundsReadsAndWrites(t *testing.T) {
	store := openStoreWith(t, t.TempDir(), tinystore.Options{Manual: true, Memory: 1 << 20})
	db, err := Open(t.Context(), store, "app", notesMigrations, nil)
	if err != nil {
		t.Fatal(err)
	}
	ctx := t.Context()

	if _, err = db.Exec(ctx, `insert into notes (title) values (?)`, strings.Repeat("x", 2<<20)); !errors.Is(err, tinystore.ErrLimit) {
		t.Fatalf("a write heavier than the store's memory: %v", err)
	}
	if _, err = db.Exec(ctx, `insert into notes (title) values (?)`, strings.Repeat("x", 300<<10)); err != nil {
		t.Fatal(err)
	}

	whole, err := store.Reserve(ctx, 1<<20)
	if err != nil {
		t.Fatal(err)
	}
	waiting, cancel := context.WithTimeout(ctx, 50*time.Millisecond)
	defer cancel()
	if _, _, err = One[note](waiting, db, `select * from notes`); !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("a read while the memory is held: %v", err)
	}
	err = db.Tx(ctx, func(tx *Tx) error {
		_, _, readErr := One[note](ctx, tx, `select * from notes`)
		return readErr
	})
	if !errors.Is(err, tinystore.ErrLimit) {
		t.Fatalf("a read inside Tx while the memory is held: %v", err)
	}
	whole.Release()

	if row, found, err := One[note](ctx, db, `select * from notes`); err != nil || !found || len(row.Title) != 300<<10 {
		t.Fatalf("a read once the memory is free: %d bytes, %v", len(row.Title), err)
	}
	if usage := store.Memory(); usage.Used != 0 || usage.Peak > usage.Capacity {
		t.Fatalf("after the calls the store holds %+v", usage)
	}
}

// a value SQLite would make longer than the store's memory is refused before
// SQLite allocates it, though the answer is one number; a shorter one is made
func TestAValueSQLiteWouldMakePastTheStoresMemoryIsRefused(t *testing.T) {
	store := openStoreWith(t, t.TempDir(), tinystore.Options{Manual: true, Memory: 1 << 20})
	db, err := Open(t.Context(), store, "app", notesMigrations, nil)
	if err != nil {
		t.Fatal(err)
	}
	ctx := t.Context()

	const measured = `select length(printf('%.*c', ?, 'x'))`
	if _, err = Scalar[int64](ctx, db, measured, 2<<20); !errors.Is(err, tinystore.ErrLimit) {
		t.Errorf("a text longer than the store's memory, measured: %v", err)
	}
	_, err = db.Exec(ctx, `insert into notes (title) values (printf('%.*c', ?, 'x'))`, 2<<20)
	if !errors.Is(err, tinystore.ErrLimit) {
		t.Errorf("a text longer than the store's memory, written: %v", err)
	}
	if length, err := Scalar[int64](ctx, db, measured, 300<<10); err != nil || length != 300<<10 {
		t.Errorf("a text within the store's memory, measured: %d, %v", length, err)
	}
}
