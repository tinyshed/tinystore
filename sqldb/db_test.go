package sqldb

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"testing/fstest"

	"github.com/tinyshed/tinystore"
)

var notesMigrations = fstest.MapFS{
	"001_notes.sql": {Data: []byte(`create table notes (
		id    integer primary key,
		title text not null,
		body  text not null default ''
	) strict;`)},
}

type note struct {
	ID    int64
	Title string
	Body  string
}

func openStore(t *testing.T, dir string) *tinystore.Store {
	t.Helper()
	store, err := tinystore.Open(t.Context(), dir, tinystore.Options{Manual: true})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := store.Close(context.Background()); err != nil {
			t.Error(err)
		}
	})
	return store
}

func openNotes(t *testing.T) *DB {
	t.Helper()
	db, err := Open(t.Context(), openStore(t, t.TempDir()), "app", notesMigrations)
	if err != nil {
		t.Fatal(err)
	}
	return db
}

func TestNotesThroughEveryCall(t *testing.T) {
	db := openNotes(t)
	ctx := t.Context()

	created, err := ExecOne[note](ctx, db, `insert into notes (title, body) values (?, ?) returning id, title, body`, "first", "hello")
	if err != nil || created.ID == 0 || created.Title != "first" {
		t.Fatalf("create: %+v, %v", created, err)
	}
	got, err := One[note](ctx, db, `select id, title, body from notes where id = ?`, created.ID)
	if err != nil || got != created {
		t.Fatalf("get: %+v, %v", got, err)
	}
	renamed, err := ExecOne[note](ctx, db, `update notes set title = ? where id = ? returning id, title, body`, "renamed", created.ID)
	if err != nil || renamed.Title != "renamed" {
		t.Fatalf("rename: %+v, %v", renamed, err)
	}
	id, err := ExecScalar[int64](ctx, db, `insert into notes (title) values ('second') returning id`)
	if err != nil || id != created.ID+1 {
		t.Fatalf("second id: %d, %v", id, err)
	}
	listed, err := All[note](ctx, db, `select id, title, body from notes order by id`)
	if err != nil || len(listed) != 2 || listed[0].Title != "renamed" {
		t.Fatalf("list: %+v, %v", listed, err)
	}
	if _, err = db.Exec(ctx, `delete from notes where id = ?`, created.ID); err != nil {
		t.Fatal(err)
	}
	count, err := Scalar[int](ctx, db, `select count(*) from notes`)
	if err != nil || count != 1 {
		t.Fatalf("count after delete: %d, %v", count, err)
	}
	if _, err = One[note](ctx, db, `select id, title, body from notes where id = ?`, created.ID); !errors.Is(err, sql.ErrNoRows) {
		t.Fatalf("a deleted note: %v", err)
	}
	if _, err = ExecOne[note](ctx, db, `update notes set title = 'x' where id = 999 returning id, title, body`); !errors.Is(err, sql.ErrNoRows) {
		t.Fatalf("updating no note: %v", err)
	}
}

func TestAReadCannotWriteAndSaysWhereToWrite(t *testing.T) {
	db := openNotes(t)
	_, err := Scalar[int64](t.Context(), db, `insert into notes (title) values ('sneaky') returning id`)
	if !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("a write sent to a read: %v", err)
	}
	if count, _ := Scalar[int](t.Context(), db, `select count(*) from notes`); count != 0 {
		t.Fatalf("the refused write left %d rows", count)
	}
}

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

	if err := db.Tx(ctx, insertTwo); err != nil {
		t.Fatal(err)
	}
	if count, _ := Scalar[int](ctx, db, `select count(*) from notes`); count != 2 {
		t.Fatalf("committed %d rows, want 2", count)
	}
}

// statements from many goroutines share a commit, and one that fails rolls
// back alone
func TestExecsShareACommitAndFailAlone(t *testing.T) {
	db := openNotes(t)
	ctx := t.Context()
	before, err := db.file.WriterCounters(ctx)
	if err != nil {
		t.Fatal(err)
	}

	const writers = 64
	errs := make([]error, writers+1)
	var wg sync.WaitGroup
	for n := range writers {
		wg.Go(func() {
			_, errs[n] = db.Exec(ctx, `insert into notes (id, title) values (?, ?)`, n+1, fmt.Sprint("note ", n))
		})
	}
	wg.Go(func() { _, errs[writers] = db.Exec(ctx, `insert into notes (id, title) values (1000, null)`) })
	wg.Wait()

	if err = errors.Join(errs[:writers]...); err != nil || errs[writers] == nil {
		t.Fatalf("the writes answered %v, the failing one %v", err, errs[writers])
	}
	count, err := Scalar[int](ctx, db, `select count(*) from notes`)
	if err != nil || count != writers {
		t.Fatalf("%d notes after %d writes: %v", count, writers, err)
	}
	after, err := db.file.WriterCounters(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if commits := after.Commits - before.Commits; commits > writers/4 {
		t.Fatalf("%d writes took %d commits", writers+1, commits)
	}
}

// throughNil takes its columns through an embedded pointer, which is nil in a
// new value, so reflection panics reaching them
type throughNil struct{ *note }

// a panic inside a grouped statement's own work rolls back that statement
// alone and goes on in its caller's goroutine, whichever goroutine ran it, and
// the writer takes the next write
func TestAPanicInsideAWriteRollsBackItsStatementAlone(t *testing.T) {
	db := openNotes(t)
	ctx := t.Context()
	func() {
		defer func() {
			if recovered := fmt.Sprint(recover()); !strings.Contains(recovered, "nil pointer to embedded struct") {
				t.Fatalf("the caller recovered %v", recovered)
			}
		}()
		_, _ = ExecOne[throughNil](ctx, db, `insert into notes (id, title) values (7, 'lost') returning id, title`)
	}()
	if _, err := db.Exec(ctx, `insert into notes (title) values ('after')`); err != nil {
		t.Fatalf("a write after the panic: %v", err)
	}
	if count, err := Scalar[int](ctx, db, `select count(*) from notes`); err != nil || count != 1 {
		t.Fatalf("%d notes after a panicked write and a good one: %v", count, err)
	}
}

func TestMigrationsApplyOnceAndAChangedOneRefuses(t *testing.T) {
	dir := t.TempDir()
	store, err := tinystore.Open(t.Context(), dir, tinystore.Options{Manual: true})
	if err != nil {
		t.Fatal(err)
	}
	if _, err = Open(t.Context(), store, "app", notesMigrations); err != nil {
		t.Fatal(err)
	}
	if err = store.Close(t.Context()); err != nil {
		t.Fatal(err)
	}

	grown := fstest.MapFS{
		"001_notes.sql": notesMigrations["001_notes.sql"],
		"002_tags.sql":  {Data: []byte(`create table tags (note_id integer not null, tag text not null) strict;`)},
	}
	store, err = tinystore.Open(t.Context(), dir, tinystore.Options{Manual: true})
	if err != nil {
		t.Fatal(err)
	}
	db, err := Open(t.Context(), store, "app", grown)
	if err != nil {
		t.Fatalf("a new migration: %v", err)
	}
	if _, err = db.Exec(t.Context(), `insert into tags values (1, 'go')`); err != nil {
		t.Fatalf("the new table: %v", err)
	}
	if err = store.Close(t.Context()); err != nil {
		t.Fatal(err)
	}

	edited := fstest.MapFS{
		"001_notes.sql": {Data: []byte(`create table notes (id integer primary key) strict;`)},
		"002_tags.sql":  grown["002_tags.sql"],
	}
	_, err = Open(t.Context(), openStore(t, dir), "app", edited)
	if err == nil || !strings.Contains(err.Error(), "migration 1 changed after application") {
		t.Fatalf("a database whose applied migration was edited: %v", err)
	}
}

func TestNamesAreCheckedAndEachIsOpenedOnce(t *testing.T) {
	dir := t.TempDir()
	store := openStore(t, dir)
	for _, name := range []string{"", "App", "../x", "a b", string(make([]byte, 65))} {
		if _, err := Open(t.Context(), store, name, notesMigrations); !errors.Is(err, tinystore.ErrInvalid) {
			t.Errorf("name %q: %v", name, err)
		}
	}
	if _, err := Open(t.Context(), store, "app", notesMigrations); err != nil {
		t.Fatal(err)
	}
	if _, err := Open(t.Context(), store, "app", notesMigrations); !errors.Is(err, tinystore.ErrInUse) {
		t.Fatalf("app twice: %v", err)
	}
	if _, err := Open(t.Context(), store, "audit", notesMigrations); err != nil {
		t.Fatal(err)
	}
	for _, name := range []string{"app", "audit"} {
		if _, err := os.Stat(filepath.Join(dir, "sql", name+".db")); err != nil {
			t.Error(err)
		}
	}
}

func TestClosingTheStoreClosesTheDatabase(t *testing.T) {
	store := openStore(t, t.TempDir())
	db, err := Open(t.Context(), store, "app", notesMigrations)
	if err != nil {
		t.Fatal(err)
	}
	if err = store.Close(t.Context()); err != nil {
		t.Fatal(err)
	}
	if _, err = Scalar[int](t.Context(), db, `select 1`); !errors.Is(err, tinystore.ErrClosed) {
		t.Fatalf("a read after close: %v", err)
	}
}
