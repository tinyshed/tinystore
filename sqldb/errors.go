package sqldb

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"strings"

	"github.com/ncruces/go-sqlite3"

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

// ConstraintError is a write that broke a constraint. A unique or primary key
// is tinystore.ErrConflict, the others tinystore.ErrInvalid.
//
// SQLite's message becomes Table and Constraint when it names them:
//
//	UNIQUE constraint failed: users.email      Table users, Constraint email
//	CHECK constraint failed: length(title) > 0 Constraint length(title) > 0
type ConstraintError struct {
	// Kind comes from SQLite's extended code.
	Kind ConstraintKind
	// Table and Constraint are empty when SQLite's message does not name them.
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

var constraintKinds = map[sqlite3.ExtendedErrorCode]ConstraintKind{
	sqlite3.CONSTRAINT_UNIQUE:     UniqueViolation,
	sqlite3.CONSTRAINT_PRIMARYKEY: PrimaryKeyViolation,
	sqlite3.CONSTRAINT_ROWID:      PrimaryKeyViolation,
	sqlite3.CONSTRAINT_FOREIGNKEY: ForeignKeyViolation,
	sqlite3.CONSTRAINT_CHECK:      CheckViolation,
	sqlite3.CONSTRAINT_NOTNULL:    NotNullViolation,
}

// constraintOf reads SQLite's message for what the constraint names:
//
//	sqlite3: constraint failed: UNIQUE constraint failed: notes.author_id, notes.slug
//	→ Table notes, Constraint author_id, slug
func constraintOf(code sqlite3.ExtendedErrorCode, err error) (*ConstraintError, bool) {
	kind, known := constraintKinds[code]
	if !known {
		return nil, false
	}
	message := err.Error()
	var detailed *sqlite3.Error
	if errors.As(err, &detailed) {
		message = detailed.Error()
	}
	if _, after, found := strings.Cut(message, "constraint failed: "); found {
		message = after
	}
	c := &ConstraintError{Kind: kind, ExtendedCode: int(code), Err: err, message: message}
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

// explain names the database an error is about, and says which call a write
// sent to a reader belongs to. It gives a constraint its kind, and says
// ErrInvalid of a statement SQLite cannot run as it is written and ErrLimit of
// a value past its length. A context's error and a closed store's are kept as
// they came.
func (d *DB) explain(err error) error {
	switch {
	case err == nil:
		return nil
	case errors.Is(err, context.Canceled), errors.Is(err, context.DeadlineExceeded),
		errors.Is(err, tinystore.ErrClosed):
		return err
	}
	// the driver gives a code with SQLite's message, or the bare code
	var code sqlite3.ExtendedErrorCode
	switch {
	case errors.As(err, &code):
	case missingArgument(err):
		return fmt.Errorf("%w: sql %q: %w", tinystore.ErrInvalid, d.name, err)
	default:
		return fmt.Errorf("sql %q: %w", d.name, err)
	}
	switch code.Code() {
	case sqlite3.READONLY:
		return fmt.Errorf("%w: sql %q: a read cannot write; use Exec or an Exec form: %w",
			tinystore.ErrInvalid, d.name, err)
	case sqlite3.CONSTRAINT:
		if broken, known := constraintOf(code, err); known {
			return fmt.Errorf("sql %q: %w", d.name, broken)
		}
		return fmt.Errorf("%w: sql %q: %w", tinystore.ErrInvalid, d.name, err)
	case sqlite3.CORRUPT, sqlite3.NOTADB:
		return fmt.Errorf("%w: sql %q: %w", tinystore.ErrCorrupt, d.name, err)
	case sqlite3.ERROR, sqlite3.RANGE, sqlite3.MISMATCH:
		return fmt.Errorf("%w: sql %q: %w", tinystore.ErrInvalid, d.name, err)
	case sqlite3.TOOBIG:
		return fmt.Errorf("%w: sql %q: %w", tinystore.ErrLimit, d.name, err)
	case sqlite3.AUTH:
		return fmt.Errorf("%w: sql %q: a guest changes no schema and writes none of the store's own tables: %w",
			tinystore.ErrInvalid, d.name, err)
	}
	return fmt.Errorf("sql %q: %w", d.name, err)
}

// missingArgument is the error for arguments that do not fill a statement's
// parameters, which database/sql and the store give no type of their own
//
//	sql: expected 2 arguments, got 1    missing named argument "id"
func missingArgument(err error) bool {
	text := err.Error()
	return strings.Contains(text, " arguments, got ") || strings.Contains(text, "missing argument with index ") ||
		strings.Contains(text, "missing named argument ")
}

// heldTooLong says that a snapshot ended by its own bound, not its caller's:
// a statement it stopped, or a commit or a statement database/sql's rollback
// came before, which may have let the connection go
func heldTooLong(ctx context.Context, err error) error {
	ended := errors.Is(err, context.DeadlineExceeded) || errors.Is(err, sql.ErrTxDone) ||
		errors.Is(err, sql.ErrConnDone)
	if err != nil && ended && errors.Is(context.Cause(ctx), errSnapshotHeld) {
		return errSnapshotHeld
	}
	return err
}
