package sqldb

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"strings"

	sqlite3 "modernc.org/sqlite"
	sqlitelib "modernc.org/sqlite/lib"

	"github.com/tinyshed/tinystore"
)

// ConstraintKind is the constraint a write broke.
type ConstraintKind int

const (
	UniqueViolation ConstraintKind = iota + 1
	PrimaryKeyViolation
	ForeignKeyViolation
	CheckViolation
	NotNullViolation
)

func (k ConstraintKind) String() string {
	return [...]string{"", "unique", "primary key", "foreign key", "check", "not null"}[k]
}

// ConstraintError is a write that broke a constraint. Kind comes from
// SQLite's extended code; Table and Constraint are filled when SQLite's
// message names them, and are empty otherwise:
//
//	UNIQUE constraint failed: users.email      Table users, Constraint email
//	CHECK constraint failed: length(title) > 0 Constraint length(title) > 0
//
// A unique or primary key is tinystore.ErrConflict, the others
// tinystore.ErrInvalid.
type ConstraintError struct {
	Kind         ConstraintKind
	Table        string
	Constraint   string
	ExtendedCode int
	Err          error
	message      string
}

func (e *ConstraintError) Error() string {
	return e.message
}

func (e *ConstraintError) Unwrap() []error {
	if e.Kind == UniqueViolation || e.Kind == PrimaryKeyViolation {
		return []error{tinystore.ErrConflict, e.Err}
	}
	return []error{tinystore.ErrInvalid, e.Err}
}

var constraintKinds = map[int]ConstraintKind{
	sqlitelib.SQLITE_CONSTRAINT_UNIQUE:     UniqueViolation,
	sqlitelib.SQLITE_CONSTRAINT_PRIMARYKEY: PrimaryKeyViolation,
	sqlitelib.SQLITE_CONSTRAINT_ROWID:      PrimaryKeyViolation,
	sqlitelib.SQLITE_CONSTRAINT_FOREIGNKEY: ForeignKeyViolation,
	sqlitelib.SQLITE_CONSTRAINT_CHECK:      CheckViolation,
	sqlitelib.SQLITE_CONSTRAINT_NOTNULL:    NotNullViolation,
}

// constraintOf reads SQLite's message for what the constraint names:
//
//	constraint failed: UNIQUE constraint failed: notes.author_id, notes.slug (2067)
//	→ Table notes, Constraint author_id, slug
func constraintOf(failure *sqlite3.Error) (*ConstraintError, bool) {
	kind, known := constraintKinds[failure.Code()]
	if !known {
		return nil, false
	}
	message := failure.Error()
	if _, after, found := strings.Cut(message, ": "); found {
		message = after
	}
	if at := strings.LastIndex(message, " ("); at > 0 && strings.HasSuffix(message, ")") {
		message = message[:at]
	}
	c := &ConstraintError{Kind: kind, ExtendedCode: failure.Code(), Err: failure, message: message}
	_, named, found := strings.Cut(message, " constraint failed: ")
	switch {
	case !found:
	case kind == CheckViolation:
		c.Constraint = named
	case strings.HasPrefix(named, "index '"):
		c.Constraint = strings.TrimSuffix(strings.TrimPrefix(named, "index '"), "'")
	default:
		c.Table, c.Constraint = columnsOf(named)
	}
	return c, true
}

// columnsOf splits SQLite's list of a constraint's columns, each named with
// its table: "t.a, t.b" → t, "a, b"
func columnsOf(named string) (table, columns string) {
	var names []string
	for part := range strings.SplitSeq(named, ", ") {
		owner, column, found := strings.Cut(part, ".")
		if !found {
			return "", named
		}
		table = owner
		names = append(names, column)
	}
	return table, strings.Join(names, ", ")
}

// explain names the database an error is about, says which call a write sent
// to a reader belongs to, gives a constraint its kind, and says ErrInvalid of
// a statement SQLite cannot run as it is written and ErrLimit of a value past
// its length; a context's error and a closed store's are kept as they came
func (d *DB) explain(err error) error {
	switch {
	case err == nil:
		return nil
	case errors.Is(err, context.Canceled), errors.Is(err, context.DeadlineExceeded),
		errors.Is(err, tinystore.ErrClosed):
		return err
	}
	var failure *sqlite3.Error
	switch {
	case errors.As(err, &failure):
	case missingArgument(err):
		return fmt.Errorf("%w: sql %q: %w", tinystore.ErrInvalid, d.name, err)
	default:
		return fmt.Errorf("sql %q: %w", d.name, err)
	}
	switch failure.Code() & 0xff {
	case sqlitelib.SQLITE_READONLY:
		return fmt.Errorf("%w: sql %q: a read cannot write; use Exec or an Exec form: %w",
			tinystore.ErrInvalid, d.name, err)
	case sqlitelib.SQLITE_CONSTRAINT:
		if broken, known := constraintOf(failure); known {
			return fmt.Errorf("sql %q: %w", d.name, broken)
		}
		return fmt.Errorf("%w: sql %q: %w", tinystore.ErrInvalid, d.name, err)
	case sqlitelib.SQLITE_CORRUPT, sqlitelib.SQLITE_NOTADB:
		return fmt.Errorf("%w: sql %q: %w", tinystore.ErrCorrupt, d.name, err)
	case sqlitelib.SQLITE_ERROR, sqlitelib.SQLITE_RANGE, sqlitelib.SQLITE_MISMATCH:
		return fmt.Errorf("%w: sql %q: %w", tinystore.ErrInvalid, d.name, err)
	case sqlitelib.SQLITE_TOOBIG:
		return fmt.Errorf("%w: sql %q: %w", tinystore.ErrLimit, d.name, err)
	}
	return fmt.Errorf("sql %q: %w", d.name, err)
}

// missingArgument is the driver's error for a parameter no argument fills,
// which it gives no type of its own
//
//	missing argument with index 2    missing named argument "id"
func missingArgument(err error) bool {
	text := err.Error()
	return strings.Contains(text, "missing argument with index ") || strings.Contains(text, "missing named argument ")
}

// heldTooLong says that a snapshot ended by its own bound, not its caller's:
// a statement it stopped, or a commit database/sql's rollback came before
func heldTooLong(ctx context.Context, err error) error {
	ended := errors.Is(err, context.DeadlineExceeded) || errors.Is(err, sql.ErrTxDone)
	if err != nil && ended && errors.Is(context.Cause(ctx), errSnapshotHeld) {
		return errSnapshotHeld
	}
	return err
}
