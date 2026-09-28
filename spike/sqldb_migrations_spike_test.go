package spike

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"net/url"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// sqldbWriter is one writing connection to path, opened as internal/sqlite
// opens its writer: foreign keys on, full sync, immediate transactions
func sqldbWriter(t *testing.T, path string) *sql.Conn {
	t.Helper()
	abs, err := filepath.Abs(path)
	if err != nil {
		t.Fatal(err)
	}
	arguments := url.Values{"mode": {"rwc"}, "_txlock": {"immediate"}}
	for _, pragma := range []string{"foreign_keys(1)", "busy_timeout(5000)", "synchronous(FULL)", "journal_mode(WAL)"} {
		arguments.Add("_pragma", pragma)
	}
	uri := &url.URL{Scheme: "file", Path: filepath.ToSlash(abs), RawQuery: arguments.Encode()}
	if !strings.HasPrefix(uri.Path, "/") {
		uri.Path = "/" + uri.Path
	}
	db, err := sql.Open("sqlite", uri.String())
	if err != nil {
		t.Fatal(err)
	}
	db.SetMaxOpenConns(1)
	t.Cleanup(func() { _ = db.Close() })
	conn, err := db.Conn(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = conn.Close() })
	return conn
}

// the rebuild SQLite's documentation gives for a change ALTER TABLE cannot
// make: a new table, the rows copied, the old one dropped, the new one renamed
const sqldbRebuildAuthors = `
	create table authors_new (id integer primary key, name text not null, born integer) strict;
	insert into authors_new (id, name) select id, name from authors;
	drop table authors;
	alter table authors_new rename to authors;`

// TestSQLDBMigrationRebuild rebuilds a parent table under 100,000 children
// that reference it with on delete cascade, inside one transaction, three
// ways: foreign keys on, as a migration runs today; on, with their checks
// deferred to the commit; and off for the migration, with foreign_key_check
// before the commit, as SQLite's procedure asks. It reports the children left
// and what each took
func TestSQLDBMigrationRebuild(t *testing.T) {
	sqldbMeasuring(t)
	const authors, notes = 1000, 100_000
	for _, way := range []struct {
		name          string
		before, after string // on the connection, outside the transaction
		inside        string // first thing inside the transaction
	}{
		{"foreign keys on", "", "", ""},
		{"on, checks deferred", "", "", "pragma defer_foreign_keys = on"},
		{"off, checked before commit", "pragma foreign_keys = off", "pragma foreign_keys = on", ""},
	} {
		ctx := t.Context()
		conn := sqldbWriter(t, filepath.Join(sqldbDir(t), "app.db"))
		sqldbFill(t, conn, authors, notes)

		began := time.Now()
		if way.before != "" {
			if _, err := conn.ExecContext(ctx, way.before); err != nil {
				t.Fatal(err)
			}
		}
		err := sqldbMigrate(ctx, conn, way.inside, sqldbRebuildAuthors)
		if way.after != "" {
			err = errors.Join(err, sqldbExec(ctx, conn, way.after))
		}
		elapsed := time.Since(began)
		if err != nil {
			t.Fatalf("%s: %v", way.name, err)
		}
		var left int
		if err = conn.QueryRowContext(ctx, `select count(*) from notes`).Scan(&left); err != nil {
			t.Fatal(err)
		}
		var references string
		if err = conn.QueryRowContext(ctx, `select sql from sqlite_schema where name = 'notes'`).Scan(&references); err != nil {
			t.Fatal(err)
		}
		t.Logf("%-28s %6d of %d children left, in %v; notes still says %q", way.name, left, notes,
			elapsed.Round(time.Millisecond), strings.Join(strings.Fields(references[strings.Index(references, "references"):]), " "))
	}
}

func sqldbFill(t *testing.T, conn *sql.Conn, authors, notes int) {
	t.Helper()
	ctx := t.Context()
	err := sqldbMigrate(ctx, conn, "", `
		create table authors (id integer primary key, name text not null) strict;
		create table notes (
			id        integer primary key,
			author_id integer not null references authors (id) on delete cascade,
			title     text not null
		) strict;
		create index notes_author_id on notes (author_id);`)
	if err != nil {
		t.Fatal(err)
	}
	tx, err := conn.BeginTx(ctx, nil)
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = tx.Rollback() }()
	for id := range authors {
		if _, err = tx.ExecContext(ctx, `insert into authors (id, name) values (?, ?)`, id, fmt.Sprint("author ", id)); err != nil {
			t.Fatal(err)
		}
	}
	for id := range notes {
		if _, err = tx.ExecContext(ctx, `insert into notes (author_id, title) values (?, ?)`, id%authors, "a note"); err != nil {
			t.Fatal(err)
		}
	}
	if err = tx.Commit(); err != nil {
		t.Fatal(err)
	}
}

// sqldbMigrate runs a script in one transaction, first setting what inside
// asks, and refuses the commit while foreign_key_check finds a broken row
func sqldbMigrate(ctx context.Context, conn *sql.Conn, inside, script string) (err error) {
	tx, err := conn.BeginTx(ctx, nil)
	if err != nil {
		return err
	}
	defer func() {
		if err != nil {
			err = errors.Join(err, tx.Rollback())
		}
	}()
	if inside != "" {
		if _, err = tx.ExecContext(ctx, inside); err != nil {
			return err
		}
	}
	if _, err = tx.ExecContext(ctx, script); err != nil {
		return err
	}
	var broken int
	if err = tx.QueryRowContext(ctx, `select count(*) from pragma_foreign_key_check`).Scan(&broken); err != nil {
		return err
	}
	if broken > 0 {
		return fmt.Errorf("%d rows break a foreign key", broken)
	}
	return tx.Commit()
}

func sqldbExec(ctx context.Context, conn *sql.Conn, query string) error {
	_, err := conn.ExecContext(ctx, query)
	return err
}

// TestSQLDBExpressionText shows what SQLite keeps of a table's checks and
// defaults, and what its pragmas report, as a table is made, gains a column,
// has a column renamed and is renamed itself: what Open and the test could
// compare structurally and what only as text
func TestSQLDBExpressionText(t *testing.T) {
	sqldbMeasuring(t)
	ctx := t.Context()
	conn := sqldbWriter(t, filepath.Join(sqldbDir(t), "app.db"))
	show := func(step, table string) {
		var text string
		if err := conn.QueryRowContext(ctx, `select sql from sqlite_schema where name = ?`, table).Scan(&text); err != nil {
			t.Fatal(err)
		}
		t.Logf("%s — sqlite_schema keeps:\n%s", step, text)
		rows, err := conn.QueryContext(ctx, `select name, type, "notnull", dflt_value, pk from pragma_table_xinfo(?)`, table)
		if err != nil {
			t.Fatal(err)
		}
		defer func() { _ = rows.Close() }()
		for rows.Next() {
			var (
				name, kind string
				notNull    bool
				pk         int
				dflt       sql.NullString
			)
			if err = rows.Scan(&name, &kind, &notNull, &dflt, &pk); err != nil {
				t.Fatal(err)
			}
			t.Logf("  table_xinfo %-10s %-8s not null %-5v default %-30q pk %d", name, kind, notNull, dflt.String, pk)
		}
		if err = rows.Err(); err != nil {
			t.Fatal(err)
		}
	}
	for _, step := range []struct{ name, statement, table string }{
		{"made", `create table t (
			id     integer primary key,
			done   integer not null default 0 check (done in (0, 1)),
			status text not null default 'draft',
			code   text not null default (lower(hex(randomblob(4)))),
			check (length(status) > 0)
		) strict`, "t"},
		{"a column added", `alter table t add column tags text not null default '[]' check (json_valid(tags))`, "t"},
		{"a column renamed", `alter table t rename column done to finished`, "t"},
		{"the table renamed", `alter table t rename to u`, "u"},
		{"a check spelled otherwise, in a new table", `create table v (
			finished integer not null default 0 CHECK ( finished IN(0,1) )
		) strict`, "v"},
	} {
		if err := sqldbExec(ctx, conn, step.statement); err != nil {
			t.Fatalf("%s: %v", step.name, err)
		}
		show(step.name, step.table)
	}
	for _, pragma := range []string{`pragma_index_list('u')`, `pragma_foreign_key_list('u')`} {
		columns, err := sqldbPragmaColumns(ctx, conn, pragma)
		if err != nil {
			t.Fatal(err)
		}
		t.Logf("%s reports columns %v", pragma, columns)
	}
}

// sqldbPragmaColumns is the columns a table-valued pragma reports
func sqldbPragmaColumns(ctx context.Context, conn *sql.Conn, pragma string) (columns []string, err error) {
	rows, err := conn.QueryContext(ctx, `select * from `+pragma)
	if err != nil {
		return nil, err
	}
	defer func() { err = errors.Join(err, rows.Close()) }()
	if columns, err = rows.Columns(); err != nil {
		return nil, err
	}
	return columns, rows.Err()
}
