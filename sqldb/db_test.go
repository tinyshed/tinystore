package sqldb

import (
	"context"
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
	return openStoreWith(t, dir, tinystore.Options{Manual: true})
}

func openStoreWith(t *testing.T, dir string, options tinystore.Options) *tinystore.Store {
	t.Helper()
	store, err := tinystore.Open(t.Context(), dir, options)
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
	db, err := Open(t.Context(), openStore(t, t.TempDir()), "app", notesMigrations, nil)
	if err != nil {
		t.Fatal(err)
	}
	return db
}

func TestNotesThroughEveryCall(t *testing.T) {
	db := openNotes(t)
	ctx := t.Context()

	created, found, err := ExecOne[note](ctx, db, `insert into notes (title, body) values (?, ?) returning *`, "first", "hello")
	if err != nil || !found || created.ID == 0 || created.Title != "first" {
		t.Fatalf("create: %+v, %t, %v", created, found, err)
	}
	got, found, err := One[note](ctx, db, `select * from notes where id = ?`, created.ID)
	if err != nil || !found || got != created {
		t.Fatalf("get: %+v, %t, %v", got, found, err)
	}
	renamed, found, err := ExecOne[note](ctx, db, `update notes set title = ? where id = ? returning *`, "renamed", created.ID)
	if err != nil || !found || renamed.Title != "renamed" {
		t.Fatalf("rename: %+v, %t, %v", renamed, found, err)
	}
	id, err := ExecScalar[int64](ctx, db, `insert into notes (title) values ('second') returning id`)
	if err != nil || id != created.ID+1 {
		t.Fatalf("second id: %d, %v", id, err)
	}
	listed, err := All[note](ctx, db, `select * from notes order by id`)
	if err != nil || len(listed) != 2 || listed[0].Title != "renamed" {
		t.Fatalf("list: %+v, %v", listed, err)
	}
	touched, err := ExecAll[note](ctx, db, `update notes set body = 'seen' returning *`)
	if err != nil || len(touched) != 2 || touched[0].Body != "seen" || touched[1].Body != "seen" {
		t.Fatalf("touch every note: %+v, %v", touched, err)
	}
	if _, err = db.Exec(ctx, `delete from notes where id = ?`, created.ID); err != nil {
		t.Fatal(err)
	}
	count, err := Scalar[int](ctx, db, `select count(*) from notes`)
	if err != nil || count != 1 {
		t.Fatalf("count after delete: %d, %v", count, err)
	}
	if _, found, err = One[note](ctx, db, `select * from notes where id = ?`, created.ID); found || err != nil {
		t.Fatalf("a deleted note: found %t, %v", found, err)
	}
	if _, found, err = ExecOne[note](ctx, db, `update notes set title = 'x' where id = 999 returning *`); found || err != nil {
		t.Fatalf("updating no note: found %t, %v", found, err)
	}
}

func TestAReadCannotWriteAndSaysWhereToWrite(t *testing.T) {
	db := openNotes(t)
	_, err := Scalar[int64](t.Context(), db, `insert into notes (title) values ('sneaky') returning id`)
	if !errors.Is(err, tinystore.ErrInvalid) || !strings.Contains(err.Error(), "use Exec") {
		t.Fatalf("a write sent to a read: %v", err)
	}
	if count, _ := Scalar[int](t.Context(), db, `select count(*) from notes`); count != 0 {
		t.Fatalf("the refused write left %d rows", count)
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

// explosive panics when a column is scanned into it
type explosive struct{}

func (*explosive) Scan(any) error { panic("the scan exploded") }

type panicky struct {
	ID    int64
	Title explosive
}

// A panic inside a grouped statement's own work rolls back that statement alone
// and goes on in its caller's goroutine, whichever goroutine ran it. The writer
// takes the next write.
func TestAPanicInsideAWriteRollsBackItsStatementAlone(t *testing.T) {
	db := openNotes(t)
	ctx := t.Context()
	func() {
		defer func() {
			if recovered := fmt.Sprint(recover()); recovered != "the scan exploded" {
				t.Fatalf("the caller recovered %v", recovered)
			}
		}()
		_, _, _ = ExecOne[panicky](ctx, db, `insert into notes (id, title) values (7, 'lost') returning id, title`)
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
	if _, err = Open(t.Context(), store, "app", notesMigrations, nil); err != nil {
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
	db, err := Open(t.Context(), store, "app", grown, nil)
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
	_, err = Open(t.Context(), openStore(t, dir), "app", edited, nil)
	if !errors.Is(err, tinystore.ErrInvalid) || !strings.Contains(err.Error(), "migration 1 changed after application") {
		t.Fatalf("a database whose applied migration was edited: %v", err)
	}
}

// an embed.FS of migrations/*.sql holds them in its one directory, which Open
// finds; two directories are a question Open does not answer
func TestTheMigrationsAreFoundInTheirOneDirectory(t *testing.T) {
	store := openStore(t, t.TempDir())
	embedded := fstest.MapFS{"migrations/001_notes.sql": notesMigrations["001_notes.sql"]}
	db, err := Open(t.Context(), store, "app", embedded, nil)
	if err != nil {
		t.Fatal(err)
	}
	if _, err = db.Exec(t.Context(), `insert into notes (title) values ('found')`); err != nil {
		t.Fatal(err)
	}
	two := fstest.MapFS{"app/001.sql": notesMigrations["001_notes.sql"], "billing/001.sql": notesMigrations["001_notes.sql"]}
	if _, err = Open(t.Context(), store, "billing", two, nil); !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("two directories of migrations: %v", err)
	}
}

func TestNamesAreCheckedAndEachIsOpenedOnce(t *testing.T) {
	dir := t.TempDir()
	store := openStore(t, dir)
	for _, name := range []string{"", "App", "../x", "a b", string(make([]byte, 65))} {
		if _, err := Open(t.Context(), store, name, notesMigrations, nil); !errors.Is(err, tinystore.ErrInvalid) {
			t.Errorf("name %q: %v", name, err)
		}
	}
	if _, err := Open(t.Context(), store, "app", notesMigrations, nil); err != nil {
		t.Fatal(err)
	}
	if _, err := Open(t.Context(), store, "app", notesMigrations, nil); !errors.Is(err, tinystore.ErrInUse) {
		t.Fatalf("app twice: %v", err)
	}
	if _, err := Open(t.Context(), store, "audit", notesMigrations, nil); err != nil {
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
	db, err := Open(t.Context(), store, "app", notesMigrations, nil)
	if err != nil {
		t.Fatal(err)
	}
	if err = store.Close(t.Context()); err != nil {
		t.Fatal(err)
	}
	if _, err = Scalar[int](t.Context(), db, `select 1`); !errors.Is(err, tinystore.ErrClosed) {
		t.Fatalf("a read after close: %v", err)
	}
	if _, err = db.Exec(t.Context(), `delete from notes`); !errors.Is(err, tinystore.ErrClosed) {
		t.Fatalf("a write after close: %v", err)
	}
}

// ApplyNone opens only a file that applied every migration given, and makes
// none; Migrated checks an open file the same way
func TestApplyNoneAndMigratedApplyNothing(t *testing.T) {
	dir := t.TempDir()
	store := openStore(t, dir)
	_, err := Open(t.Context(), store, "app", notesMigrations, nil, ApplyNone())
	if !errors.Is(err, ErrPending) || !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("a file no migration made: %v", err)
	}
	if _, statErr := os.Stat(filepath.Join(dir, "sql", "app.db")); !errors.Is(statErr, os.ErrNotExist) {
		t.Fatalf("ApplyNone made the file: %v", statErr)
	}

	db, err := Open(t.Context(), store, "app", notesMigrations, nil)
	if err != nil {
		t.Fatal(err)
	}
	if err := db.Migrated(t.Context(), notesMigrations); err != nil {
		t.Fatalf("the migrations it applied: %v", err)
	}
	more := fstest.MapFS{
		"001_notes.sql": notesMigrations["001_notes.sql"],
		"002_tags.sql":  {Data: []byte(`create table tags (name text primary key) strict;`)},
	}
	if err := db.Migrated(t.Context(), more); !errors.Is(err, ErrPending) {
		t.Fatalf("a migration it has not applied: %v", err)
	}
	if _, err := Scalar[int](t.Context(), db, `select count(*) from tags`); err == nil {
		t.Fatal("Migrated applied a migration")
	}
	changed := fstest.MapFS{"001_notes.sql": {Data: []byte(`create table notes (id integer primary key) strict;`)}}
	if err := db.Migrated(t.Context(), changed); err == nil || errors.Is(err, ErrPending) {
		t.Fatalf("a changed migration: %v", err)
	}

	if err := store.Close(t.Context()); err != nil {
		t.Fatal(err)
	}

	again := openStore(t, dir)
	if _, err := Open(t.Context(), again, "app", more, nil, ApplyNone()); !errors.Is(err, ErrPending) {
		t.Fatalf("a file short of a migration: %v", err)
	}
	if _, err := Open(t.Context(), again, "app", notesMigrations, nil, ApplyNone()); err != nil {
		t.Fatalf("a file that applied every migration: %v", err)
	}
}

// searchSQL is docs/sqldb.md's full-text recipe for the notes, and a table of
// places by their bounds
const searchSQL = `create virtual table notes_fts using fts5(title, body, content = 'notes', content_rowid = 'id');
create trigger notes_fts_insert after insert on notes begin
	insert into notes_fts (rowid, title, body) values (new.id, new.title, new.body);
end;
create trigger notes_fts_delete after delete on notes begin
	insert into notes_fts (notes_fts, rowid, title, body) values ('delete', old.id, old.title, old.body);
end;
create virtual table places using rtree(id, min_x, max_x, min_y, max_y);
`

// FTS5 and R*Tree, which SQLite as the driver builds it leaves to each
// connection, work on the writer that runs the triggers and on the readers.
// The check of the file against its schema passes over their shadow tables,
// and a snapshot of the file searches as the file does.
func TestFullTextAndRTreeTablesWorkInTheFileAndItsSnapshot(t *testing.T) {
	schema := Schema(Table[note]("notes", PrimaryKey("id")))
	migrations := mapFS(map[string]string{"001_schema.sql": schema.SQL(), "002_search.sql": searchSQL})
	store := openStore(t, t.TempDir())
	db, err := Open(t.Context(), store, "app", migrations, schema)
	if err != nil {
		t.Fatal(err)
	}
	for _, n := range []note{
		{Title: "tinystore", Body: "an embedded data runtime"},
		{Title: "search", Body: "a full-text index"},
		{Title: "gone", Body: "a runtime deleted"},
	} {
		if _, err = db.Exec(t.Context(), `insert into notes (title, body) values (?, ?)`, n.Title, n.Body); err != nil {
			t.Fatal(err)
		}
	}
	if _, err = db.Exec(t.Context(), `delete from notes where title = 'gone'`); err != nil {
		t.Fatal(err)
	}
	if _, err = db.Exec(t.Context(), `insert into places values (1, 0, 10, 0, 10), (2, 20, 30, 20, 30)`); err != nil {
		t.Fatal(err)
	}
	searchNotesAndPlaces(t, db)

	snapshot, err := store.Snapshot(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	copied, err := Open(t.Context(), openStore(t, snapshot.Dir), "app", migrations, schema)
	if err != nil {
		t.Fatal(err)
	}
	searchNotesAndPlaces(t, copied)
}

func searchNotesAndPlaces(t *testing.T, db *DB) {
	t.Helper()
	hits, err := All[note](t.Context(), db, `select n.* from notes_fts f join notes n on n.id = f.rowid
		where notes_fts match ? order by bm25(notes_fts)`, "runtime")
	if err != nil || len(hits) != 1 || hits[0].Title != "tinystore" {
		t.Fatalf("the full-text search found %+v, %v", hits, err)
	}
	place, err := Scalar[int64](t.Context(), db,
		`select id from places where min_x <= 25 and max_x >= 25 and min_y <= 25 and max_y >= 25`)
	if err != nil || place != 2 {
		t.Fatalf("the R*Tree found place %d, %v", place, err)
	}
}
