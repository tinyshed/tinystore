// Package sqlite owns file and transaction mechanics, not engine data.
package sqlite

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"net/url"
	"path/filepath"
	"strings"

	_ "modernc.org/sqlite"
)

type File struct {
	writer *sql.DB
	reader *sql.DB
}

func Open(ctx context.Context, path string) (*File, error) {
	if path == "" || path == ":memory:" {
		return nil, errors.New("open SQLite: a file path is required")
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
	f.reader.SetMaxOpenConns(2)
	f.reader.SetMaxIdleConns(2)
	if err = f.reader.PingContext(ctx); err != nil {
		return nil, errors.Join(fmt.Errorf("connect SQLite reader: %w", err), f.Close())
	}
	return f, nil
}

// View pins every callback read to the same connection and snapshot.
func (f *File) View(ctx context.Context, read func(*sql.Tx) error) error {
	return transact(ctx, f.reader, read)
}

func (f *File) Update(ctx context.Context, write func(*sql.Tx) error) error {
	return transact(ctx, f.writer, write)
}

func transact(ctx context.Context, db *sql.DB, work func(*sql.Tx) error) (err error) {
	tx, err := db.BeginTx(ctx, nil)
	if err != nil {
		return fmt.Errorf("begin SQLite transaction: %w", err)
	}
	defer func() {
		if rollbackErr := tx.Rollback(); rollbackErr != nil && !errors.Is(rollbackErr, sql.ErrTxDone) {
			err = errors.Join(err, fmt.Errorf("rollback SQLite transaction: %w", rollbackErr))
		}
	}()
	if err = work(tx); err != nil {
		return err
	}
	if err = tx.Commit(); err != nil {
		return fmt.Errorf("commit SQLite transaction: %w", err)
	}
	return nil
}

// Close releases connections; callers must first drain their operations.
func (f *File) Close() error {
	var err error
	if f.reader != nil {
		if closeErr := f.reader.Close(); closeErr != nil {
			err = fmt.Errorf("close SQLite reader: %w", closeErr)
		}
	}
	if f.writer != nil {
		if closeErr := f.writer.Close(); closeErr != nil {
			err = errors.Join(err, fmt.Errorf("close SQLite writer: %w", closeErr))
		}
	}
	return err
}
