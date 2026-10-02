package server

import (
	"context"
	"errors"
	"reflect"
	"slices"
	"testing"
	"testing/fstest"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/server/internal/client"
	"github.com/tinyshed/tinystore/server/wire"
	"github.com/tinyshed/tinystore/sqldb"
)

var notesMigration = wire.SQLMigration{Name: "0001_notes.sql", Text: `create table notes (
	id    integer primary key,
	title text not null,
	score real,
	body  blob
) strict;
create unique index notes_title on notes (title);`}

var tagsMigration = wire.SQLMigration{Name: "0002_tags.sql", Text: `create table tags (tag text not null) strict;`}

func openSQL(t *testing.T, conn *client.Conn, database wire.SQLDatabase) uint64 {
	t.Helper()
	handle, err := openSQLErr(t, conn, database)
	if err != nil {
		t.Fatalf("open %s: %v", database.Name, err)
	}
	return handle
}

func openSQLErr(t *testing.T, conn *client.Conn, database wire.SQLDatabase) (uint64, error) {
	t.Helper()
	body, err := conn.Call(t.Context(), wire.SQLOpen, database)
	if err != nil {
		return 0, err
	}
	var handle wire.Handle
	return handle.Handle, handle.Decode(body)
}

func sqlExecErr(t *testing.T, conn *client.Conn, statement wire.SQLStatement) (wire.SQLDone, error) {
	t.Helper()
	body, err := conn.Call(t.Context(), wire.SQLExec, statement)
	if err != nil {
		return wire.SQLDone{}, err
	}
	var done wire.SQLDone
	return done, done.Decode(body)
}

func mustExec(t *testing.T, conn *client.Conn, statement wire.SQLStatement) wire.SQLDone {
	t.Helper()
	done, err := sqlExecErr(t, conn, statement)
	if err != nil {
		t.Fatalf("exec %q: %v", statement.SQL, err)
	}
	return done
}

// sqlQueryErr downloads a query's columns and rows
func sqlQueryErr(t *testing.T, conn *client.Conn, statement wire.SQLStatement) ([]string, [][]any, error) {
	t.Helper()
	st, err := conn.Open(t.Context(), wire.SQLQuery, statement, true)
	if err != nil {
		return nil, nil, err
	}
	body, err := st.Response(t.Context())
	if err != nil {
		return nil, nil, err
	}
	var columns wire.SQLColumns
	if err = columns.Decode(body); err != nil {
		return nil, nil, err
	}
	rows := [][]any{}
	for {
		body, last, err := st.Next(t.Context())
		if err != nil {
			return columns.Columns, rows, err
		}
		if last {
			return columns.Columns, rows, (&wire.Empty{}).Decode(body)
		}
		var row wire.SQLRow
		if err = row.Decode(body); err != nil {
			return nil, nil, err
		}
		rows = append(rows, row.Values)
	}
}

func mustQuery(t *testing.T, conn *client.Conn, statement wire.SQLStatement) ([]string, [][]any) {
	t.Helper()
	columns, rows, err := sqlQueryErr(t, conn, statement)
	if err != nil {
		t.Fatalf("query %q: %v", statement.SQL, err)
	}
	return columns, rows
}

func sqlBatchErr(t *testing.T, conn *client.Conn, batch wire.SQLStatements) ([]wire.SQLResult, error) {
	t.Helper()
	body, err := conn.Call(t.Context(), wire.SQLBatch, batch)
	if err != nil {
		return nil, err
	}
	var results wire.SQLResults
	return results.Results, results.Decode(body)
}

func failureOf(err error) wire.Error {
	var failure *wire.Error
	if errors.As(err, &failure) {
		return *failure
	}
	return wire.Error{Message: "not a stream's error: " + errorText(err)}
}

func errorText(err error) string {
	if err == nil {
		return "no error"
	}
	return err.Error()
}

func TestSQLOverTheWire(t *testing.T) {
	ts := startTestServer(t, Options{})
	conn := ts.dial(t, wire.Hello{})
	if !slices.Contains(conn.Welcome.Engines, "sql") {
		t.Fatalf("WELCOME serves %v", conn.Welcome.Engines)
	}
	app := openSQL(t, conn, wire.SQLDatabase{Name: "app", Migrations: []wire.SQLMigration{notesMigration}})

	insert := wire.SQLStatement{
		Handle: app, SQL: `insert into notes (title, score, body) values (?, ?, ?)`,
		Args: []any{"first", 1.5, []byte{0, 0xff}},
	}
	if done := mustExec(t, conn, insert); done.Changes != 1 || done.LastID != 1 {
		t.Fatalf("an insert: %+v", done)
	}
	named := wire.SQLStatement{
		Handle: app, SQL: `insert into notes (title, score) values (:title, @score)`,
		Named: map[string]any{"title": "second", "score": true},
	}
	if done := mustExec(t, conn, named); done.LastID != 2 {
		t.Fatalf("an insert of named arguments: %+v", done)
	}

	columns, rows := mustQuery(t, conn, wire.SQLStatement{
		Handle: app,
		SQL:    `select id, title, score, body from notes order by id`,
	})
	want := [][]any{{int64(1), "first", 1.5, []byte{0, 0xff}}, {int64(2), "second", 1.0, nil}}
	if !slices.Equal(columns, []string{"id", "title", "score", "body"}) || !reflect.DeepEqual(rows, want) {
		t.Fatalf("a query: %v %#v", columns, rows)
	}
	_, rows = mustQuery(t, conn, wire.SQLStatement{
		Handle: app, Write: true,
		SQL: `update notes set score = score * 2 where id = ? returning id, score`, Args: []any{int64(1)},
	})
	if !reflect.DeepEqual(rows, [][]any{{int64(1), 3.0}}) {
		t.Fatalf("a write's returning clause: %#v", rows)
	}

	_, err := sqlExecErr(t, conn, wire.SQLStatement{Handle: app, SQL: `insert into notes (title) values ('first')`})
	if failure := failureOf(err); failure.Code != wire.CodeConflict || failure.What["constraint"] != "title" ||
		failure.What["table"] != "notes" {
		t.Fatalf("a title taken: %+v", failure)
	}
	for _, refused := range []wire.SQLStatement{
		{Handle: app, SQL: `insert notes values`},
		{Handle: app, SQL: `select * from nothing`},
		{Handle: app, SQL: `insert into notes (title) values (?)`},
		{Handle: app, SQL: `select 1`, Args: []any{0.0 / zero()}},
		{Handle: 99, SQL: `select 1`},
	} {
		if _, err = sqlExecErr(t, conn, refused); failureOf(err).Code != wire.CodeInvalid {
			t.Errorf("%q %v: %v", refused.SQL, refused.Args, err)
		}
	}
	if _, _, err = sqlQueryErr(t, conn, wire.SQLStatement{Handle: app, SQL: `delete from notes`}); failureOf(err).Code !=
		wire.CodeInvalid {
		t.Errorf("a write through a query not marked write: %v", err)
	}
}

// zero hides a division from the compiler, which refuses a constant NaN
func zero() float64 { return 0 }

// a batch is one transaction: a statement that fails rolls back the ones
// before it, and names itself; a read batch reads from one snapshot
func TestAnSQLBatchIsOneTransaction(t *testing.T) {
	ts := startTestServer(t, Options{})
	conn := ts.dial(t, wire.Hello{})
	app := openSQL(t, conn, wire.SQLDatabase{Name: "app", Migrations: []wire.SQLMigration{notesMigration}})
	mustExec(t, conn, wire.SQLStatement{Handle: app, SQL: `insert into notes (title) values ('taken')`})

	_, err := sqlBatchErr(t, conn, wire.SQLStatements{Handle: app, Statements: []wire.SQLStatement{
		{SQL: `insert into notes (title) values ('kept?')`},
		{SQL: `insert into notes (title) values ('taken')`},
	}})
	if failure := failureOf(err); failure.Code != wire.CodeConflict || failure.What["call"] != "1" {
		t.Fatalf("a batch whose second statement conflicts: %+v", failure)
	}
	if _, rows := mustQuery(t, conn, wire.SQLStatement{Handle: app, SQL: `select count(*) from notes`}); rows[0][0] !=
		int64(1) {
		t.Fatalf("%v notes after a failed batch", rows[0][0])
	}

	results, err := sqlBatchErr(t, conn, wire.SQLStatements{Handle: app, Statements: []wire.SQLStatement{
		{SQL: `insert into notes (title) values (?) returning id, title`, Args: []any{"new"}, Rows: true},
		{SQL: `update notes set score = 1`},
	}})
	if err != nil || len(results) != 2 || !reflect.DeepEqual(results[0].Rows, [][]any{{int64(2), "new"}}) ||
		results[1].Columns != nil || results[1].Changes != 2 {
		t.Fatalf("a batch's results: %+v, %v", results, err)
	}

	results, err = sqlBatchErr(t, conn, wire.SQLStatements{Handle: app, Read: true, Statements: []wire.SQLStatement{
		{SQL: `select count(*) from notes`},
		{SQL: `select title from notes order by id`},
	}})
	if err != nil || !reflect.DeepEqual(results[0].Rows, [][]any{{int64(2)}}) ||
		!reflect.DeepEqual(results[1].Rows, [][]any{{"taken"}, {"new"}}) {
		t.Fatalf("a read batch: %+v, %v", results, err)
	}
	_, err = sqlBatchErr(t, conn, wire.SQLStatements{Handle: app, Read: true, Statements: []wire.SQLStatement{
		{SQL: `select 1`}, {SQL: `delete from notes`},
	}})
	if failure := failureOf(err); failure.Code != wire.CodeInvalid || failure.What["call"] != "1" {
		t.Fatalf("a write in a read batch: %+v", failure)
	}
}

// the first open of a database opens it, an admin's applying its migrations
// and a data connection's finding them applied; every later one checks
func TestSQLOpenAppliesOnceAndChecksAfter(t *testing.T) {
	admin, data := newToken(t), newToken(t)
	tokens, err := ParseTokens([]byte("admin " + admin + "\ndata " + data))
	if err != nil {
		t.Fatal(err)
	}
	ts := startTestServer(t, Options{Tokens: tokens})
	local := ts.dial(t, wire.Hello{})
	remote := ts.dialAt(t, ts.remote, wire.Hello{Token: data})
	notes := []wire.SQLMigration{notesMigration}
	grown := []wire.SQLMigration{notesMigration, tagsMigration}
	edited := []wire.SQLMigration{{Name: notesMigration.Name, Text: "create table notes (id integer) strict;"}}

	for _, c := range []struct {
		name     string
		conn     *client.Conn
		database wire.SQLDatabase
		code     wire.Code
	}{
		{
			"a data connection first, which makes no file", remote,
			wire.SQLDatabase{Name: "app", Migrations: notes},
			wire.CodePermission,
		},
		{"a first open without migrations", local, wire.SQLDatabase{Name: "app"}, wire.CodeInvalid},
		{"an admin's first open, which applies them", local, wire.SQLDatabase{Name: "app", Migrations: notes}, ""},
		{"a data connection's, which finds them", remote, wire.SQLDatabase{Name: "app", Migrations: notes}, ""},
		{"a later open without migrations", remote, wire.SQLDatabase{Name: "app"}, ""},
		{
			"a data connection with a migration more", remote,
			wire.SQLDatabase{Name: "app", Migrations: grown},
			wire.CodePermission,
		},
		{
			"an admin with a migration more, while it is open", local,
			wire.SQLDatabase{Name: "app", Migrations: grown},
			wire.CodeInUse,
		},
		{
			"a migration changed after it was applied", local,
			wire.SQLDatabase{Name: "app", Migrations: edited},
			wire.CodeInvalid,
		},
		{"a name no database has", local, wire.SQLDatabase{Name: "No Name", Migrations: notes}, wire.CodeInvalid},
		{"a migration in a directory", local, wire.SQLDatabase{Name: "other", Migrations: []wire.SQLMigration{
			{Name: "migrations/0001.sql", Text: notesMigration.Text},
		}}, wire.CodeInvalid},
		{"two migrations of one name", local, wire.SQLDatabase{Name: "other", Migrations: []wire.SQLMigration{
			notesMigration, notesMigration,
		}}, wire.CodeInvalid},
	} {
		_, err := openSQLErr(t, c.conn, c.database)
		if got := failureOf(err).Code; err != nil && got != c.code || err == nil && c.code != "" {
			t.Errorf("%s: %v", c.name, err)
		}
	}
	if _, err := openSQLErr(t, remote, wire.SQLDatabase{Name: "app", Migrations: notes}); err != nil {
		t.Fatal(err)
	}
}

// a database the program opened is checked, not opened again, and both see
// each other's rows
func TestADatabaseTheProgramOpenedIsChecked(t *testing.T) {
	root := t.TempDir()
	store, err := tinystore.Open(t.Context(), root, tinystore.Options{Manual: true})
	if err != nil {
		t.Fatal(err)
	}
	migrations := fstest.MapFS{notesMigration.Name: {Data: []byte(notesMigration.Text)}}
	db, err := sqldb.Open(t.Context(), store, "app", migrations, nil)
	if err != nil {
		t.Fatal(err)
	}
	if _, err = db.Exec(t.Context(), `insert into notes (title) values ('from go')`); err != nil {
		t.Fatal(err)
	}
	ts := serveTestStore(t, root, store, Options{SQL: map[string]*sqldb.DB{"app": db}})
	conn := ts.dial(t, wire.Hello{})

	app := openSQL(t, conn, wire.SQLDatabase{Name: "app", Migrations: []wire.SQLMigration{notesMigration}})
	if _, rows := mustQuery(t, conn, wire.SQLStatement{Handle: app, SQL: `select title from notes`}); len(rows) != 1 ||
		rows[0][0] != "from go" {
		t.Fatalf("the program's row over the wire: %v", rows)
	}
	grown := wire.SQLDatabase{Name: "app", Migrations: []wire.SQLMigration{notesMigration, tagsMigration}}
	if _, err = openSQLErr(t, conn, grown); failureOf(err).Code != wire.CodeInUse {
		t.Fatalf("a migration more for the program's database: %v", err)
	}
}

// a data connection's statement ends at its deadline, since a recursive query
// can hold a reader or the writer for good
func TestADataStatementEndsAtItsDeadline(t *testing.T) {
	data := newToken(t)
	tokens, err := ParseTokens([]byte("data " + data))
	if err != nil {
		t.Fatal(err)
	}
	ts := startTestServer(t, Options{Tokens: tokens}, func(l *limits) { l.statement = 200 * time.Millisecond })
	openSQL(t, ts.dial(t, wire.Hello{}), wire.SQLDatabase{Name: "app", Migrations: []wire.SQLMigration{notesMigration}})
	remote := ts.dialAt(t, ts.remote, wire.Hello{Token: data})
	app := openSQL(t, remote, wire.SQLDatabase{Name: "app"})

	forever := `with recursive c(x) as (select 1 union all select x + 1 from c) select count(*) from c`
	start := time.Now()
	for _, run := range []func() error{
		func() error {
			_, _, err := sqlQueryErr(t, remote, wire.SQLStatement{Handle: app, SQL: forever})
			return err
		},
		func() error {
			_, err := sqlExecErr(t, remote, wire.SQLStatement{
				Handle: app,
				SQL:    `insert into notes (title) ` + forever + ` limit 1`,
			})
			return err
		},
	} {
		if err := run(); failureOf(err).Code != wire.CodeLimit {
			t.Errorf("a statement that never ends: %v", err)
		}
	}
	if took := time.Since(start); took > 10*time.Second {
		t.Errorf("two statements past their deadline took %s", took)
	}
	if _, rows := mustQuery(t, remote, wire.SQLStatement{Handle: app, SQL: `select count(*) from notes`}); rows[0][0] !=
		int64(0) {
		t.Errorf("%v notes after a write stopped at its deadline", rows[0][0])
	}
}

// an answer past the body the client takes fails its stream with limit, and
// the connection goes on
func TestAnAnswerPastTheBodyIsALimit(t *testing.T) {
	ts := startTestServer(t, Options{})
	conn := ts.dial(t, wire.Hello{})
	app := openSQL(t, conn, wire.SQLDatabase{Name: "app", Migrations: []wire.SQLMigration{notesMigration}})
	mustExec(t, conn, wire.SQLStatement{
		Handle: app, SQL: `insert into notes (title, body) values ('big', zeroblob(?))`,
		Args: []any{int64(conn.Welcome.MaxBody)},
	})

	big := wire.SQLStatement{Handle: app, SQL: `select title, body from notes`}
	if _, _, err := sqlQueryErr(t, conn, big); failureOf(err).Code != wire.CodeLimit {
		t.Errorf("a row past the body: %v", err)
	}
	if _, err := sqlBatchErr(t, conn, wire.SQLStatements{
		Handle: app, Read: true,
		Statements: []wire.SQLStatement{big},
	}); failureOf(err).Code != wire.CodeLimit {
		t.Errorf("results past the body: %v", err)
	}
	if _, rows := mustQuery(t, conn, wire.SQLStatement{Handle: app, SQL: `select length(body) from notes`}); rows[0][0] !=
		int64(conn.Welcome.MaxBody) {
		t.Errorf("after the limits: %v", rows)
	}
}

// a value travels as its row keeps it, whatever its column declares or its
// text looks like: no time is read into text, no flag into an integer
func TestAValueTravelsAsItsRowKeepsIt(t *testing.T) {
	ts := startTestServer(t, Options{})
	conn := ts.dial(t, wire.Hello{})
	app := openSQL(t, conn, wire.SQLDatabase{Name: "app", Migrations: []wire.SQLMigration{
		{Name: "0001_events.sql", Text: `create table events (at datetime, done boolean);`},
	}})
	mustExec(t, conn, wire.SQLStatement{Handle: app, SQL: `insert into events values
		('2024-01-02', 1), ('2026-10-02T10:00:00.123Z', 0)`})
	_, rows := mustQuery(t, conn, wire.SQLStatement{Handle: app, SQL: `select at, done from events order by rowid`})
	want := [][]any{{"2024-01-02", int64(1)}, {"2026-10-02T10:00:00.123Z", int64(0)}}
	if !reflect.DeepEqual(rows, want) {
		t.Fatalf("a time and a flag: %#v", rows)
	}
}

// a download whose client lets go of its stream still ends it once
func TestAnSQLQueryCancelledEndsItsStream(t *testing.T) {
	ts := startTestServer(t, Options{})
	conn := ts.dial(t, wire.Hello{StreamCredit: 64 << 10})
	app := openSQL(t, conn, wire.SQLDatabase{Name: "app", Migrations: []wire.SQLMigration{notesMigration}})
	mustExec(t, conn, wire.SQLStatement{Handle: app, SQL: `insert into notes (title, body)
		select 'n' || value, zeroblob(10000) from json_each(json_array(1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20))`})
	st, err := conn.Open(t.Context(), wire.SQLQuery, wire.SQLStatement{Handle: app, SQL: `select * from notes`}, true)
	if err != nil {
		t.Fatal(err)
	}
	if _, err = st.Response(t.Context()); err != nil {
		t.Fatal(err)
	}
	if err = st.Cancel(); err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithTimeout(t.Context(), 10*time.Second)
	defer cancel()
	for {
		_, last, err := st.Next(ctx)
		if err != nil || last {
			if code := failureOf(err).Code; err != nil && code != wire.CodeCancelled {
				t.Fatalf("a cancelled download ended with %v", err)
			}
			break
		}
	}
	mustQuery(t, conn, wire.SQLStatement{Handle: app, SQL: `select 1`})
}
