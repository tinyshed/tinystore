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

	_ "modernc.org/sqlite"
)

type File struct {
	writer      *sql.DB
	writerConn  *writeConnection
	writeSlots  chan struct{}
	reader      *sql.DB
	readSlots   chan struct{}
	readersMu   sync.Mutex
	idleReaders []*readConnection
}

// Open takes the reader-pool size because every pragma here is per connection,
// so the pool is sized once and never allowed to expire and reopen.
func Open(ctx context.Context, path string, readers int) (*File, error) {
	if path == "" || path == ":memory:" {
		return nil, errors.New("open SQLite: a file path is required")
	}
	if readers < 1 {
		return nil, fmt.Errorf("open SQLite: %d readers", readers)
	}
	abs, err := filepath.Abs(path)
	if err != nil {
		return nil, fmt.Errorf("resolve SQLite path: %w", err)
	}
	uri := &url.URL{Scheme: "file", Path: filepath.ToSlash(abs)}
	if !strings.HasPrefix(uri.Path, "/") {
		uri.Path = "/" + uri.Path
	}
	args := url.Values{"mode": {"rwc"}, "_txlock": {"immediate"}}
	for _, pragma := range []string{"foreign_keys(1)", "busy_timeout(5000)", "synchronous(FULL)", "cache_size(-1024)"} {
		args.Add("_pragma", pragma)
	}
	uri.RawQuery = args.Encode()
	w, err := sql.Open("sqlite", uri.String())
	if err != nil {
		return nil, fmt.Errorf("open SQLite writer: %w", err)
	}
	w.SetMaxOpenConns(1)
	w.SetMaxIdleConns(1)
	f := &File{writer: w}
	var journal string
	if err = w.QueryRowContext(ctx, `pragma journal_mode=WAL`).Scan(&journal); err != nil || journal != "wal" {
		if err == nil {
			err = fmt.Errorf("journal mode is %q", journal)
		}
		return nil, errors.Join(fmt.Errorf("enable SQLite WAL: %w", err), f.Close())
	}
	args.Set("mode", "rw")
	args.Set("_txlock", "deferred")
	args.Add("_pragma", "query_only(1)")
	uri.RawQuery = args.Encode()
	f.reader, err = sql.Open("sqlite", uri.String())
	if err != nil {
		return nil, errors.Join(fmt.Errorf("open SQLite reader: %w", err), f.Close())
	}
	f.reader.SetMaxOpenConns(readers)
	f.reader.SetMaxIdleConns(readers)
	if err = f.reader.PingContext(ctx); err != nil {
		return nil, errors.Join(fmt.Errorf("connect SQLite reader: %w", err), f.Close())
	}
	f.readSlots = make(chan struct{}, readers)
	f.writeSlots = make(chan struct{}, 1)
	connection, err := w.Conn(ctx)
	if err != nil {
		return nil, errors.Join(fmt.Errorf("connect SQLite writer: %w", err), f.Close())
	}
	f.writerConn = &writeConnection{conn: connection}
	return f, nil
}

// View pins every callback read to the same connection and snapshot.
func (f *File) View(ctx context.Context, read func(*sql.Tx) error) error {
	return f.view(ctx, func(tx *sql.Tx, _ *readConnection) error { return read(tx) })
}

func (f *File) Update(ctx context.Context, write func(*sql.Tx) error) error {
	return f.update(ctx, func(connection *writeConnection) (error, bool) {
		return transactReusable(ctx, connection.conn, write)
	})
}

func (f *File) update(ctx context.Context, work func(*writeConnection) (error, bool)) error {
	select {
	case f.writeSlots <- struct{}{}:
	case <-ctx.Done():
		return ctx.Err()
	}
	defer func() { <-f.writeSlots }()
	if f.writerConn == nil {
		connection, err := f.writer.Conn(ctx)
		if err != nil {
			return fmt.Errorf("reconnect SQLite writer: %w", err)
		}
		f.writerConn = &writeConnection{conn: connection}
	}
	err, reusable := work(f.writerConn)
	if !reusable || ctx.Err() != nil || errors.Is(err, driver.ErrBadConn) || errors.Is(err, sql.ErrConnDone) || errors.Is(err, context.Canceled) || errors.Is(err, context.DeadlineExceeded) {
		closeErr := f.writerConn.close()
		f.writerConn = nil
		return errors.Join(err, closeErr)
	}
	return err
}

type transactionStarter interface {
	BeginTx(context.Context, *sql.TxOptions) (*sql.Tx, error)
}

func transactReusable(ctx context.Context, db transactionStarter, work func(*sql.Tx) error) (err error, reusable bool) {
	tx, err := db.BeginTx(ctx, nil)
	if err != nil {
		return fmt.Errorf("begin SQLite transaction: %w", err), false
	}
	reusable = true
	defer func() {
		if rollbackErr := tx.Rollback(); rollbackErr != nil && !errors.Is(rollbackErr, sql.ErrTxDone) {
			err = errors.Join(err, fmt.Errorf("rollback SQLite transaction: %w", rollbackErr))
			reusable = false
		}
	}()
	if err = work(tx); err != nil {
		return err, reusable
	}
	if err = tx.Commit(); err != nil {
		return fmt.Errorf("commit SQLite transaction: %w", err), false
	}
	return nil, reusable
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
