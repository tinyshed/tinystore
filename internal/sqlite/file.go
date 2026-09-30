// Package sqlite owns file and transaction mechanics, not engine data.
package sqlite

import (
	"cmp"
	"context"
	"database/sql"
	"database/sql/driver"
	"errors"
	"fmt"
	"net/url"
	"path/filepath"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/ncruces/go-sqlite3"
)

type File struct {
	path        string
	writer      *sql.DB
	writerConn  *writeConnection
	writeSlots  chan struct{}
	writes      group
	reader      *sql.DB
	readSlots   chan struct{}
	readersMu   sync.Mutex
	idleReaders []*readConnection
	commits     atomic.Uint64
	statements  int
	hold        time.Duration
	patience    time.Duration
	waited      func(label string)
}

type WriterCounters struct {
	Commits, CacheWrites, CacheSpills, CacheHits, CacheMisses uint64
	Prepared, Evicted                                         uint64
	Programs                                                  int
}

func (f *File) WriterCounters(ctx context.Context) (WriterCounters, error) {
	if err := takeSlot(ctx, f.writeSlots); err != nil {
		return WriterCounters{}, err
	}
	defer freeSlot(f.writeSlots)

	result := WriterCounters{Commits: f.commits.Load()}
	if f.writerConn == nil {
		return result, nil
	}
	result.Prepared = f.writerConn.prepares
	result.Evicted = f.writerConn.evictions
	result.Programs = len(f.writerConn.statements)

	err := f.writerConn.conn.Raw(func(driverConn any) error {
		return readCacheCounters(driverConn, &result)
	})
	return result, err
}

// Config sizes the reader pool once, because every pragma here is per
// connection and nothing in the pool may expire and reopen. PageSize applies
// only to a file this call creates; zero keeps SQLite's default.
type Config struct {
	Readers     int
	PageSize    int
	WriterCache int // bytes of the writer's page cache; zero keeps the readers' 1 MiB
	Statements  int // compiled statements each connection keeps; zero keeps 32

	// MaxLength is the longest string, blob or row a statement may make, since
	// SQLite allocates one whole before a budget can count it. Zero keeps
	// SQLite's default.
	MaxLength int

	// Connected runs on each connection as it opens. SQLite as the driver
	// builds it leaves FTS5 and R*Tree to each connection, and an
	// application's database registers them here.
	Connected func(*sqlite3.Conn) error

	// Waited is told once when a grouped write has waited Patience for the
	// writer, with the label UpdateGroupedAs gave the write.
	Waited func(label string)

	// GroupHold bounds a group's hold on the writer from when it holds it, and
	// Patience a grouped write's wait before Waited; zero keeps ten seconds.
	GroupHold, Patience time.Duration
}

func (c Config) check() error {
	if c.Readers < 1 {
		return fmt.Errorf("open SQLite: %d readers", c.Readers)
	}
	if c.PageSize != 0 && (c.PageSize < 512 || c.PageSize > 65536 || c.PageSize&(c.PageSize-1) != 0) {
		return fmt.Errorf("open SQLite: page size %d is not a power of two from 512 to 65536", c.PageSize)
	}
	if c.WriterCache < 0 || c.Statements < 0 || c.MaxLength < 0 || c.GroupHold < 0 || c.Patience < 0 {
		return fmt.Errorf("open SQLite: a negative cache, statements, length, hold or patience in %+v", c)
	}
	return nil
}

func Open(ctx context.Context, path string, config Config) (*File, error) {
	if path == "" || path == ":memory:" {
		return nil, errors.New("open SQLite: a file path is required")
	}
	if err := config.check(); err != nil {
		return nil, err
	}
	abs, err := filepath.Abs(path)
	if err != nil {
		return nil, fmt.Errorf("resolve SQLite path: %w", err)
	}

	f, err := openWriter(ctx, abs, config)
	if err != nil {
		return nil, err
	}
	f.path = abs

	if err = f.openReaders(ctx, abs, config); err != nil {
		return nil, errors.Join(err, f.Close())
	}

	if err = f.connectWriter(ctx); err != nil {
		return nil, errors.Join(fmt.Errorf("connect SQLite writer: %w", err), f.Close())
	}
	return f, nil
}

const walQuery = `pragma journal_mode=WAL`

// openWriter opens the single writer connection and turns the file to WAL, so
// readers keep their snapshots while it writes.
//
// The page size is set first because it must precede the file's first write,
// and turning on WAL is one.
func openWriter(ctx context.Context, abs string, config Config) (*File, error) {
	arguments := writerArguments()
	if config.PageSize > 0 {
		arguments.Add("_pragma", fmt.Sprintf("page_size(%d)", config.PageSize))
	}
	if config.WriterCache > 0 {
		withWriterCache(arguments, config.WriterCache)
	}
	writer, err := openPool(connectionURL(abs, arguments), config.MaxLength, config.Connected)
	if err != nil {
		return nil, fmt.Errorf("open SQLite writer: %w", err)
	}
	writer.SetMaxOpenConns(1)
	writer.SetMaxIdleConns(1)
	f := &File{
		writer: writer, writeSlots: make(chan struct{}, 1), waited: config.Waited,
		statements: cmp.Or(config.Statements, keptStatements),
		hold:       cmp.Or(config.GroupHold, groupHold),
		patience:   cmp.Or(config.Patience, writerPatience),
	}

	var journal string
	if err = writer.QueryRowContext(ctx, walQuery).Scan(&journal); err != nil || journal != "wal" {
		if err == nil {
			err = fmt.Errorf("journal mode is %q", journal)
		}
		return nil, errors.Join(fmt.Errorf("enable SQLite WAL: %w", err), f.Close())
	}
	return f, nil
}

func (f *File) openReaders(ctx context.Context, abs string, config Config) error {
	var err error
	f.reader, err = openPool(connectionURL(abs, readerArguments()), config.MaxLength, config.Connected)
	if err != nil {
		return fmt.Errorf("open SQLite reader: %w", err)
	}
	f.reader.SetMaxOpenConns(config.Readers)
	f.reader.SetMaxIdleConns(config.Readers)
	if err = f.reader.PingContext(ctx); err != nil {
		return fmt.Errorf("connect SQLite reader: %w", err)
	}
	f.readSlots = make(chan struct{}, config.Readers)
	return nil
}

// connectWriter pins the writer's one connection, which keeps its prepared
// programs for as long as it stays healthy.
func (f *File) connectWriter(ctx context.Context) error {
	connection, err := f.writer.Conn(ctx)
	if err != nil {
		return err
	}
	f.writerConn = &writeConnection{preparedConnection{conn: connection, limit: f.statements}}
	return nil
}

// withWriterCache gives the writer a page cache of bytes, the readers keeping
// their own
func withWriterCache(arguments url.Values, bytes int) {
	for i, pragma := range arguments["_pragma"] {
		if strings.HasPrefix(pragma, "cache_size(") {
			arguments["_pragma"][i] = fmt.Sprintf("cache_size(-%d)", bytes>>10)
		}
	}
}

// connectionURL carries the pragmas, because each pooled connection applies
// them itself when it opens:
//
//	writer  file:/data/metrics.db?_pragma=foreign_keys%281%29&…&_txlock=immediate&mode=rwc
//	reader  file:/data/metrics.db?_pragma=foreign_keys%281%29&…&_pragma=query_only%281%29&_txlock=deferred&mode=rw
//
// A Windows path keeps its drive first, file:D:/data/metrics.db, since the
// driver's file layer takes the path as SQLite hands it over.
func connectionURL(abs string, arguments url.Values) string {
	path := (&url.URL{Path: filepath.ToSlash(abs)}).EscapedPath()
	return "file:" + path + "?" + arguments.Encode()
}

// writerArguments sync a commit before it returns. On macOS an fsync leaves the
// data in the drive's own cache, so fullfsync and checkpoint_fullfsync ask for
// F_FULLFSYNC there; other platforms ignore them.
func writerArguments() url.Values {
	arguments := url.Values{"mode": {"rwc"}, "_txlock": {"immediate"}}
	for _, pragma := range []string{
		"foreign_keys(1)", "busy_timeout(5000)", "synchronous(FULL)", "fullfsync(1)", "checkpoint_fullfsync(1)",
		"cache_size(-1024)",
	} {
		arguments.Add("_pragma", pragma)
	}
	return arguments
}

// readerArguments are the writer's, except that a reader never creates the
// file, never takes the write lock and refuses to write.
func readerArguments() url.Values {
	arguments := writerArguments()
	arguments.Set("mode", "rw")
	arguments.Set("_txlock", "deferred")
	arguments.Add("_pragma", "query_only(1)")
	return arguments
}

// View pins every callback read to the same connection and snapshot.
func (f *File) View(ctx context.Context, read func(*sql.Tx) error) error {
	return f.view(ctx, func(tx *sql.Tx, _ *readConnection) error { return read(tx) })
}

func (f *File) Update(ctx context.Context, write func(*sql.Tx) error) error {
	return f.update(ctx, func(connection *writeConnection) (bool, error) {
		return transactReusable(ctx, connection.conn, write)
	})
}

func (f *File) update(ctx context.Context, work func(*writeConnection) (bool, error)) error {
	if err := takeSlot(ctx, f.writeSlots); err != nil {
		return err
	}
	defer freeSlot(f.writeSlots)
	return f.holding(ctx, work)
}

// holding runs work on the writer, whose slot its caller holds, reconnecting
// it first when the last work left it unfit
func (f *File) holding(ctx context.Context, work func(*writeConnection) (bool, error)) error {
	if f.writerConn == nil {
		if err := f.connectWriter(ctx); err != nil {
			return fmt.Errorf("reconnect SQLite writer: %w", err)
		}
	}

	reusable, err := work(f.writerConn)
	if err == nil {
		f.commits.Add(1)
	}
	if !keepConnection(ctx, reusable, err) {
		closeErr := f.writerConn.close()
		f.writerConn = nil
		return errors.Join(err, closeErr)
	}
	return err
}

func takeSlot(ctx context.Context, slots chan struct{}) error {
	select {
	case slots <- struct{}{}:
		return nil
	case <-ctx.Done():
		return ctx.Err()
	}
}

func freeSlot(slots chan struct{}) {
	<-slots
}

type transactionStarter interface {
	BeginTx(context.Context, *sql.TxOptions) (*sql.Tx, error)
}

// transactReusable also reports whether the connection may serve the next
// caller, which it may not after a failed begin, commit or rollback.
func transactReusable(ctx context.Context, db transactionStarter, work func(*sql.Tx) error) (reusable bool, err error) {
	tx, err := db.BeginTx(ctx, nil)
	if err != nil {
		return false, fmt.Errorf("begin SQLite transaction: %w", err)
	}
	defer func() {
		if rollbackErr := tx.Rollback(); rollbackErr != nil && !errors.Is(rollbackErr, sql.ErrTxDone) {
			err = errors.Join(err, fmt.Errorf("rollback SQLite transaction: %w", rollbackErr))
			reusable = false
		}
	}()

	if err = work(tx); err != nil {
		return true, err
	}
	if err = tx.Commit(); err != nil {
		return false, fmt.Errorf("commit SQLite transaction: %w", err)
	}
	return true, nil
}

// keepConnection refuses a connection that may still hold a half-run statement:
// its transaction did not end cleanly, or its caller gave up during it.
func keepConnection(ctx context.Context, reusable bool, err error) bool {
	if !reusable || ctx.Err() != nil {
		return false
	}
	return !errors.Is(err, driver.ErrBadConn) && !errors.Is(err, sql.ErrConnDone) &&
		!errors.Is(err, context.Canceled) && !errors.Is(err, context.DeadlineExceeded)
}

// Close releases connections; callers must first drain their operations.
func (f *File) Close() error {
	var err error
	f.readersMu.Lock()
	idle := f.idleReaders
	f.idleReaders = nil
	f.readersMu.Unlock()
	for _, connection := range idle {
		err = errors.Join(err, connection.close())
	}
	if f.reader != nil {
		if closeErr := f.reader.Close(); closeErr != nil {
			err = errors.Join(err, fmt.Errorf("close SQLite reader: %w", closeErr))
		}
	}
	if f.writerConn != nil {
		err = errors.Join(err, f.writerConn.close())
		f.writerConn = nil
	}
	if f.writer != nil {
		if closeErr := f.writer.Close(); closeErr != nil {
			err = errors.Join(err, fmt.Errorf("close SQLite writer: %w", closeErr))
		}
	}
	return err
}
