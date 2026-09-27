package sqldb

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"io/fs"
	"log/slog"
	"regexp"
	"sync"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/internal/sqlite"
)

// sqlApplicationID is "TSQL", the SQLite application id that claims a file for this engine
const sqlApplicationID = 0x5453514c

// ErrOutcomeUnknown is a write whose group's commit failed: it may or may not
// be in the file, and its caller reads it back before writing again.
var ErrOutcomeUnknown = sqlite.ErrOutcomeUnknown

// errPanicked rolls back the statement whose reading panicked; the panic goes
// on in its caller's goroutine
var errPanicked = errors.New("sqldb: a write panicked")

// readers is each database's pool of read connections
const readers = 4

// a name becomes a file name, so it stays short and plain
var validName = regexp.MustCompile(`^[a-z0-9][a-z0-9_-]{0,63}$`)

type DB struct {
	name string
	file *sqlite.File
	log  *slog.Logger

	mu      sync.Mutex
	closed  bool
	running sync.WaitGroup
}

// Open opens or creates sql/<name>.db inside the store and applies, in one
// transaction, every migration it has not applied before:
//
//	001_users.sql   applied on the first run
//	002_posts.sql   applied the first time a binary carrying it opens the file
//
// A migration changed after it was applied, a database newer than the binary's
// migrations, or another engine's file refuses to open.
func Open(ctx context.Context, store *tinystore.Store, name string, migrations fs.FS) (*DB, error) {
	if !validName.MatchString(name) {
		return nil, fmt.Errorf("%w: database name %q", tinystore.ErrInvalid, name)
	}

	path, release, err := store.Claim(fileName(name))
	if err != nil {
		return nil, err
	}

	db, err := openFile(ctx, path, name, migrations)
	if err != nil {
		release()
		return nil, err
	}
	db.log = store.Logger("sql").With("database", name)
	if err = store.Attach(db); err != nil {
		release()
		return nil, errors.Join(err, db.file.Close())
	}

	db.log.Info("opened", "path", path)
	return db, nil
}

func openFile(ctx context.Context, path, name string, migrations fs.FS) (*DB, error) {
	file, err := sqlite.Open(ctx, path, sqlite.Config{Readers: readers})
	if err != nil {
		return nil, fmt.Errorf("sql %q: open: %w", name, err)
	}
	if err = file.Migrate(ctx, sqlApplicationID, migrations); err != nil {
		return nil, errors.Join(fmt.Errorf("sql %q: %w", name, err), file.Close())
	}
	return &DB{name: name, file: file}, nil
}

func fileName(name string) string { return "sql/" + name + ".db" }

// Snapshot copies the database into dir while it keeps working.
func (d *DB) Snapshot(ctx context.Context, dir string) ([]tinystore.SnapshotFile, error) {
	schema, err := d.file.Snapshot(ctx, tinystore.SnapshotPath(dir, fileName(d.name)))
	if err != nil {
		return nil, fmt.Errorf("snapshot sql %q: %w", d.name, err)
	}
	return []tinystore.SnapshotFile{{Name: fileName(d.name), Engine: "sql", Schema: schema}}, nil
}

// Exec runs one statement on the file's one writer. Statements from many
// goroutines commit together, each in a savepoint of one transaction, with one
// fsync: one that fails rolls back alone, a caller whose context ends before
// its statement starts writes nothing, and a group whose commit fails answers
// ErrOutcomeUnknown.
func (d *DB) Exec(ctx context.Context, query string, args ...any) (sql.Result, error) {
	var result sql.Result
	err := d.write(ctx, func(q querier) (err error) {
		result, err = q.ExecContext(ctx, query, args...)
		return err
	})
	return result, err
}

// Tx runs work in one writer transaction of its own, on the caller's
// goroutine: nil commits, an error or a panic rolls back. Every call inside it
// takes tx, not the DB.
func (d *DB) Tx(ctx context.Context, work func(tx *Tx) error) error {
	if err := d.enter(); err != nil {
		return err
	}
	defer d.running.Done()
	err := d.file.Update(ctx, func(tx *sql.Tx) error { return work(&Tx{q: tx}) })
	return d.explain(err)
}

// Close waits for the calls in flight and closes the file. The store calls it:
// an application closes the store instead.
func (d *DB) Close(context.Context) error {
	d.mu.Lock()
	d.closed = true
	d.mu.Unlock()

	d.running.Wait()
	err := d.file.Close()
	d.log.Info("closed")
	return err
}

// Tx is one writer transaction, given to the function passed to DB.Tx.
type Tx struct{ q querier }

func (t *Tx) Exec(ctx context.Context, query string, args ...any) (sql.Result, error) {
	return t.q.ExecContext(ctx, query, args...)
}

// querier is what a read or a write runs a statement through: the transaction
// of a reader, or of the writer
type querier interface {
	ExecContext(ctx context.Context, query string, args ...any) (sql.Result, error)
	QueryContext(ctx context.Context, query string, args ...any) (*sql.Rows, error)
}

func (d *DB) read(ctx context.Context, work func(querier) error) error {
	if err := d.enter(); err != nil {
		return err
	}
	defer d.running.Done()
	err := d.file.View(ctx, func(tx *sql.Tx) error { return work(tx) })
	return d.explain(err)
}

// write runs one statement's work grouped with the writes other goroutines
// are waiting to commit, which may run it on theirs: a panic reading its rows
// rolls back its savepoint alone and goes on in its caller's goroutine
func (d *DB) write(ctx context.Context, work func(querier) error) error {
	if err := d.enter(); err != nil {
		return err
	}
	defer d.running.Done()
	var panicked any
	err := d.file.UpdateGrouped(ctx, 0, func(w sqlite.Writer) (err error) {
		defer func() {
			if recovered := recover(); recovered != nil {
				panicked, err = recovered, errPanicked
			}
		}()
		return work(w)
	})
	if panicked != nil {
		panic(panicked)
	}
	return d.explain(err)
}

func (d *DB) enter() error {
	d.mu.Lock()
	defer d.mu.Unlock()
	if d.closed {
		return fmt.Errorf("%w: sql %q", tinystore.ErrClosed, d.name)
	}
	d.running.Add(1)
	return nil
}
