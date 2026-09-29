package sqldb

import (
	"errors"
	"strings"
	"testing"
	"testing/fstest"
	"time"

	"github.com/tinyshed/tinystore"
)

func openDesign(t *testing.T, d design) *DB {
	t.Helper()
	db, err := Open(t.Context(), openStore(t, t.TempDir()), "app", designMigrations(t), d.schema)
	if err != nil {
		t.Fatal(err)
	}
	return db
}

// NoteWithDescription is Note a month later, a field the migrations do not
// make yet
type NoteWithDescription struct {
	Note
	Description string
}

// Open applies the migrations, checks the file against the schema, refuses a
// file that differs naming each difference, and changes nothing while it does
func TestOpenChecksTheFileAgainstTheSchemaAndChangesNothing(t *testing.T) {
	d := declareDesign(t)
	dir := t.TempDir()
	store := openStore(t, dir)
	if _, err := Open(t.Context(), store, "app", designMigrations(t), d.schema); err != nil {
		t.Fatalf("the migrations make what the schema declares: %v", err)
	}

	grown := Table[NoteWithDescription]("notes",
		PrimaryKey("id"),
		References("author_id", d.users, Cascade),
		Default("done", false),
		Default("tags", []string{}),
		Index("author_id", "created_at"),
		Index("title"),
		Check("length(title) > 0"),
	)
	_, err := Open(t.Context(), store, "later", designMigrations(t), Schema(d.users, grown))
	for _, want := range []string{
		`sql "later": the file does not match the schema:`,
		"notes.description is declared in NoteWithDescription, and the file has no such column; is a migration missing?",
		"the index notes_title on notes is declared, and the file has no such index",
		"the file has the index notes_open on notes, which the schema does not declare",
	} {
		if !errors.Is(err, tinystore.ErrInvalid) || !strings.Contains(err.Error(), want) {
			t.Fatalf("a schema ahead of its migrations: %v\nwant %q", err, want)
		}
	}

	db, err := Open(t.Context(), store, "unchecked", designMigrations(t), nil)
	if err != nil {
		t.Fatal(err)
	}
	columns, err := Scalar[int](t.Context(), db, `select count(*) from pragma_table_xinfo('notes')`)
	if err != nil || columns != 7 {
		t.Fatalf("the refused Open left notes with %d columns, %v", columns, err)
	}
}

// What the file may spell otherwise and still match:
//
//	a default whose text SQLite computes to the same value
//	an expression
//	a check
//	the case of a name
//	an index a migration named otherwise
func TestOpenDoesNotRefuseAnExpressionSpelledOtherwise(t *testing.T) {
	d := declareDesign(t)
	spelled := fstest.MapFS{"001_notes.sql": {Data: []byte(`
		create table USERS (
			ID integer primary key, EMAIL text not null, DISPLAY_NAME text not null, CREATED_AT int not null
		) strict;
		create unique index users_email on users (email);
		create table notes (
			id text not null primary key,
			author_id integer not null references Users on delete cascade,
			title text not null,
			done integer not null default 00 check ( done IN(0,1) ),
			tags text not null default ('[]') check (json_valid ( tags )),
			due text check (due is date(due) and due > '2000-01-01'),
			created_at integer not null,
			check (length(title) >= 1)
		) strict;
		create index idx_author on notes (author_id, created_at);
		create index notes_open on notes (done, due);
		create trigger notes_touched after update on notes begin select 1; end;
		create view open_notes as select * from notes where done = 0;
		create index notes_lower_title on notes (lower(title));`)}}
	if _, err := Open(t.Context(), openStore(t, t.TempDir()), "app", spelled, d.schema); err != nil {
		t.Fatalf("a file spelling the schema otherwise: %v", err)
	}
}

func TestOpenRefusesAPartialIndexWithTheDeclaredName(t *testing.T) {
	migrations := fstest.MapFS{"001_notes.sql": {Data: []byte(`
		CREATE TABLE notes (id INTEGER PRIMARY KEY, title TEXT NOT NULL, body TEXT NOT NULL) STRICT;
		CREATE INDEX notes_title ON notes (title) WHERE body <> '';`)}}
	schema := Schema(Table[note]("notes", PrimaryKey("id"), Index("title")))
	_, err := Open(t.Context(), openStore(t, t.TempDir()), "app", migrations, schema)
	if !errors.Is(err, tinystore.ErrInvalid) || !strings.Contains(err.Error(), "the index notes_title") {
		t.Fatalf("partial index accepted as a whole-table index: %v", err)
	}
}

// each difference of structure is one line naming it
func TestOpenNamesEachDifferenceOfStructure(t *testing.T) {
	d := declareDesign(t)
	differing := fstest.MapFS{"001_notes.sql": {Data: []byte(`
		create table users (id integer primary key, email text not null, display_name text,
			created_at integer not null, nickname text) strict;
		create table notes (
			id text not null primary key, author_id integer not null references users (id),
			title text not null, done integer not null default 1, tags text not null default '[]',
			due text, created_at text not null
		);`)}}
	_, err := Open(t.Context(), openStore(t, t.TempDir()), "app", differing, d.schema)
	for _, want := range []string{
		"users.display_name is NOT NULL in the schema and nullable in the file",
		"users.nickname is in the file, and User has no field for it",
		"the index users_email on users is declared, and the file has no such index",
		"notes is STRICT in the schema and not STRICT in the file",
		"notes.created_at is INTEGER in the schema and TEXT in the file",
		"notes.done defaults to 0 in the schema and to 1 in the file",
		"notes.author_id refers to users (id) ON DELETE CASCADE in the schema and to users (id) in the file",
	} {
		if err == nil || !strings.Contains(err.Error(), want) {
			t.Errorf("%v\nwant %q", err, want)
		}
	}
	if err != nil && strings.Contains(err.Error(), "check") {
		t.Errorf("a check was compared: %v", err)
	}
}

// a migration rebuilding a parent table, as SQLite's procedure for a change
// ALTER TABLE cannot make does, keeps the children that refer to it
func TestARebuiltTableKeepsItsChildren(t *testing.T) {
	d := declareDesign(t)
	dir := t.TempDir()
	store := openStore(t, dir)
	db, err := Open(t.Context(), store, "app", designMigrations(t), d.schema)
	if err != nil {
		t.Fatal(err)
	}
	author, err := Insert(t.Context(), db, d.users, User{Email: "a@example.com", Name: "A", CreatedAt: time.Now()})
	if err != nil {
		t.Fatal(err)
	}
	for i := range 100 {
		note := Note{ID: UUID{byte(i)}, AuthorID: author.ID, Title: "note", CreatedAt: time.Now()}
		if _, err = Insert(t.Context(), db, d.notes, note); err != nil {
			t.Fatal(err)
		}
	}
	if err = store.Close(t.Context()); err != nil {
		t.Fatal(err)
	}

	migrations := designMigrations(t)
	migrations["002_rebuild_users.sql"] = &fstest.MapFile{Data: []byte(`
		create table users_new (
			id integer primary key, email text not null, display_name text not null,
			created_at integer not null
		) strict;
		insert into users_new select id, email, display_name, created_at from users;
		drop table users;
		alter table users_new rename to users;
		create unique index users_email on users (email);`)}
	db, err = Open(t.Context(), openStore(t, dir), "app", migrations, d.schema)
	if err != nil {
		t.Fatal(err)
	}
	if children, _ := Scalar[int](t.Context(), db, `select count(*) from notes`); children != 100 {
		t.Fatalf("%d of 100 notes survived their authors' table being rebuilt", children)
	}
}
