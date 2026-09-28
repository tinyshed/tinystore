package sqldb

import (
	"context"
	"errors"
	"fmt"
	"io/fs"
	"log/slog"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/internal/admission"
	"github.com/tinyshed/tinystore/internal/sqlite"
)

// sqlApplicationID is "TSQL", the SQLite application id that claims a file for this engine
const sqlApplicationID = 0x5453514c

// ErrOutcomeUnknown is a write whose group's commit failed: it may or may not
// be in the file, and its caller reads it back before writing again.
var ErrOutcomeUnknown = sqlite.ErrOutcomeUnknown

// ErrPending is a migration the file has not applied, which ApplyNone and
// Migrated find and apply none of; it comes with tinystore.ErrInvalid.
var ErrPending = sqlite.ErrPending

type DB struct {
	name     string
	file     *sqlite.File
	log      *slog.Logger
	runtime  *tinystore.Store
	budgeted bool // the store has Options.Memory to reserve from
	gate     admission.Gate
	writes   admission.Slots
	closed   error
	leave    func() // gate.Leave, bound once rather than at every call
	holder   atomic.Pointer[holder]
	snapshot time.Duration // how long an Each or a View holds its snapshot
	closing  sync.Once
	closeErr error
}

// tuning is what a test shortens: how long the writer is held and waited for,
// and how long a snapshot lasts
type tuning struct {
	writer    sqlite.Config
	snapshot  time.Duration
	applyNone bool // ApplyNone's: the file must have applied every migration
}

// holder is the transaction that holds the writer: where it began, and when
type holder struct {
	site  uintptr
	since time.Time
}

// Open opens or creates sql/<name>.db inside the store, applies in one
// transaction every migration the file has not applied, then checks the file
// against schema, which may be nil:
//
//	001_users.sql   applied on the first run
//	002_posts.sql   applied the first time a binary carrying it opens the file
//
// The migrations are the .sql files at the root of migrations or, when it has
// none, in its one directory, as an embed.FS of migrations/*.sql holds them. A
// migration changed after it was applied, a file that has applied more of them
// than the binary knows, another engine's file, or a file that does not match
// the schema refuses to open.
func Open(ctx context.Context, store *tinystore.Store, name string, migrations fs.FS, schema *SchemaDef,
	options ...OpenOption,
) (*DB, error) {
	timing := tuning{snapshot: snapshotHold}
	for _, option := range options {
		option(&timing)
	}
	return open(ctx, store, name, migrations, schema, timing)
}

// OpenOption changes how Open treats the file.
type OpenOption func(*tuning)

// ApplyNone opens a file only if it has applied every migration given, and
// applies none, as a program that may change rows and not the schema opens
// one: a migration the file has not applied is ErrPending, and a file that is
// not there is not made.
func ApplyNone() OpenOption {
	return func(t *tuning) { t.applyNone = true }
}

func open(
	ctx context.Context, store *tinystore.Store, name string, migrations fs.FS, schema *SchemaDef, timing tuning,
) (*DB, error) {
	if !validName.MatchString(name) {
		return nil, fmt.Errorf("%w: database name %q", tinystore.ErrInvalid, name)
	}
	scripts, err := findMigrations(migrations)
	if err != nil {
		return nil, fmt.Errorf("sql %q: %w", name, err)
	}

	path, release, err := store.Claim(fileName(name))
	if err != nil {
		return nil, err
	}
	if err = mayOpen(path, timing); err != nil {
		release()
		return nil, migrationError(name, err)
	}

	d, err := openFile(ctx, store, name, path, scripts, timing)
	if err != nil {
		release()
		return nil, err
	}
	if schema != nil {
		err = d.check(ctx, schema)
	}
	if err == nil {
		err = store.Attach(d)
	}
	if err != nil {
		release()
		return nil, errors.Join(err, d.file.Close())
	}

	d.log.Info("opened", "path", path)
	return d, nil
}

func openFile(
	ctx context.Context, store *tinystore.Store, name, path string, scripts fs.FS, timing tuning,
) (*DB, error) {
	d := &DB{
		name: name, runtime: store, log: store.Logger("sql").With("database", name),
		budgeted: store.Memory().Capacity > 0, writes: admission.NewSlots(writeSlots),
		closed: fmt.Errorf("%w: sql %q", tinystore.ErrClosed, name), snapshot: timing.snapshot,
	}
	d.leave = d.gate.Leave

	config := timing.writer
	config.Readers, config.Statements, config.Waited = readers, statements, d.waited
	file, err := sqlite.Open(ctx, path, config)
	if err != nil {
		return nil, fmt.Errorf("sql %q: open: %w", name, err)
	}
	if timing.applyNone {
		err = file.Verify(ctx, sqlApplicationID, scripts)
	} else {
		err = file.Migrate(ctx, sqlApplicationID, scripts)
	}
	if err != nil {
		return nil, errors.Join(migrationError(name, err), file.Close())
	}
	d.file = file
	return d, nil
}

// mayOpen refuses to make a file that ApplyNone opens: none there has applied
// anything
func mayOpen(path string, timing tuning) error {
	if !timing.applyNone {
		return nil
	}
	if _, err := os.Stat(path); errors.Is(err, fs.ErrNotExist) {
		return fmt.Errorf("%w: the file is not there", ErrPending)
	}
	return nil
}

// migrationError names the database, and says ErrInvalid of a pending
// migration too
func migrationError(name string, err error) error {
	if errors.Is(err, ErrPending) {
		return fmt.Errorf("%w: sql %q: %w", tinystore.ErrInvalid, name, err)
	}
	return fmt.Errorf("sql %q: %w", name, err)
}

// Migrated checks migrations against those the file applied, applying none:
// one it has not applied is ErrPending, and one changed after it was applied,
// or missing, is refused as Open refuses it. A database opens once in a store,
// so a program that opens it again checks with Migrated instead.
func (d *DB) Migrated(ctx context.Context, migrations fs.FS) error {
	scripts, err := findMigrations(migrations)
	if err != nil {
		return fmt.Errorf("sql %q: %w", d.name, err)
	}
	leave, err := d.admit(ctx)
	if err != nil {
		return err
	}
	defer leave()
	if err = d.file.Verify(ctx, sqlApplicationID, scripts); err != nil {
		return migrationError(d.name, err)
	}
	return nil
}

// findMigrations is the directory of migrations holds its .sql files: its
// root, or, while the root has none, its one directory
//
//	embed.FS of migrations/*.sql → migrations/
func findMigrations(migrations fs.FS) (fs.FS, error) {
	if migrations == nil {
		return nil, fmt.Errorf("%w: no migrations", tinystore.ErrInvalid)
	}
	for {
		entries, err := fs.ReadDir(migrations, ".")
		if err != nil {
			return nil, fmt.Errorf("read migrations: %w", err)
		}
		var directories []string
		for _, entry := range entries {
			switch {
			case !entry.IsDir() && strings.HasSuffix(entry.Name(), ".sql"):
				return migrations, nil
			case entry.IsDir():
				directories = append(directories, entry.Name())
			}
		}
		if len(directories) != 1 {
			return nil, fmt.Errorf("%w: the migrations hold no .sql files, and %d directories; fs.Sub names the one",
				tinystore.ErrInvalid, len(directories))
		}
		if migrations, err = fs.Sub(migrations, directories[0]); err != nil {
			return nil, err
		}
	}
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

// Close lets the calls in flight finish and closes the file; cancellation
// stops waiting, not the cleanup. The store calls it: an application closes
// the store instead.
func (d *DB) Close(ctx context.Context) error {
	drained, _ := d.gate.Close()
	select {
	case <-drained:
	case <-ctx.Done():
		return ctx.Err()
	}
	d.closing.Do(func() {
		d.closeErr = d.file.Close()
		d.log.Info("closed")
	})
	return d.closeErr
}

func (d *DB) admit(ctx context.Context) (leave func(), err error) {
	if err = d.gate.Enter(ctx, d.closed); err != nil {
		return nil, err
	}
	return d.leave, nil
}

// admitWrite lets a write in and holds one of the write slots until it is
// answered, so that the writes waiting for a group are bounded
func (d *DB) admitWrite(ctx context.Context) (func(), error) {
	leave, err := d.admit(ctx)
	if err != nil {
		return nil, err
	}
	free, err := d.writes.Take(ctx)
	if err != nil {
		leave()
		return nil, err
	}
	return func() {
		free()
		leave()
	}, nil
}

// waited logs a write that has waited ten seconds for the writer, and the
// transaction holding it, when one does: a call on the DB inside its own Tx
// waits for the Tx, which waits for it
func (d *DB) waited(query string) {
	if held := d.holder.Load(); held != nil {
		d.log.Warn("a write has waited ten seconds behind a transaction", "query", query,
			"transaction", site(held.site), "held", time.Since(held.since).Round(time.Millisecond))
		return
	}
	d.log.Warn("a write has waited ten seconds for the writer", "query", query)
}

// caller is where the function calling the one that calls it was called from
func caller() uintptr {
	var pcs [1]uintptr
	runtime.Callers(3, pcs[:])
	return pcs[0]
}

func site(pc uintptr) string {
	frame, _ := runtime.CallersFrames([]uintptr{pc}).Next()
	return fmt.Sprintf("%s (%s:%d)", frame.Function, filepath.Base(frame.File), frame.Line)
}
