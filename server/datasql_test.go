package server

import (
	"context"
	"errors"
	"fmt"
	"io/fs"
	"path/filepath"
	"slices"
	"strings"
	"testing"
	"testing/fstest"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/server/wire"
	"github.com/tinyshed/tinystore/sqldb"
)

// The database the adversarial round attacks. It has a table with an index, a
// trigger, full-text search, an autoincrement's sequence, and the history every
// sqldb file keeps.
const scratchSchema = `create table notes (id integer primary key, title text not null, score real) strict;
create index notes_title on notes (title);
create table log (note_id integer not null) strict;
create trigger notes_log after insert on notes begin insert into log values (new.id); end;
create virtual table docs using fts5 (body);
create table counters (id integer primary key autoincrement, name text);`

// planVerdict is what the second line says of a statement on its own
type planVerdict uint8

const (
	planPasses  planVerdict = iota // it compiles to a program the line lets run
	planRefuses                    // its program is refused
	planFails                      // SQLite refuses to compile it
	planUnasked                    // the first line never hands it on: compiling it could change the reader
)

// attempts are the adversarial round: statements a data connection might
// send, each with what the first line and the second line alone say of it
var attempts = []struct {
	name   string
	sql    string
	lexer  bool // the first line refuses it
	plan   planVerdict
	change bool // it changes rows, which a data connection may
}{
	{"a query", `select id, title from notes where id = 1`, false, planPasses, false},
	{"an insert returning its id", `insert into notes (title) values ('x') returning id`, false, planPasses, true},
	{
		"a recursive query", `with recursive c(x) as (select 1 union all select x + 1 from c where x < 5) select x from c`,
		false, planPasses, false,
	},
	{"values", `values (1, 'a'), (2, 'b')`, false, planPasses, false},
	{"a replace", `replace into notes (id, title) values (1, 'replaced')`, false, planPasses, true},
	{"a delete of every row", `delete from notes`, false, planPasses, true},
	{"a full-text insert", `insert into docs (body) values ('hello world')`, false, planPasses, true},
	{"a full-text query", `select rowid from docs where docs match 'hello'`, false, planPasses, false},
	{"an autoincrement's sequence", `insert into counters (name) values ('c')`, false, planPasses, true},
	{"its sequence, emptied", `delete from sqlite_sequence`, false, planPasses, true},
	{"a trigger the schema holds", `insert into notes (title) values ('logged')`, false, planPasses, true},
	{"the migration history, read", `select version, name from _tinystore_migrations`, false, planPasses, false},
	{"the schema, read", `select name, sql from sqlite_schema`, false, planPasses, false},
	{"a semicolon in a string", `select ';'`, false, planPasses, false},
	{"a semicolon ending the statement, then a comment", "select 1; -- done", false, planPasses, false},
	{"a line comment runs past a carriage return", "select 1 -- x\r; drop table notes", false, planPasses, false},
	{"a bracketed name holding a semicolon", `select 1 as [a;b]`, false, planPasses, false},
	{"a Tcl parameter holding a semicolon", `select $a(;)`, false, planFails, false},
	{"an extension loaded", `select load_extension('evil')`, false, planFails, false}, // the store's SQLite has none

	{"a second statement", `select 1; drop table notes`, true, planUnasked, false},
	{"a second statement after a comment", `select 1 /* ; */ ; drop table notes`, true, planUnasked, false},
	{"a second statement after a doubled quote", `select 'it''s'; drop table notes`, true, planUnasked, false},
	{"comments do not nest", `select 1 /* /* */ ; drop table notes */`, true, planUnasked, false},
	{"a comment left open", `select 1 /* ; drop table notes`, true, planUnasked, false},
	{"a string left open", `select 'x; drop table notes`, true, planUnasked, false},
	{"a second statement past a line comment", "select 1 -- ;\n; drop table notes", true, planUnasked, false},
	{"a Tcl parameter hiding a quote", `select $a(') ; drop table notes ; --'`, true, planUnasked, false},
	{"a backquoted name with a doubled quote", "select 1 as `a``;` ; drop table notes", true, planUnasked, false},
	{"a bracket ends at its first ]", `select 1 as [a]]; drop table notes`, true, planUnasked, false},
	{"a NUL byte", "select 1\x00; drop table notes", true, planUnasked, false},
	{"a no-break space is part of a name", "select\u00a01; drop table notes", true, planUnasked, false},
	{"a line separator is no whitespace", "select 1;\u2028drop table notes", true, planUnasked, false},
	{"a byte order mark is whitespace", "\ufeffdrop table notes", true, planRefuses, false},
	{"a long s is no s", "ſelect 1", true, planFails, false},
	{"no statement", " -- nothing", true, planUnasked, false},

	{"a table dropped", `drop table notes`, true, planRefuses, false},
	{"a table made", `create table x (a)`, true, planRefuses, false},
	{"an index made", `create index notes_score on notes (score)`, true, planRefuses, false},
	{"a trigger made", `create trigger spy after delete on notes begin select 1; end`, true, planRefuses, false},
	{"a view made", `create view v as select 1`, true, planRefuses, false},
	{"a column added", `alter table notes add column x`, true, planRefuses, false},
	{"a table renamed", `alter table notes rename to n`, true, planRefuses, false},
	{"a virtual table made", `create virtual table y using fts5 (a)`, true, planRefuses, false},
	{"a virtual table dropped", `drop table docs`, true, planRefuses, false},
	{"a file attached", `attach 'attached.db' as attached`, true, planRefuses, false},
	{"a file detached", `detach main`, true, planRefuses, false},
	{"an attach inside a with", `with x as (select 1) attach 'attached.db' as attached`, false, planFails, false},
	{"a vacuum", `vacuum`, true, planRefuses, false},
	{"a vacuum into a file", `vacuum into 'vacuumed.db'`, true, planRefuses, false},
	{"a transaction begun", `begin`, true, planRefuses, false},
	{"a transaction committed", `commit`, true, planRefuses, false},
	{"a transaction rolled back", `rollback`, true, planRefuses, false},
	{"a savepoint", `savepoint s`, true, planRefuses, false},
	{"a savepoint released", `release s`, true, planRefuses, false},
	{"an analysis", `analyze`, true, planRefuses, false},
	{"a reindex", `reindex`, true, planPasses, false},
	{"an explain", `explain select 1`, true, planFails, false},
	{"the schema made writable", `pragma writable_schema = on`, true, planUnasked, false},
	{"the journal changed", `pragma journal_mode = delete`, true, planUnasked, false},
	{"foreign keys turned off", `pragma foreign_keys = off`, true, planUnasked, false},
	{"the user version set", `pragma user_version = 7`, true, planUnasked, false},
	{"the reader made a writer", `pragma query_only = 0`, true, planUnasked, false},

	{"an optimize that analyzes", `select * from pragma_optimize`, true, planRefuses, false},
	{"an optimize with its mask", `select * from pragma_optimize(0x10002)`, true, planRefuses, false},
	{"an optimize named by a string", `select * from 'pragma_optimize'`, true, planRefuses, false},
	{"an optimize in capitals, quoted", `select * from "PRAGMA_OPTIMIZE"`, true, planRefuses, false},
	{"an optimize in brackets", `select * from [pragma_optimize]`, true, planRefuses, false},
	{"an optimize of main", `select * from main.pragma_optimize`, true, planRefuses, false},
	{"an optimize inside an insert", `insert into log select 1 from pragma_optimize`, true, planRefuses, false},
	{"a pragma's table, read", `select * from pragma_table_info('notes')`, true, planPasses, false},
	{"the file's pages, read", `select * from sqlite_dbpage`, true, planFails, false},
	{
		"the schema's page overwritten", `update sqlite_dbpage set data = zeroblob(4096) where pgno = 1`, true,
		planFails, false,
	},
	{"a page inserted", `insert into sqlite_dbpage values (1, zeroblob(4096))`, true, planFails, false},
	{
		"the pages named by a string", `update 'sqlite_dbpage' set data = data where pgno = 1`, true, planFails,
		false,
	},
	{"the pages of temp", `update temp.sqlite_dbpage set data = data where pgno = 1`, true, planFails, false},
	{"the pages of a schema argument", `select * from sqlite_dbpage('main')`, true, planFails, false},

	{"the history written", `insert into _tinystore_migrations values (9, 'x', x'00')`, false, planRefuses, false},
	{"the history cleared", `delete from _tinystore_migrations`, false, planRefuses, false},
	{"the history edited", `update _tinystore_migrations set name = 'x'`, false, planRefuses, false},
	{"the history replaced", `replace into _tinystore_migrations values (1, 'x', x'00')`, false, planRefuses, false},
	{
		"the history deleted from a with", `with h as (select 1) delete from _tinystore_migrations where version in h`,
		false, planRefuses, false,
	},
	{
		"the schema inserted into", `insert into sqlite_schema values ('table', 'x', 'x', 0, 'create table x (a)')`,
		false, planFails, false,
	},
	{"the schema updated", `update sqlite_master set sql = 'x'`, false, planFails, false},
	{"the schema cleared", `delete from sqlite_schema`, false, planFails, false},
}

// openScratch is the attacked database in a store of its own
func openScratch(t testing.TB) (*sqldb.DB, string) {
	t.Helper()
	root := t.TempDir()
	store, err := tinystore.Open(context.Background(), root, tinystore.Options{Manual: true})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if closeErr := store.Close(context.Background()); closeErr != nil {
			t.Error(closeErr)
		}
	})
	db, err := sqldb.Open(context.Background(), store, "app",
		fstest.MapFS{"0001_app.sql": {Data: []byte(scratchSchema)}}, nil)
	if err != nil {
		t.Fatal(err)
	}
	return db, root
}

// each line refuses what it is there to refuse, on its own, so that neither
// rests on the other where both can see
func TestEachLineOfTheCheckRefusesOnItsOwn(t *testing.T) {
	db, _ := openScratch(t)
	ctx := t.Context()
	for _, attempt := range attempts {
		lexed := readDataSQL(attempt.sql)
		if refused := errors.Is(lexed, errDataConnection); refused != attempt.lexer {
			t.Errorf("%s: the first line says %v", attempt.name, lexed)
		}
		if attempt.plan == planUnasked {
			continue
		}
		planned := explainDataSQL(ctx, db, attempt.sql, nil)
		var verdict planVerdict
		switch {
		case errors.Is(planned, errDataConnection):
			verdict = planRefuses
		case planned != nil:
			verdict = planFails
		}
		if verdict != attempt.plan {
			t.Errorf("%s: the second line says %v", attempt.name, planned)
		}
	}
}

// a data connection's statements change rows and never the schema: the
// adversarial round over the wire, each attempt as exec, query and batch
func TestADataClientCannotChangeTheSchema(t *testing.T) {
	data := newToken(t)
	tokens, err := ParseTokens([]byte("data " + data))
	if err != nil {
		t.Fatal(err)
	}
	ts := startTestServer(t, Options{Tokens: tokens})
	t.Chdir(ts.root)
	admin := ts.dial(t, wire.Hello{})
	openSQL(t, admin, wire.SQLDatabase{Name: "app", Migrations: []wire.SQLMigration{
		{Name: "0001_app.sql", Text: scratchSchema},
	}})
	remote := ts.dialAt(t, ts.remote, wire.Hello{Token: data})
	app := openSQL(t, remote, wire.SQLDatabase{Name: "app"})
	ts.server.sqlOpening.Lock()
	db := ts.server.databases["app"]
	ts.server.sqlOpening.Unlock()
	before := guardedState(t, db, ts.root)

	for _, attempt := range attempts {
		statement := wire.SQLStatement{Handle: app, SQL: attempt.sql}
		_, execErr := sqlExecErr(t, remote, statement)
		_, _, queryErr := sqlQueryErr(t, remote, wire.SQLStatement{Handle: app, SQL: attempt.sql, Write: true})
		_, batchErr := sqlBatchErr(t, remote, wire.SQLStatements{Handle: app, Statements: []wire.SQLStatement{
			{SQL: `select 1`}, {SQL: attempt.sql},
		}})
		refused := attempt.lexer || attempt.plan == planRefuses
		for _, err := range []error{execErr, queryErr, batchErr} {
			if code := failureOf(err).Code; refused && code != wire.CodePermission ||
				!refused && code == wire.CodePermission {
				t.Errorf("%s: %v", attempt.name, err)
			}
		}
		if after := guardedState(t, db, ts.root); after != before {
			t.Fatalf("%s changed what a data connection may not:\n%s", attempt.name, changed(before, after))
		}
	}
}

// FuzzDataSQL runs whatever the check lets through as a data connection's
// exec would, and fails when the schema, the migration history, the writer's
// settings, the databases it attached or the files beside the database
// changed. go test runs the adversarial round as its seeds.
func FuzzDataSQL(f *testing.F) {
	for _, attempt := range attempts {
		f.Add(attempt.sql)
	}
	db, root := openScratch(f)
	f.Chdir(root)
	before := guardedState(f, db, root)
	f.Fuzz(func(t *testing.T, statement string) {
		ctx, cancel := context.WithTimeout(context.Background(), time.Second)
		defer cancel()
		if checkDataSQL(ctx, db, statement, nil) != nil {
			return
		}
		_, _ = db.Exec(ctx, statement)
		if after := guardedState(t, db, root); after != before {
			t.Fatalf("%q changed what a data connection may not:\n%s", statement, changed(before, after))
		}
	})
}

// the writer's settings a data connection may not change, per connection or
// in the file
var guardedPragmas = []string{
	"foreign_keys", "journal_mode", "synchronous", "locking_mode", "query_only", "writable_schema",
	"ignore_check_constraints", "trusted_schema", "recursive_triggers", "defer_foreign_keys", "cell_size_check",
	"secure_delete", "temp_store", "cache_size", "busy_timeout", "automatic_index", "reverse_unordered_selects",
	"legacy_alter_table", "user_version", "application_id", "schema_version", "auto_vacuum", "page_size",
	"max_page_count", "journal_size_limit", "wal_autocheckpoint", "mmap_size", "analysis_limit", "database_list",
}

// guardedState is, as text, what no data connection's statement may change. It
// holds the schema, the migration history, the writer's settings and the
// databases it attached, all read on the writer, and the files in the store's
// directory.
func guardedState(t testing.TB, db *sqldb.DB, root string) string {
	t.Helper()
	var state strings.Builder
	queries := []string{
		`select type, name, tbl_name, rootpage, sql from sqlite_schema order by type, name`,
		`select version, name, hex(checksum) from _tinystore_migrations order by version`,
	}
	for _, pragma := range guardedPragmas {
		queries = append(queries, "pragma "+pragma)
	}
	for _, query := range queries {
		rows, err := sqldb.ExecQuery(context.Background(), db, query)
		if err != nil {
			t.Fatalf("%s: %v", query, err)
		}
		fmt.Fprintf(&state, "%s: %v\n", query, rows.Values)
	}
	err := filepath.WalkDir(root, func(path string, entry fs.DirEntry, err error) error {
		if err == nil && !strings.HasSuffix(path, "-wal") && !strings.HasSuffix(path, "-shm") {
			fmt.Fprintf(&state, "file %s\n", filepath.ToSlash(strings.TrimPrefix(path, root)))
		}
		return err
	})
	if err != nil {
		t.Fatal(err)
	}
	return state.String()
}

// changed is the lines of after that before does not hold
func changed(before, after string) string {
	was := strings.Split(before, "\n")
	var lines []string
	for line := range strings.SplitSeq(after, "\n") {
		if !slices.Contains(was, line) {
			lines = append(lines, "+ "+line)
		}
	}
	return strings.Join(lines, "\n")
}
