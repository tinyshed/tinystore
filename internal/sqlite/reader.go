package sqlite

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
)

// the compiled statements a connection keeps unless Config.Statements says
const keptStatements = 32

type Reader interface {
	QueryContext(context.Context, string, ...any) (*sql.Rows, error)
}

// preparedConnection keeps the statements it compiled by their text, the
// least recently used closed first once it holds limit of them
type preparedConnection struct {
	conn       *sql.Conn
	limit      int
	statements map[string]*keptStatement
	newest     *keptStatement
	oldest     *keptStatement
	prepares   uint64
	evictions  uint64
}

// keptStatement is one compiled statement in its connection's order of use
type keptStatement struct {
	query     string
	statement *sql.Stmt
	newer     *keptStatement
	older     *keptStatement
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
	if kept := r.statements[query]; kept != nil {
		r.unlink(kept)
		r.pushNewest(kept)
		return kept.statement, nil
	}
	if len(r.statements) >= r.limit && r.oldest != nil {
		if err := r.retireOldest(); err != nil {
			return nil, err
		}
	}
	statement, err := r.conn.PrepareContext(ctx, query)
	if err != nil {
		return nil, fmt.Errorf("prepare SQLite read: %w", err)
	}
	if r.statements == nil {
		r.statements = make(map[string]*keptStatement, r.limit)
	}
	kept := &keptStatement{query: query, statement: statement}
	r.statements[query] = kept
	r.pushNewest(kept)
	r.prepares++
	return statement, nil
}

func (r *preparedConnection) retireOldest() error {
	oldest := r.oldest
	r.unlink(oldest)
	delete(r.statements, oldest.query)
	r.evictions++
	if err := oldest.statement.Close(); err != nil {
		return fmt.Errorf("retire SQLite read: %w", err)
	}
	return nil
}

func (r *preparedConnection) pushNewest(kept *keptStatement) {
	kept.older, kept.newer = r.newest, nil
	if r.newest != nil {
		r.newest.newer = kept
	}
	r.newest = kept
	if r.oldest == nil {
		r.oldest = kept
	}
}

func (r *preparedConnection) unlink(kept *keptStatement) {
	if kept.newer != nil {
		kept.newer.older = kept.older
	} else {
		r.newest = kept.older
	}
	if kept.older != nil {
		kept.older.newer = kept.newer
	} else {
		r.oldest = kept.newer
	}
	kept.newer, kept.older = nil, nil
}

func (r *preparedConnection) close() error {
	var err error
	for _, kept := range r.statements {
		err = errors.Join(err, kept.statement.Close())
	}
	if r.conn != nil {
		err = errors.Join(err, r.conn.Close())
	}
	r.conn, r.statements, r.newest, r.oldest = nil, nil, nil, nil
	return err
}

// ViewPrepared reuses bounded SQL programs, while every call gets a fresh snapshot.
func (f *File) ViewPrepared(ctx context.Context, read func(Reader) error) error {
	return f.view(ctx, func(_ *sql.Tx, connection *readConnection) error {
		return read(connection)
	})
}

// Lookup runs read on a reader without a transaction of its own, so that each
// statement is its own snapshot: a point read pays for one statement, not for
// the BEGIN and COMMIT a View puts around it. read closes the rows it opens.
func (f *File) Lookup(ctx context.Context, read func(Reader) error) error {
	return f.withReader(ctx, func(connection *readConnection) (bool, error) {
		return true, read(connection)
	})
}

func (f *File) view(ctx context.Context, read func(*sql.Tx, *readConnection) error) error {
	return f.withReader(ctx, func(connection *readConnection) (bool, error) {
		return transactReusable(ctx, connection.conn, func(tx *sql.Tx) error { return read(tx, connection) })
	})
}

// withReader lends work a reader connection, connecting it on first use, and
// closes it when work leaves it unfit for the next caller
func (f *File) withReader(ctx context.Context, work func(*readConnection) (reusable bool, err error)) error {
	if err := takeSlot(ctx, f.readSlots); err != nil {
		return err
	}
	defer freeSlot(f.readSlots)

	connection := f.takeIdleReader()
	defer f.returnIdleReader(connection)

	if connection.conn == nil {
		var err error
		if connection.conn, err = f.reader.Conn(ctx); err != nil {
			return fmt.Errorf("acquire SQLite reader: %w", err)
		}
		if err = f.limitLength(connection.conn); err != nil {
			return errors.Join(err, connection.close())
		}
	}

	reusable, err := work(connection)
	if !keepConnection(ctx, reusable, err) {
		return errors.Join(err, connection.close())
	}
	return err
}

// takeIdleReader prefers the most recently used connection, whose prepared
// programs are warm; a new one connects on first use.
func (f *File) takeIdleReader() *readConnection {
	f.readersMu.Lock()
	defer f.readersMu.Unlock()
	last := len(f.idleReaders) - 1
	if last < 0 {
		return &readConnection{preparedConnection{limit: f.statements}}
	}
	connection := f.idleReaders[last]
	f.idleReaders = f.idleReaders[:last]
	return connection
}

func (f *File) returnIdleReader(connection *readConnection) {
	f.readersMu.Lock()
	defer f.readersMu.Unlock()
	f.idleReaders = append(f.idleReaders, connection)
}

// EachRow hands every row to visit and closes the rows before it returns, so
// that the connection can run its next query; the first error stops the rows.
func EachRow(rows *sql.Rows, what string, visit func(*sql.Rows) error) error {
	var err error
	for rows.Next() {
		if err = visit(rows); err != nil {
			break
		}
	}
	if err == nil {
		err = rows.Err()
	}
	closeErr := rows.Close()
	if err != nil {
		return fmt.Errorf("read %s: %w", what, errors.Join(err, closeErr))
	}
	if closeErr != nil {
		return fmt.Errorf("close %s: %w", what, closeErr)
	}
	return nil
}

type Row struct {
	rows *sql.Rows
	err  error
}

// QueryRow runs a statement that walks a range for its row, a count or a sum,
// which ctx interrupts while it runs.
func QueryRow(ctx context.Context, reader Reader, query string, args ...any) *Row {
	rows, err := reader.QueryContext(ctx, query, args...) //nolint:rowserrcheck // Scan iterates and checks Err
	return &Row{rows: rows, err: err}
}

// QueryRowByKey runs a statement that finds its row by a key, or writes that row and returns it.
//
// ctx is checked before the statement and not while it runs: a context that can
// end costs database/sql and the driver a goroutine each, more than such a
// statement takes.
func QueryRowByKey(ctx context.Context, reader Reader, query string, args ...any) *Row {
	if err := ctx.Err(); err != nil {
		return &Row{err: err}
	}
	return QueryRow(context.WithoutCancel(ctx), reader, query, args...)
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

// ReaderPrepares is how many statements the idle readers have compiled.
func (f *File) ReaderPrepares() uint64 {
	f.readersMu.Lock()
	defer f.readersMu.Unlock()
	var prepares uint64
	for _, connection := range f.idleReaders {
		prepares += connection.prepares
	}
	return prepares
}
