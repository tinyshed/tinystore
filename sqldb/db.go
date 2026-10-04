package sqldb

import (
	"cmp"
	"context"
	"errors"
	"fmt"
	"io/fs"
	"log/slog"
	"math"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/ncruces/go-sqlite3"

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
	readers   int
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
//
// Nil migrations open the file as it is, making an empty one when there is
// none: nothing is applied and no history is checked, as a script or a first
// try wants. A later Open that carries migrations applies them from the first.
func Open(ctx context.Context, store *tinystore.Store, name string, migrations fs.FS, schema *SchemaDef,
	options ...OpenOption,
) (*DB, error) {
	timing := tuning{snapshot: snapshotHold}
	for _, option := range options {
		option(&timing)
	}
	timing.applyNone = timing.applyNone || store.Guest()
	return open(ctx, store, name, migrations, schema, timing)
}

type OpenOption func(*tuning)

// ApplyNone opens a file only if it has applied every migration given, and
// applies none, as a program that may change rows and not the schema opens one.
//
// A migration the file has not applied is ErrPending, and a file that is not
// there is not made.
func ApplyNone() OpenOption {
	return func(t *tuning) { t.applyNone = true }
}

// Readers is how many reader connections the database opens under load, eight
// unless it says, and fewer when the store's Options.Readers says; half a MiB
// each, and those beyond one close after a minute unused.
func Readers(n int) OpenOption {
	return func(t *tuning) { t.readers = n }
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

// asGuest registers the modules a connection of a guest store needs, and
// refuses what a guest must not do: change the schema, which its owner checked
// and compiled its statements against, or write the store's own tables, whose
// owner keeps their state in memory, as jobs keeps its queues'.
func asGuest(conn *sqlite3.Conn) error {
	if err := sqlite.RegisterModules(conn); err != nil {
		return err
	}
	return conn.SetAuthorizer(guestAuthorizer)
}

func guestAuthorizer(action sqlite3.AuthorizerActionCode, name, argument, _, _ string) sqlite3.AuthorizerReturnCode {
	switch action {
	case sqlite3.AUTH_INSERT, sqlite3.AUTH_UPDATE, sqlite3.AUTH_DELETE:
		if guestProtectedTable(name) {
			return sqlite3.AUTH_DENY
		}
	case sqlite3.AUTH_PRAGMA:
		if argument != "" && guestSchemaPragma(name) {
			return sqlite3.AUTH_DENY
		}
	case sqlite3.AUTH_CREATE_TABLE, sqlite3.AUTH_CREATE_INDEX, sqlite3.AUTH_CREATE_TRIGGER, sqlite3.AUTH_CREATE_VIEW,
		sqlite3.AUTH_CREATE_VTABLE, sqlite3.AUTH_DROP_TABLE, sqlite3.AUTH_DROP_INDEX, sqlite3.AUTH_DROP_TRIGGER,
		sqlite3.AUTH_DROP_VIEW, sqlite3.AUTH_DROP_VTABLE, sqlite3.AUTH_ALTER_TABLE:
		return sqlite3.AUTH_DENY
	}
	return sqlite3.AUTH_OK
}

func guestProtectedTable(name string) bool {
	name = strings.ToLower(name)
	return strings.HasPrefix(name, "_tinystore_") || name == "sqlite_schema" || name == "sqlite_master" ||
		name == "sqlite_temp_schema" || name == "sqlite_temp_master"
}

func guestSchemaPragma(name string) bool {
	switch strings.ToLower(name) {
	case "writable_schema", "schema_version", "application_id", "user_version":
		return true
	}
	return false
}

// modulePackages are the imports that link a virtual table module, which
// SQLite as the driver builds it leaves to each connection. SQLite tells an
// FTS5 table's shadow tables from the application's own only on a connection
// that has the module, so every connection to the file registers every
// module the program linked.
var modulePackages = map[string]string{
	"fts5":      "github.com/tinyshed/tinystore/sqldb/fts5",
	"rtree":     "github.com/tinyshed/tinystore/sqldb/rtree",
	"rtree_i32": "github.com/tinyshed/tinystore/sqldb/rtree",
	"geopoly":   "github.com/tinyshed/tinystore/sqldb/rtree",
}

const virtualTablesQuery = `select name, sql from sqlite_schema
	where type = 'table' and sql like 'create virtual table%'`

// checkModules refuses a file holding a virtual table whose module the
// program did not link, naming the import that links it
func checkModules(ctx context.Context, r sqlite.Reader) error {
	rows, err := r.QueryContext(ctx, virtualTablesQuery)
	if err != nil {
		return err
	}
	defer rows.Close()
	for rows.Next() {
		var name, create string
		if err = rows.Scan(&name, &create); err != nil {
			return err
		}
		if module := moduleOf(create); module != "" && !sqlite.ModuleLinked(module) {
			return missingModule(name, module)
		}
	}
	return rows.Err()
}

func missingModule(table, module string) error {
	if pkg, known := modulePackages[strings.ToLower(module)]; known {
		return fmt.Errorf("%w: %s uses %s, which the program did not link: import _ %q",
			tinystore.ErrInvalid, table, module, pkg)
	}
	return fmt.Errorf("%w: %s uses the module %s, which sqldb has no package for", tinystore.ErrInvalid, table, module)
}

// moduleOf is the module of a CREATE VIRTUAL TABLE:
//
//	create virtual table notes_fts using fts5(title, body)  →  fts5
func moduleOf(create string) string {
	lower := strings.ToLower(create)
	at := strings.Index(lower, " using ")
	if at < 0 {
		return ""
	}
	rest := strings.TrimSpace(create[at+len(" using "):])
	end := strings.IndexFunc(rest, func(r rune) bool { return r == '(' || r == ' ' || r == ';' })
	if end < 0 {
		end = len(rest)
	}
	return strings.Trim(rest[:end], "\"`[]")
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
	config.Readers = store.Readers(cmp.Or(timing.readers, readers))
	config.Statements, config.Waited = statements, d.waited
	config.MaxLength = int(min(store.Memory().Capacity, math.MaxInt32))
	config.Connected = sqlite.RegisterModules
	if store.Guest() {
		config.Connected = asGuest
	}
	file, err := sqlite.Open(ctx, path, config)
	if err != nil {
		return nil, fmt.Errorf("sql %q: open: %w", name, err)
	}
	switch {
	case scripts == nil && timing.applyNone: // a data connection opens what is there, as it is
	case scripts == nil:
		err = file.Claim(ctx, sqlApplicationID)
	case timing.applyNone:
		err = file.Verify(ctx, sqlApplicationID, scripts)
	default:
		err = file.Migrate(ctx, sqlApplicationID, scripts)
	}
	if err == nil {
		err = file.Lookup(ctx, func(r sqlite.Reader) error { return checkModules(ctx, r) })
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
	// #nosec G703 -- Claim validated this path inside the store.
	if _, err := os.Stat(path); errors.Is(err, fs.ErrNotExist) {
		return fmt.Errorf("%w: the file is not there", ErrPending)
	}
	return nil
}

// migrationError names the database, and says ErrInvalid of migrations the
// file has not applied or whose history they do not match
func migrationError(name string, err error) error {
	if errors.Is(err, ErrPending) || errors.Is(err, sqlite.ErrMismatch) {
		return fmt.Errorf("%w: sql %q: %w", tinystore.ErrInvalid, name, err)
	}
	if _, after, found := strings.Cut(err.Error(), "no such module: "); found {
		module, _, _ := strings.Cut(after, " ")
		return fmt.Errorf("sql %q: %w: %w", name, missingModule("a migration", module), err)
	}
	return fmt.Errorf("sql %q: %w", name, err)
}

// Migrated checks migrations against those the file applied, applying none:
// one it has not applied is ErrPending, and one changed after it was applied,
// or missing, is refused as Open refuses it. A database opens once in a store,
// so a program that opens it again checks with Migrated instead. Nil
// migrations check nothing, as Open opens with none.
func (d *DB) Migrated(ctx context.Context, migrations fs.FS) error {
	scripts, err := findMigrations(migrations)
	if err != nil || scripts == nil {
		return wrapName(d.name, err)
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

func wrapName(name string, err error) error {
	if err == nil {
		return nil
	}
	return fmt.Errorf("sql %q: %w", name, err)
}

// findMigrations is the directory in which migrations holds its .sql files: its
// root, or, while the root has none, its one directory; nil when there are none.
//
//	embed.FS of migrations/*.sql → migrations/
func findMigrations(migrations fs.FS) (fs.FS, error) {
	if migrations == nil {
		return nil, nil
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

// Copy writes a consistent copy of the database name into dir, as the store's
// Snapshot copies a database that is open, while no one in this store has it
// open: a backup of a whole directory takes the databases no program opened
// too. Opening one instead would make it open for good, and a migration that
// waits for its program would then wait for the store to open again.
//
// It holds the name while it copies, so an Open meanwhile is
// tinystore.ErrInUse, as is a copy of a database that is open.
func Copy(ctx context.Context, store *tinystore.Store, name, dir string) (tinystore.SnapshotFile, error) {
	if !validName.MatchString(name) {
		return tinystore.SnapshotFile{}, fmt.Errorf("%w: database name %q", tinystore.ErrInvalid, name)
	}
	path, release, err := store.Claim(fileName(name))
	if err != nil {
		return tinystore.SnapshotFile{}, fmt.Errorf("copy sql %q: %w", name, err)
	}
	defer release()
	if _, err = os.Stat(path); err != nil {
		return tinystore.SnapshotFile{}, fmt.Errorf("copy sql %q: %w", name, err)
	}
	schema, err := sqlite.Copy(ctx, path, tinystore.SnapshotPath(dir, fileName(name)))
	if err != nil {
		return tinystore.SnapshotFile{}, fmt.Errorf("copy sql %q: %w", name, err)
	}
	return tinystore.SnapshotFile{Name: fileName(name), Engine: "sql", Schema: schema}, nil
}

// SQLiteFile is the database's file, for an engine that keeps its tables in it,
// as a jobs store opened with Options.In does. A program has no use for it.
func (d *DB) SQLiteFile() *sqlite.File {
	return d.file
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

// waited logs a write that has waited ten seconds for the writer, naming the
// transaction holding it when there is one. A call on the DB inside its own Tx
// waits for the Tx, which waits for it.
func (d *DB) waited(query string) {
	if held := d.holder.Load(); held != nil {
		d.log.Warn("a write has waited ten seconds behind a transaction", "query", query,
			"transaction", site(held.site), "held", time.Since(held.since).Round(time.Millisecond))
		return
	}
	d.log.Warn("a write has waited ten seconds for the writer", "query", query)
}

// caller is where the function that calls it was called from: for Tx, the
// application's line that called Tx.
func caller() uintptr {
	var pcs [1]uintptr
	runtime.Callers(3, pcs[:])
	return pcs[0]
}

func site(pc uintptr) string {
	frame, _ := runtime.CallersFrames([]uintptr{pc}).Next()
	return fmt.Sprintf("%s (%s:%d)", frame.Function, filepath.Base(frame.File), frame.Line)
}
