package sqlite

import (
	"context"
	"database/sql"
	"database/sql/driver"
	"errors"
	"fmt"
	"math"
	"strconv"

	"github.com/ncruces/go-sqlite3"
	sqlitedriver "github.com/ncruces/go-sqlite3/driver"
)

// openPool opens the connections url names through the driver's own
// connector, not database/sql's name for it, which a program's other SQLite
// driver may hold: a program linking mattn/go-sqlite3 too builds with
// -ldflags=-X=github.com/ncruces/go-sqlite3/driver.driverName= and loses
// nothing here.
func openPool(url string, maxLength int, connected func(*sqlite3.Conn) error) (*sql.DB, error) {
	inner, err := (&sqlitedriver.SQLite{}).OpenConnector(url)
	if err != nil {
		return nil, err
	}
	return sql.OpenDB(&connector{inner: inner, maxLength: maxLength, connected: connected}), nil
}

// connector sets each connection up as it opens: the longest string, blob or
// row a statement may make, then what its file's Config.Connected adds
type connector struct {
	inner     driver.Connector
	maxLength int
	connected func(*sqlite3.Conn) error
}

func (c *connector) Connect(ctx context.Context) (driver.Conn, error) {
	conn, err := c.inner.Connect(ctx)
	if err != nil {
		return nil, err
	}

	opened, ok := conn.(sqlitedriver.Conn)
	execer, execs := conn.(driver.ExecerContext)
	checker, checks := conn.(driver.NamedValueChecker)
	if !ok || !execs || !checks {
		return nil, errors.Join(fmt.Errorf("SQLite connection %T is not the driver's", conn), conn.Close())
	}

	if c.maxLength > 0 {
		opened.Raw().Limit(sqlite3.LIMIT_LENGTH, min(c.maxLength, math.MaxInt32))
	}
	if c.connected != nil {
		if err = c.connected(opened.Raw()); err != nil {
			return nil, errors.Join(err, conn.Close())
		}
	}
	return &session{Conn: opened, execer: execer, checker: checker}, nil
}

func (c *connector) Driver() driver.Driver {
	return c.inner.Driver()
}

// session is one connection as database/sql uses it.
//
// It says when the connection is fit for its next user, so that database/sql
// keeps a connection whose transaction a context ended, which the driver has
// rolled back, rather than closing it and a pinned reader's programs with it.
// And it refuses a statement whose named parameter no argument fills, which
// the driver would leave NULL.
type session struct {
	sqlitedriver.Conn
	execer  driver.ExecerContext
	checker driver.NamedValueChecker
}

var (
	_ driver.SessionResetter    = (*session)(nil)
	_ driver.Validator          = (*session)(nil)
	_ driver.ExecerContext      = (*session)(nil)
	_ driver.NamedValueChecker  = (*session)(nil)
	_ driver.ConnPrepareContext = (*session)(nil)
)

// ResetSession refuses a connection left inside a transaction
func (s *session) ResetSession(context.Context) error {
	if !s.Raw().GetAutocommit() {
		return driver.ErrBadConn
	}
	return nil
}

func (s *session) IsValid() bool {
	return s.Raw().GetAutocommit()
}

func (s *session) ExecContext(ctx context.Context, query string, args []driver.NamedValue) (driver.Result, error) {
	return s.execer.ExecContext(ctx, query, args)
}

func (s *session) CheckNamedValue(arg *driver.NamedValue) error {
	return s.checker.CheckNamedValue(arg)
}

func (s *session) PrepareContext(ctx context.Context, query string) (driver.Stmt, error) {
	prepared, err := s.Conn.PrepareContext(ctx, query)
	if err != nil {
		return nil, err
	}

	execs, canExec := prepared.(driver.StmtExecContext)
	queries, canQuery := prepared.(driver.StmtQueryContext)
	checker, checks := prepared.(driver.NamedValueChecker)
	if !canExec || !canQuery || !checks {
		return nil, errors.Join(fmt.Errorf("SQLite statement %T is not the driver's", prepared), prepared.Close())
	}
	return &statement{
		Stmt: prepared, execs: execs, queries: queries, checker: checker,
		parameters: parametersOf(prepared),
	}, nil
}

// statement is a prepared statement that knows each parameter's name or position.
type statement struct {
	driver.Stmt
	execs      driver.StmtExecContext
	queries    driver.StmtQueryContext
	checker    driver.NamedValueChecker
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

// parametersOf keeps the positions SQLite assigned while compiling the SQL.
// A numbered parameter makes the driver leave argument counting to us.
func parametersOf(prepared driver.Stmt) []parameter {
	bindings, ok := prepared.(interface {
		BindCount() int
		BindName(int) string
	})
	if !ok {
		return nil
	}
	var parameters []parameter
	for i := 1; i <= bindings.BindCount(); i++ {
		name := bindings.BindName(i)
		if name != "" && name[0] == '?' || name == "$"+strconv.Itoa(i) {
			name = ""
		}
		parameters = append(parameters, parameter{position: i, name: name})
	}
	return parameters
}

// filled refuses arguments that leave a named parameter empty
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

func (s *statement) ExecContext(ctx context.Context, args []driver.NamedValue) (driver.Result, error) {
	if err := s.filled(args); err != nil {
		return nil, err
	}
	return s.execs.ExecContext(ctx, args)
}

func (s *statement) QueryContext(ctx context.Context, args []driver.NamedValue) (driver.Rows, error) {
	if err := s.filled(args); err != nil {
		return nil, err
	}
	return s.queries.QueryContext(ctx, args)
}

func (s *statement) CheckNamedValue(arg *driver.NamedValue) error {
	return s.checker.CheckNamedValue(arg)
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
