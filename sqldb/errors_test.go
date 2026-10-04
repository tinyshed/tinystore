package sqldb

import (
	"database/sql"
	"errors"
	"testing"
	"time"
	"uuid"

	"github.com/tinyshed/tinystore"
)

// a constraint a write breaks is a *ConstraintError of its kind, naming what
// SQLite's message names; a key already held is a conflict, the rest invalid
func TestAConstraintSaysItsKind(t *testing.T) {
	d := declareDesign(t)
	db := openDesign(t, d)
	ctx := t.Context()
	author, err := Insert(ctx, db, d.users, User{Email: "a@example.com", Name: "A", CreatedAt: time.Now()})
	if err != nil {
		t.Fatal(err)
	}
	note := Note{ID: uuid.UUID{1}, AuthorID: author.ID, Title: "first", CreatedAt: time.Now()}
	if _, err = Insert(ctx, db, d.notes, note); err != nil {
		t.Fatal(err)
	}

	for _, c := range []struct {
		name              string
		write             func() error
		kind              ConstraintKind
		table, constraint string
		sentinel          error
	}{
		{"an email taken", func() error {
			_, err := Insert(ctx, db, d.users, User{Email: "a@example.com", Name: "B", CreatedAt: time.Now()})
			return err
		}, UniqueViolation, "users", "email", tinystore.ErrConflict},
		{
			"a key held", func() error { _, err := Insert(ctx, db, d.notes, note); return err },
			PrimaryKeyViolation, "notes", "id", tinystore.ErrConflict,
		},
		{"a rowid held", func() error {
			_, err := db.Exec(ctx, `insert into users (id, email, display_name, created_at) values (?, 'b', 'B', 0)`, author.ID)
			return err
		}, PrimaryKeyViolation, "users", "id", tinystore.ErrConflict},
		{"an author nobody is", func() error {
			_, err := Insert(ctx, db, d.notes, Note{ID: uuid.UUID{2}, AuthorID: 999, Title: "x", CreatedAt: time.Now()})
			return err
		}, ForeignKeyViolation, "", "", tinystore.ErrInvalid},
		{"an empty title", func() error {
			_, err := Insert(ctx, db, d.notes, Note{ID: uuid.UUID{3}, AuthorID: author.ID, CreatedAt: time.Now()})
			return err
		}, CheckViolation, "", "length(title) > 0", tinystore.ErrInvalid},
		{"no title at all", func() error {
			_, err := db.Exec(ctx, `insert into notes (id, author_id, created_at) values ('x', ?, 0)`, author.ID)
			return err
		}, NotNullViolation, "notes", "title", tinystore.ErrInvalid},
		{"inside a Tx", func() error {
			return db.Tx(ctx, func(tx *Tx) error {
				_, err := Insert(ctx, tx, d.users, User{Email: "a@example.com", Name: "C"})
				return err
			})
		}, UniqueViolation, "users", "email", tinystore.ErrConflict},
	} {
		err := c.write()
		broken, ok := errors.AsType[*ConstraintError](err)
		if !ok || broken.Kind != c.kind || broken.Table != c.table || broken.Constraint != c.constraint ||
			!errors.Is(err, c.sentinel) {
			t.Errorf("%s: %v, as %+v", c.name, err, broken)
		}
	}
}

// a statement SQLite cannot run as it is written is invalid, whichever call
// sends it, and a value past SQLite's length is a limit
func TestAStatementSQLiteRefusesIsInvalid(t *testing.T) {
	db := openNotes(t)
	ctx := t.Context()
	for _, c := range []struct {
		name     string
		run      func() error
		sentinel error
	}{
		{"a syntax error", func() error {
			_, err := db.Exec(ctx, `insert notes values`)
			return err
		}, tinystore.ErrInvalid},
		{"a table it does not have", func() error {
			_, err := Query(ctx, db, `select * from nothing`)
			return err
		}, tinystore.ErrInvalid},
		{"a column it does not have", func() error {
			_, err := All[note](ctx, db, `select nothing from notes`)
			return err
		}, tinystore.ErrInvalid},
		{"a function it does not have", func() error {
			return db.Tx(ctx, func(tx *Tx) error {
				_, err := tx.Exec(ctx, `select nothing()`)
				return err
			})
		}, tinystore.ErrInvalid},
		{"a value past its length", func() error {
			_, err := Scalar[[]byte](ctx, db, `select zeroblob(1000000001)`)
			return err
		}, tinystore.ErrLimit},
	} {
		if err := c.run(); !errors.Is(err, c.sentinel) {
			t.Errorf("%s: %v", c.name, err)
		}
	}
	if _, err := db.Exec(ctx, `insert into notes (title) values (?)`); !errors.Is(err, tinystore.ErrInvalid) {
		t.Errorf("a missing argument: %v", err)
	}
	if _, err := Query(ctx, db, `select :id`); !errors.Is(err, tinystore.ErrInvalid) {
		t.Errorf("a missing named argument: %v", err)
	}
}

func TestAStatementWithNumberedParametersNeedsEveryArgument(t *testing.T) {
	db := openNotes(t)
	for _, c := range []struct {
		query string
		args  []any
	}{
		{`select ?1 + ?2`, []any{1}},
		{`select ?1 + :second`, []any{sql.Named("second", 2)}},
		{`select :first + ?2`, []any{sql.Named("first", 1)}},
	} {
		if _, err := db.Exec(t.Context(), c.query, c.args...); !errors.Is(err, tinystore.ErrInvalid) {
			t.Errorf("%s without every argument: %v", c.query, err)
		}
	}

	for _, query := range []string{`select ?1 + ?2`, `select $1 + $2`} {
		value, err := Scalar[int64](t.Context(), db, query, 1, 2)
		if err != nil || value != 3 {
			t.Errorf("%s with its arguments: %d, %v", query, value, err)
		}
	}
}
