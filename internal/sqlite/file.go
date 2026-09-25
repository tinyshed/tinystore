// Package sqlite owns file and transaction mechanics, not engine data.
package sqlite

import (
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

	"modernc.org/sqlite"
)

type File struct {
	path        string
	writer      *sql.DB
	writerConn  *writeConnection
	writeSlots  chan struct{}
	reader      *sql.DB
	readSlots   chan struct{}
	readersMu   sync.Mutex
	idleReaders []*readConnection
	commits     atomic.Uint64
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

func readCacheCounters(driverConn any, result *WriterCounters) error {
	status, ok := driverConn.(sqlite.DBStatus)
	if !ok {
		return fmt.Errorf("SQLite writer does not expose database status")
	}
	for _, item := range []struct {
		op sqlite.DBStatusOp
		to *uint64
	}{
		{sqlite.DBStatusCacheWrite, &result.CacheWrites},
		{sqlite.DBStatusCacheSpill, &result.CacheSpills},
		{sqlite.DBStatusCacheHit, &result.CacheHits},
		{sqlite.DBStatusCacheMiss, &result.CacheMisses},
	} {
		count, _, err := status.Status(item.op, false)
		if err != nil {
			return fmt.Errorf("read SQLite writer status: %w", err)
		}
		if count < 0 {
			return fmt.Errorf("SQLite writer status counter overflow")
		}
		*item.to = uint64(count)
	}
	return nil
}

// Config sizes the reader pool once, because every pragma here is per
// connection and nothing in the pool may expire and reopen. PageSize applies
// only to a file this call creates; zero keeps SQLite's default.
type Config struct {
	Readers  int
	PageSize int
}

func (c Config) check() error {
	if c.Readers < 1 {
		return fmt.Errorf("open SQLite: %d readers", c.Readers)
	}
	if c.PageSize != 0 && (c.PageSize < 512 || c.PageSize > 65536 || c.PageSize&(c.PageSize-1) != 0) {
		return fmt.Errorf("open SQLite: page size %d is not a power of two from 512 to 65536", c.PageSize)
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

	f, err := openWriter(ctx, abs, config.PageSize)
	if err != nil {
		return nil, err
	}
	f.path = abs

	if err = f.openReaders(ctx, abs, config.Readers); err != nil {
		return nil, errors.Join(err, f.Close())
	}

	if err = f.connectWriter(ctx); err != nil {
		return nil, errors.Join(fmt.Errorf("connect SQLite writer: %w", err), f.Close())
	}
	return f, nil
}

const walQuery = `pragma journal_mode=WAL`

// openWriter opens the pool of the one connection that writes, and turns the
// file to WAL, so that readers keep their snapshots while it writes. The page
// size is a pragma of the connection because it must precede the file's first
// write, which turning on WAL is.
func openWriter(ctx context.Context, abs string, pageSize int) (*File, error) {
	arguments := writerArguments()
	if pageSize > 0 {
		arguments.Add("_pragma", fmt.Sprintf("page_size(%d)", pageSize))
	}
	writer, err := sql.Open("sqlite", connectionURL(abs, arguments))
	if err != nil {
		return nil, fmt.Errorf("open SQLite writer: %w", err)
	}
	writer.SetMaxOpenConns(1)
	writer.SetMaxIdleConns(1)
	f := &File{writer: writer, writeSlots: make(chan struct{}, 1)}

	var journal string
	if err = writer.QueryRowContext(ctx, walQuery).Scan(&journal); err != nil || journal != "wal" {
		if err == nil {
			err = fmt.Errorf("journal mode is %q", journal)
		}
		return nil, errors.Join(fmt.Errorf("enable SQLite WAL: %w", err), f.Close())
	}
	return f, nil
}

func (f *File) openReaders(ctx context.Context, abs string, readers int) error {
	var err error
	f.reader, err = sql.Open("sqlite", connectionURL(abs, readerArguments()))
	if err != nil {
		return fmt.Errorf("open SQLite reader: %w", err)
	}
	f.reader.SetMaxOpenConns(readers)
	f.reader.SetMaxIdleConns(readers)
	if err = f.reader.PingContext(ctx); err != nil {
		return fmt.Errorf("connect SQLite reader: %w", err)
	}
	f.readSlots = make(chan struct{}, readers)
	return nil
}

// connectWriter pins the writer's one connection, which keeps its prepared
// programs for as long as it stays healthy.
func (f *File) connectWriter(ctx context.Context) error {
	connection, err := f.writer.Conn(ctx)
	if err != nil {
		return err
	}
	f.writerConn = &writeConnection{conn: connection}
	return nil
}

// connectionURL carries the pragmas, because each pooled connection applies
// them itself when it opens:
//
//	writer  file:///data/metrics.db?_pragma=foreign_keys%281%29&…&_txlock=immediate&mode=rwc
//	reader  file:///data/metrics.db?_pragma=foreign_keys%281%29&…&_pragma=query_only%281%29&_txlock=deferred&mode=rw
func connectionURL(abs string, arguments url.Values) string {
	uri := &url.URL{Scheme: "file", Path: filepath.ToSlash(abs)}
	if !strings.HasPrefix(uri.Path, "/") {
		uri.Path = "/" + uri.Path
	}
	uri.RawQuery = arguments.Encode()
	return uri.String()
}

func writerArguments() url.Values {
	arguments := url.Values{"mode": {"rwc"}, "_txlock": {"immediate"}}
	for _, pragma := range []string{"foreign_keys(1)", "busy_timeout(5000)", "synchronous(FULL)", "cache_size(-1024)"} {
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
