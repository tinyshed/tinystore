package sqlite

import (
	"context"
	"database/sql"
	"database/sql/driver"
	"errors"
	"fmt"
)

const readerStatements = 32

type Reader interface {
	QueryContext(context.Context, string, ...any) (*sql.Rows, error)
}

type preparedConnection struct {
	conn       *sql.Conn
	statements map[string]*sql.Stmt
	order      []string
}

type readConnection struct {
	preparedConnection
}

func (r *preparedConnection) QueryContext(ctx context.Context, query string, args ...any) (*sql.Rows, error) {
	statement, err := r.prepare(ctx, query) //nolint:sqlclosecheck // retained until eviction or connection close
	if err != nil {
		return nil, err
	}
	return statement.QueryContext(ctx, args...)
}

func (r *preparedConnection) prepare(ctx context.Context, query string) (*sql.Stmt, error) {
	if statement := r.statements[query]; statement != nil {
		return statement, nil
	}
	if len(r.order) == readerStatements {
		oldest := r.order[0]
		if err := r.statements[oldest].Close(); err != nil {
			return nil, fmt.Errorf("retire SQLite read: %w", err)
		}
		delete(r.statements, oldest)
		r.order = r.order[1:]
	}
	statement, err := r.conn.PrepareContext(ctx, query)
	if err != nil {
		return nil, fmt.Errorf("prepare SQLite read: %w", err)
	}
	if r.statements == nil {
		r.statements = make(map[string]*sql.Stmt)
	}
	r.statements[query] = statement
	r.order = append(r.order, query)
	return statement, nil
}

func (r *preparedConnection) close() error {
	var err error
	for _, statement := range r.statements {
		err = errors.Join(err, statement.Close())
	}
	if r.conn != nil {
		err = errors.Join(err, r.conn.Close())
	}
	r.conn, r.statements, r.order = nil, nil, nil
	return err
}

// ViewPrepared reuses bounded SQL programs, while every call gets a fresh snapshot.
func (f *File) ViewPrepared(ctx context.Context, read func(Reader) error) error {
	return f.view(ctx, func(_ *sql.Tx, connection *readConnection) error {
		return read(connection)
	})
}

func (f *File) view(ctx context.Context, read func(*sql.Tx, *readConnection) error) error {
	select {
	case f.readSlots <- struct{}{}:
	case <-ctx.Done():
		return ctx.Err()
	}
	f.readersMu.Lock()
	var connection *readConnection
	if last := len(f.idleReaders) - 1; last >= 0 {
		connection = f.idleReaders[last]
		f.idleReaders = f.idleReaders[:last]
	}
	f.readersMu.Unlock()
	if connection == nil {
		connection = &readConnection{}
	}
	defer func() {
		f.readersMu.Lock()
		f.idleReaders = append(f.idleReaders, connection)
		f.readersMu.Unlock()
		<-f.readSlots
	}()
	if connection.conn == nil {
		var err error
		connection.conn, err = f.reader.Conn(ctx)
		if err != nil {
			return fmt.Errorf("acquire SQLite reader: %w", err)
		}
	}
	err, reusable := transactReusable(ctx, connection.conn, func(tx *sql.Tx) error { return read(tx, connection) })
	if !reusable || ctx.Err() != nil || errors.Is(err, driver.ErrBadConn) || errors.Is(err, sql.ErrConnDone) || errors.Is(err, context.Canceled) || errors.Is(err, context.DeadlineExceeded) {
		return errors.Join(err, connection.close())
	}
	return err
}

type Row struct {
	rows *sql.Rows
	err  error
}

func QueryRow(ctx context.Context, reader Reader, query string, args ...any) *Row {
	rows, err := reader.QueryContext(ctx, query, args...) //nolint:rowserrcheck // Scan owns iteration and checks rows.Err
	return &Row{rows: rows, err: err}
}

func (r *Row) Scan(dest ...any) error {
	if r.err != nil {
		return r.err
	}
	defer r.rows.Close()
	if !r.rows.Next() {
		if err := r.rows.Err(); err != nil {
			return err
		}
		return sql.ErrNoRows
	}
	if err := r.rows.Scan(dest...); err != nil {
		return err
	}
	return r.rows.Close()
}
