package sqldb

import (
	"errors"
	"testing"
	"time"

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
	note := Note{ID: UUID{1}, AuthorID: author.ID, Title: "first", CreatedAt: time.Now()}
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
			_, err := Insert(ctx, db, d.notes, Note{ID: UUID{2}, AuthorID: 999, Title: "x", CreatedAt: time.Now()})
			return err
		}, ForeignKeyViolation, "", "", tinystore.ErrInvalid},
		{"an empty title", func() error {
			_, err := Insert(ctx, db, d.notes, Note{ID: UUID{3}, AuthorID: author.ID, CreatedAt: time.Now()})
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
