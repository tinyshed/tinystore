// Package catalog reads what a SQLite file says its tables are, through its
// pragmas and the text sqlite_schema keeps, and says how two files differ: in
// structure, which sqldb's Open checks, and in the text of expressions, which
// a test reports. It knows SQLite, not Go.
package catalog

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"slices"
	"strings"

	_ "modernc.org/sqlite" // Declare opens a database in memory
)

// Reader is where a catalog is read from: a connection, or a pool of them.
type Reader interface {
	QueryContext(ctx context.Context, query string, args ...any) (*sql.Rows, error)
}

// Catalog is the tables of one file, in the order they were made.
type Catalog struct {
	Tables []*Table
}

type Table struct {
	Name         string
	Strict       bool
	WithoutRowID bool
	Columns      []*Column
	PrimaryKey   []string
	References   []Reference
	Indexes      []*Index
	Checks       []string // every CHECK's expression as written, a column's and the table's
	Triggers     []string // the CREATE TRIGGER text of each trigger on it
	SQL          string   // the CREATE TABLE text sqlite_schema keeps
}

type Column struct {
	Name       string
	Type       string // as declared, upper case
	NotNull    bool
	Default    string // its text, an expression without its outer parentheses; "" for none
	Definition string // what CREATE TABLE says of it, its name first
}

type Reference struct {
	From     []string
	Table    string
	To       []string // empty when it names its parent's key without its columns
	OnDelete string
	OnUpdate string
}

type Index struct {
	Name    string
	Columns []string // "" stands for an expression
	Unique  bool
	Origin  string // c for CREATE INDEX, u for a UNIQUE constraint, pk for the primary key
	Partial bool
	SQL     string // empty for an index a constraint made
}

// Table is the table named name, in any case, or nil.
func (c *Catalog) Table(name string) *Table {
	for _, t := range c.Tables {
		if strings.EqualFold(t.Name, name) {
			return t
		}
	}
	return nil
}

func (t *Table) Column(name string) *Column {
	for _, c := range t.Columns {
		if strings.EqualFold(c.Name, name) {
			return c
		}
	}
	return nil
}

// Plain is an index on columns alone, as a schema declares them; an index on
// an expression or of a part of the rows is its migrations' own
func (ix *Index) Plain() bool {
	return !ix.Partial && !containsFold(ix.Columns, "")
}

const (
	tablesQuery = `select s.name, s.sql, t.wr, t.strict from sqlite_schema as s
		join pragma_table_list as t on t.schema = 'main' and t.name = s.name
		where s.type = 'table' and t.type = 'table' and s.name not like 'sqlite\_%' escape '\'
			and s.name <> '_tinystore_migrations'
		order by s.rowid`
	columnsQuery = `select name, type, "notnull", coalesce(dflt_value, ''), pk
		from pragma_table_xinfo(?) order by cid`
	indexesQuery = `select l.name, l."unique", l.origin, l.partial, coalesce(s.sql, '')
		from pragma_index_list(?) as l left join sqlite_schema as s on s.type = 'index' and s.name = l.name
		order by l.name`
	indexColumnsQuery = `select coalesce(name, ''), cid from pragma_index_xinfo(?) where key = 1 order by seqno`
	referencesQuery   = `select id, "table", "from", coalesce("to", ''), on_update, on_delete
		from pragma_foreign_key_list(?) order by id, seq`
	triggersQuery = `select sql from sqlite_schema where type = 'trigger' and tbl_name = ? order by rowid`
)

// Read is the catalog of the file r reads, its own tables left out: SQLite's,
// the migration history, and the shadow tables of a virtual one.
func Read(ctx context.Context, r Reader) (*Catalog, error) {
	tables, err := readTables(ctx, r)
	if err != nil {
		return nil, err
	}
	for _, t := range tables {
		if err = t.read(ctx, r); err != nil {
			return nil, fmt.Errorf("read table %s: %w", t.Name, err)
		}
	}
	return &Catalog{Tables: tables}, nil
}

// Declare is the catalog of the file ddl makes, in a database in memory.
func Declare(ctx context.Context, ddl string) (_ *Catalog, err error) {
	db, err := sql.Open("sqlite", ":memory:")
	if err != nil {
		return nil, err
	}
	defer func() { err = errors.Join(err, db.Close()) }()
	db.SetMaxOpenConns(1)
	if _, err = db.ExecContext(ctx, ddl); err != nil {
		return nil, fmt.Errorf("the schema does not run: %w", err)
	}
	return Read(ctx, db)
}

func readTables(ctx context.Context, r Reader) ([]*Table, error) {
	var tables []*Table
	err := each(ctx, r, tablesQuery, nil, func(rows *sql.Rows) error {
		t := &Table{}
		if err := rows.Scan(&t.Name, &t.SQL, &t.WithoutRowID, &t.Strict); err != nil {
			return err
		}
		tables = append(tables, t)
		return nil
	})
	return tables, err
}

func (t *Table) read(ctx context.Context, r Reader) error {
	if err := t.readColumns(ctx, r); err != nil {
		return err
	}
	if err := t.readIndexes(ctx, r); err != nil {
		return err
	}
	if err := t.readReferences(ctx, r); err != nil {
		return err
	}
	return each(ctx, r, triggersQuery, []any{t.Name}, func(rows *sql.Rows) error {
		var trigger string
		err := rows.Scan(&trigger)
		t.Triggers = append(t.Triggers, trigger)
		return err
	})
}

func (t *Table) readColumns(ctx context.Context, r Reader) error {
	definitions := map[string]string{}
	for _, e := range elements(t.SQL) {
		if e.name != "" {
			definitions[strings.ToLower(e.name)] = e.text
		}
		t.Checks = append(t.Checks, e.checks...)
	}
	type keyed struct {
		at   int
		name string
	}
	var keys []keyed
	err := each(ctx, r, columnsQuery, []any{t.Name}, func(rows *sql.Rows) error {
		c := &Column{}
		var at int
		if err := rows.Scan(&c.Name, &c.Type, &c.NotNull, &c.Default, &at); err != nil {
			return err
		}
		c.Type = strings.ToUpper(c.Type)
		c.Definition = definitions[strings.ToLower(c.Name)]
		t.Columns = append(t.Columns, c)
		if at > 0 {
			keys = append(keys, keyed{at: at, name: c.Name})
		}
		return nil
	})
	slices.SortFunc(keys, func(a, b keyed) int { return a.at - b.at })
	for _, key := range keys {
		t.PrimaryKey = append(t.PrimaryKey, key.name)
	}
	return err
}

func (t *Table) readIndexes(ctx context.Context, r Reader) error {
	err := each(ctx, r, indexesQuery, []any{t.Name}, func(rows *sql.Rows) error {
		ix := &Index{}
		if err := rows.Scan(&ix.Name, &ix.Unique, &ix.Origin, &ix.Partial, &ix.SQL); err != nil {
			return err
		}
		t.Indexes = append(t.Indexes, ix)
		return nil
	})
	for _, ix := range t.Indexes {
		if err != nil {
			break
		}
		err = each(ctx, r, indexColumnsQuery, []any{ix.Name}, func(rows *sql.Rows) error {
			var name string
			var at int
			if scanErr := rows.Scan(&name, &at); scanErr != nil {
				return scanErr
			}
			if at == -2 {
				name = ""
			}
			ix.Columns = append(ix.Columns, name)
			return nil
		})
	}
	return err
}

func (t *Table) readReferences(ctx context.Context, r Reader) error {
	last := -1
	return each(ctx, r, referencesQuery, []any{t.Name}, func(rows *sql.Rows) error {
		var id int
		var parent, from, to, onUpdate, onDelete string
		if err := rows.Scan(&id, &parent, &from, &to, &onUpdate, &onDelete); err != nil {
			return err
		}
		if id != last {
			t.References = append(t.References, Reference{Table: parent, OnDelete: onDelete, OnUpdate: onUpdate})
			last = id
		}
		ref := &t.References[len(t.References)-1]
		ref.From = append(ref.From, from)
		if to != "" {
			ref.To = append(ref.To, to)
		}
		return nil
	})
}

// each hands every row of query to visit, and closes them
func each(ctx context.Context, r Reader, query string, args []any, visit func(*sql.Rows) error) error {
	rows, err := r.QueryContext(ctx, query, args...)
	if err != nil {
		return err
	}
	defer rows.Close()
	for rows.Next() {
		if err = visit(rows); err != nil {
			return err
		}
	}
	if err = rows.Err(); err != nil {
		return err
	}
	return rows.Close()
}

func containsFold(names []string, name string) bool {
	for _, n := range names {
		if strings.EqualFold(n, name) {
			return true
		}
	}
	return false
}
