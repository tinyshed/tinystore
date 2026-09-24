package sqlite

import (
	"context"
	"database/sql"
	"fmt"
)

type Writer interface {
	Reader
	ExecContext(context.Context, string, ...any) (sql.Result, error)
}

type writeConnection struct {
	preparedConnection
}

func (w *writeConnection) ExecContext(ctx context.Context, query string, arguments ...any) (sql.Result, error) {
	statement, err := w.prepare(ctx, query) //nolint:sqlclosecheck // retained until eviction or connection close
	if err != nil {
		return nil, err
	}
	result, err := statement.ExecContext(ctx, arguments...)
	if err != nil {
		return nil, fmt.Errorf("execute SQLite write: %w", err)
	}
	return result, nil
}

// UpdatePrepared reuses SQL programs on the single writer connection within one transaction.
func (f *File) UpdatePrepared(ctx context.Context, write func(Writer) error) error {
	return f.update(ctx, func(connection *writeConnection) (bool, error) {
		return transactReusable(ctx, connection.conn, func(_ *sql.Tx) error {
			return write(connection)
		})
	})
}
