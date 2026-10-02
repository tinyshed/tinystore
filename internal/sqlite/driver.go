package sqlite

import (
	"bytes"
	"context"
	"database/sql"
	"database/sql/driver"
	"errors"
	"fmt"
	"io"
	"math"
	"net/url"
	"strconv"
	"strings"

	"github.com/ncruces/go-sqlite3"
)

// The store's connections come from a database/sql driver of its own over
// ncruces' SQLite, which no name registers.
//
// ncruces' own driver package registers "sqlite3" as it loads, as
// mattn/go-sqlite3 does, cgo or not: a program linking both panicked before
// main. And it read a time into TEXT it guessed held one, and scanned a blob
// into a []byte by appending to what the slice held. This one gives every
// value as SQLite keeps it: an integer, a float, text, a blob or nothing, the
// bytes a caller keeps always its own.

// openPool opens the connections name describes, a file: URL whose _pragma
// arguments each connection runs as it opens and whose _txlock says how its
// transactions begin; maxLength and connected are Config's
func openPool(name string, maxLength int, connected func(*sqlite3.Conn) error) (*sql.DB, error) {
	begin, err := beginOf(name)
	if err != nil {
		return nil, err
	}
	return sql.OpenDB(&connector{name: name, begin: begin, maxLength: maxLength, connected: connected}), nil
}

// OpenDB opens name, a path or a file: URL, through the store's own driver,
// for a database its caller keeps itself: a schema declared in memory, or a
// file a test makes.
func OpenDB(name string) (*sql.DB, error) {
	return openPool(name, 0, nil)
}

// beginOf is the statement that begins a transaction of name's pool
//
//	file:/data/metrics.db?…&_txlock=immediate    →    BEGIN IMMEDIATE
func beginOf(name string) (string, error) {
	var lock string
	if rest, isURL := strings.CutPrefix(name, "file:"); isURL {
		_, query, _ := strings.Cut(rest, "?")
		arguments, err := url.ParseQuery(query)
		if err != nil {
			return "", fmt.Errorf("sqlite: the arguments of %s: %w", name, err)
		}
		lock = arguments.Get("_txlock")
	}
	switch lock {
	case "", "deferred":
		return "BEGIN DEFERRED", nil
	case "immediate", "exclusive":
		return "BEGIN " + strings.ToUpper(lock), nil
	}
	return "", fmt.Errorf("sqlite: the transaction lock %q", lock)
}

// connector sets each connection up as it opens: the longest string, blob or
// row a statement may make, then what its file's Config.Connected adds
type connector struct {
	name      string
	begin     string
	maxLength int
	connected func(*sqlite3.Conn) error
}

func (c *connector) Connect(ctx context.Context) (driver.Conn, error) {
	raw, err := sqlite3.OpenContext(ctx, c.name)
	if err != nil {
		return nil, err
	}

	if c.maxLength > 0 {
		raw.Limit(sqlite3.LIMIT_LENGTH, min(c.maxLength, math.MaxInt32))
	}
	if c.connected != nil {
		if err = c.connected(raw); err != nil {
			return nil, errors.Join(err, raw.Close())
		}
	}
	return &conn{raw: raw, begin: c.begin}, nil
}

func (c *connector) Driver() driver.Driver {
	return opener{}
}

// opener opens a connection by name, which database/sql does only for a
// driver registered under one; the store's never is
type opener struct{}

func (opener) Open(name string) (driver.Conn, error) {
	begin, err := beginOf(name)
	if err != nil {
		return nil, err
	}
	return (&connector{name: name, begin: begin}).Connect(context.Background())
}

// conn is one connection, and the transaction it holds, as database/sql
// uses them.
//
// It says when the connection is fit for its next user, so that database/sql
// keeps a connection whose transaction a context ended, which it has rolled
// back, rather than closing it and a pinned reader's programs with it.
type conn struct {
	raw   *sqlite3.Conn
	begin string
}

var (
	_ driver.ConnBeginTx        = (*conn)(nil)
	_ driver.ConnPrepareContext = (*conn)(nil)
	_ driver.ExecerContext      = (*conn)(nil)
	_ driver.NamedValueChecker  = (*conn)(nil)
	_ driver.SessionResetter    = (*conn)(nil)
	_ driver.Validator          = (*conn)(nil)
	_ driver.Tx                 = (*conn)(nil)
)

// Raw lends the connection to what reads its status or registers on it
func (c *conn) Raw() *sqlite3.Conn {
	return c.raw
}

func (c *conn) Close() error {
	return c.raw.Close()
}

// ResetSession refuses a connection left inside a transaction
func (c *conn) ResetSession(context.Context) error {
	if !c.raw.GetAutocommit() {
		return driver.ErrBadConn
	}
	return nil
}

func (c *conn) IsValid() bool {
	return c.raw.GetAutocommit()
}

func (c *conn) Begin() (driver.Tx, error) {
	return c.BeginTx(context.Background(), driver.TxOptions{})
}

// BeginTx begins as its pool's transactions do; the store asks for no
// isolation or read-only transaction of its own, a reader being query_only
func (c *conn) BeginTx(ctx context.Context, options driver.TxOptions) (driver.Tx, error) {
	if options.Isolation != driver.IsolationLevel(sql.LevelDefault) || options.ReadOnly {
		return nil, errors.New("sqlite: a transaction takes its pool's isolation")
	}
	if err := c.exec(ctx, c.begin); err != nil {
		return nil, err
	}
	return c, nil
}

func (c *conn) Commit() error {
	err := c.raw.Exec("COMMIT")
	if err != nil && !c.raw.GetAutocommit() {
		return errors.Join(err, c.Rollback())
	}
	return err
}

// Rollback runs whatever ended the context of the transaction it ends
func (c *conn) Rollback() error {
	old := c.raw.SetInterrupt(context.Background())
	defer c.raw.SetInterrupt(old)
	return c.raw.Exec("ROLLBACK")
}

func (c *conn) Prepare(query string) (driver.Stmt, error) {
	return c.PrepareContext(context.Background(), query)
}

// PrepareContext compiles the one statement query holds; what follows it may
// only be space, semicolons and comments
func (c *conn) PrepareContext(ctx context.Context, query string) (driver.Stmt, error) {
	old := c.raw.SetInterrupt(ctx)
	defer c.raw.SetInterrupt(old)

	raw, tail, err := c.raw.Prepare(query)
	switch {
	case err != nil:
		return nil, err
	case raw == nil:
		return nil, errors.New("sqlite: the SQL holds no statement")
	case !blank(tail):
		return nil, errors.Join(errors.New("sqlite: multiple statements in one call"), raw.Close())
	}
	return newStatement(c, raw), nil
}

// ExecContext runs every statement of query when it has no argument, as a
// migration's script needs; one with arguments goes to database/sql, which
// prepares the statement it may hold
func (c *conn) ExecContext(ctx context.Context, query string, args []driver.NamedValue) (driver.Result, error) {
	if len(args) > 0 {
		return nil, driver.ErrSkip
	}
	if err := c.exec(ctx, query); err != nil {
		return nil, err
	}
	return resultOf(c.raw), nil
}

// CheckNamedValue lets every argument through to ExecContext, which hands
// one with arguments back to database/sql, and so to the statement's check
func (c *conn) CheckNamedValue(*driver.NamedValue) error {
	return nil
}

func (c *conn) exec(ctx context.Context, query string) error {
	old := c.raw.SetInterrupt(ctx)
	defer c.raw.SetInterrupt(old)
	return c.raw.Exec(query)
}

// blank says whether the SQL after a statement is only what SQLite skips:
// space, semicolons and comments, a comment left open at the end included
func blank(tail string) bool {
	for len(tail) > 0 {
		switch {
		case strings.ContainsRune(" \t\n\f\r;", rune(tail[0])):
			tail = tail[1:]
		case strings.HasPrefix(tail, "--"):
			_, tail, _ = strings.Cut(tail, "\n")
		case strings.HasPrefix(tail, "/*"):
			_, tail, _ = strings.Cut(tail[2:], "*/")
		default:
			return false
		}
	}
	return true
}

type result struct{ id, changes int64 }

// resultOf is what the statement that just ran changed, and the row it added
// when it changed one; a rowid from an earlier insert is no statement's
func resultOf(raw *sqlite3.Conn) result {
	changes := raw.Changes()
	if changes == 0 {
		return result{}
	}
	return result{id: raw.LastInsertRowID(), changes: changes}
}

func (r result) LastInsertId() (int64, error) {
	return r.id, nil
}

func (r result) RowsAffected() (int64, error) {
	return r.changes, nil
}

// statement is a prepared statement that knows each parameter's name or position.
type statement struct {
	conn       *conn
	raw        *sqlite3.Stmt
	inputs     int // what database/sql counts: the parameters, or -1 when one is named
	parameters []parameter
}

var (
	_ driver.StmtExecContext   = (*statement)(nil)
	_ driver.StmtQueryContext  = (*statement)(nil)
	_ driver.NamedValueChecker = (*statement)(nil)
)

type parameter struct {
	position int
	name     string
}

// newStatement keeps the positions SQLite assigned while compiling the SQL.
// A numbered parameter leaves argument counting to the statement.
func newStatement(c *conn, raw *sqlite3.Stmt) *statement {
	s := &statement{conn: c, raw: raw, inputs: raw.BindCount()}
	for i := 1; i <= raw.BindCount(); i++ {
		name := raw.BindName(i)
		if name != "" {
			s.inputs = -1
		}
		if name != "" && name[0] == '?' || name == "$"+strconv.Itoa(i) {
			name = ""
		}
		s.parameters = append(s.parameters, parameter{position: i, name: name})
	}
	return s
}

func (s *statement) Close() error {
	return s.raw.Close()
}

func (s *statement) NumInput() int {
	return s.inputs
}

func (s *statement) Exec(args []driver.Value) (driver.Result, error) {
	return s.ExecContext(context.Background(), ordered(args))
}

func (s *statement) Query(args []driver.Value) (driver.Rows, error) {
	return s.QueryContext(context.Background(), ordered(args))
}

func (s *statement) ExecContext(ctx context.Context, args []driver.NamedValue) (driver.Result, error) {
	if err := s.bind(args); err != nil {
		return nil, err
	}
	old := s.conn.raw.SetInterrupt(ctx)
	defer s.conn.raw.SetInterrupt(old)

	if err := errors.Join(s.raw.Exec(), s.raw.ClearBindings()); err != nil {
		return nil, err
	}
	return resultOf(s.conn.raw), nil
}

func (s *statement) QueryContext(ctx context.Context, args []driver.NamedValue) (driver.Rows, error) {
	if err := s.bind(args); err != nil {
		return nil, err
	}
	return &rows{ctx: ctx, statement: s}, nil
}

// CheckNamedValue lets SQLite's own types through, and an int, which every
// limit is, and leaves any other to database/sql's conversion
func (s *statement) CheckNamedValue(arg *driver.NamedValue) error {
	switch arg.Value.(type) {
	case nil, int64, int, float64, bool, string, []byte:
		return nil
	}
	return driver.ErrSkip
}

// bind gives each parameter its argument, a named one under every prefix it
// was written with, and clears what it bound when one does not fit
func (s *statement) bind(args []driver.NamedValue) error {
	if err := s.filled(args); err != nil {
		return err
	}
	for _, arg := range args {
		if arg.Name == "" {
			if err := s.bindAt(arg.Ordinal, arg.Value); err != nil {
				return err
			}
			continue
		}
		for _, prefix := range [...]string{":", "@", "$"} {
			if at := s.raw.BindIndex(prefix + arg.Name); at != 0 {
				if err := s.bindAt(at, arg.Value); err != nil {
					return err
				}
			}
		}
	}
	return nil
}

func (s *statement) bindAt(at int, value driver.Value) error {
	var err error
	switch v := value.(type) {
	case nil:
		err = s.raw.BindNull(at)
	case int64:
		err = s.raw.BindInt64(at, v)
	case int:
		err = s.raw.BindInt64(at, int64(v))
	case float64:
		err = s.raw.BindFloat(at, v)
	case bool:
		err = s.raw.BindBool(at, v)
	case string:
		err = s.raw.BindText(at, v)
	case []byte:
		err = s.raw.BindBlob(at, v)
	default:
		err = fmt.Errorf("sqlite: an argument of type %T", value)
	}
	if err != nil {
		return errors.Join(err, s.raw.ClearBindings())
	}
	return nil
}

// filled refuses arguments that leave a parameter empty, which SQLite would
// run as NULL
//
//	where id = :id   given sql.Named("key", 1)   →   missing named argument ":id"
func (s *statement) filled(args []driver.NamedValue) error {
	for _, parameter := range s.parameters {
		if parameter.name == "" {
			if parameter.position <= len(args) && args[parameter.position-1].Name == "" {
				continue
			}
			return fmt.Errorf("missing argument with index %d", parameter.position)
		}

		found := false
		for _, arg := range args {
			if arg.Name == parameter.name[1:] {
				found = true
				break
			}
		}
		if !found {
			return fmt.Errorf("missing named argument %q", parameter.name)
		}
	}
	return nil
}

func ordered(args []driver.Value) []driver.NamedValue {
	named := make([]driver.NamedValue, len(args))
	for i, value := range args {
		named[i] = driver.NamedValue{Ordinal: i + 1, Value: value}
	}
	return named
}

// rows reads a statement's rows. A value comes as SQLite keeps it, and bytes
// are copied for whatever keeps them; only a sql.RawBytes borrows SQLite's,
// until the next row.
type rows struct {
	ctx       context.Context
	statement *statement
	columns   []string
}

var _ driver.RowsColumnScanner = (*rows)(nil)

func (r *rows) Columns() []string {
	if r.columns == nil {
		r.columns = make([]string, r.statement.raw.ColumnCount())
		for i := range r.columns {
			r.columns[i] = r.statement.raw.ColumnName(i)
		}
	}
	return r.columns
}

// Close readies the statement for its next use
func (r *rows) Close() error {
	return errors.Join(r.statement.raw.Reset(), r.statement.raw.ClearBindings())
}

func (r *rows) NextRow() error {
	interrupt := r.statement.conn.raw
	old := interrupt.SetInterrupt(r.ctx)
	defer interrupt.SetInterrupt(old)

	if r.statement.raw.Step() {
		return nil
	}
	if err := r.statement.raw.Err(); err != nil {
		return err
	}
	return io.EOF
}

// Next is for database/sql before Go 1.27, which copies each value again
func (r *rows) Next(dest []driver.Value) error {
	if err := r.NextRow(); err != nil {
		return err
	}
	for i := range dest {
		value, err := r.value(i)
		if err != nil {
			return err
		}
		dest[i] = value
	}
	return nil
}

func (r *rows) value(i int) (driver.Value, error) {
	switch r.statement.raw.ColumnType(i) {
	case sqlite3.INTEGER:
		return r.statement.raw.ColumnInt64(i), nil
	case sqlite3.FLOAT:
		return r.statement.raw.ColumnFloat(i), nil
	case sqlite3.TEXT:
		text, err := r.held(r.statement.raw.ColumnRawText(i))
		return string(text), err
	case sqlite3.BLOB:
		blob, err := r.held(r.statement.raw.ColumnRawBlob(i))
		return bytes.Clone(blob), err
	}
	return nil, nil
}

// ScanColumn writes column i into dest as database/sql's Scan would, without
// first copying every value into one of its own
func (r *rows) ScanColumn(scan driver.ScanContext, i int, dest any) error {
	switch r.statement.raw.ColumnType(i) {
	case sqlite3.INTEGER:
		return scanInteger(scan, r.statement.raw.ColumnInt64(i), dest)
	case sqlite3.FLOAT:
		return scanFloat(scan, r.statement.raw.ColumnFloat(i), dest)
	case sqlite3.TEXT:
		text, err := r.held(r.statement.raw.ColumnRawText(i))
		if err != nil {
			return err
		}
		return scanText(scan, text, dest)
	case sqlite3.BLOB:
		blob, err := r.held(r.statement.raw.ColumnRawBlob(i))
		if err != nil {
			return err
		}
		return scanBlob(scan, blob, dest)
	}
	return sql.ConvertAssign(scan, dest, nil)
}

// held is a column's bytes as SQLite holds them; none is an empty blob, unless
// reading them failed
func (r *rows) held(view []byte) ([]byte, error) {
	if view == nil {
		return nil, r.statement.raw.Err()
	}
	return view, nil
}

func scanInteger(scan driver.ScanContext, value int64, dest any) error {
	switch d := dest.(type) {
	case *int64:
		*d = value
	case *any:
		*d = value
	default:
		return sql.ConvertAssign(scan, dest, value)
	}
	return nil
}

func scanFloat(scan driver.ScanContext, value float64, dest any) error {
	switch d := dest.(type) {
	case *float64:
		*d = value
	case *any:
		*d = value
	default:
		return sql.ConvertAssign(scan, dest, value)
	}
	return nil
}

func scanText(scan driver.ScanContext, held []byte, dest any) error {
	switch d := dest.(type) {
	case *string:
		*d = string(held)
	case *any:
		*d = string(held)
	case *sql.RawBytes:
		*d = held
	default:
		return sql.ConvertAssign(scan, dest, string(held))
	}
	return nil
}

// scanBlob copies the bytes for a []byte or an any; a sql.Scanner borrows
// them, as database/sql says, and copies what it keeps
func scanBlob(scan driver.ScanContext, held []byte, dest any) error {
	switch d := dest.(type) {
	case *[]byte:
		*d = bytes.Clone(held)
	case *any:
		*d = bytes.Clone(held)
	default:
		return sql.ConvertAssign(scan, dest, held)
	}
	return nil
}

// ended is a statement's error as its caller knows it: one SQLite interrupted
// because its context ended is that context's error, which the driver's
// INTERRUPT alone does not say.
func ended(ctx context.Context, err error) error {
	if err == nil || ctx.Err() == nil || !errors.Is(err, sqlite3.INTERRUPT) {
		return err
	}
	return fmt.Errorf("%w: %w", ctx.Err(), err)
}

// rawConnection is a driver connection that lends its SQLite connection
type rawConnection interface{ Raw() *sqlite3.Conn }

func readCacheCounters(driverConn any, result *WriterCounters) error {
	raw, ok := driverConn.(rawConnection)
	if !ok {
		return fmt.Errorf("SQLite writer %T does not expose database status", driverConn)
	}
	for _, item := range []struct {
		op sqlite3.DBStatus
		to *uint64
	}{
		{sqlite3.DBSTATUS_CACHE_WRITE, &result.CacheWrites},
		{sqlite3.DBSTATUS_CACHE_SPILL, &result.CacheSpills},
		{sqlite3.DBSTATUS_CACHE_HIT, &result.CacheHits},
		{sqlite3.DBSTATUS_CACHE_MISS, &result.CacheMisses},
	} {
		count, _, err := raw.Raw().Status(item.op, false)
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
