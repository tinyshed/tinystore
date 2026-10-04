package server

import (
	"cmp"
	"context"
	"database/sql"
	"errors"
	"fmt"
	"io/fs"
	"maps"
	"slices"
	"strings"
	"testing/fstest"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/server/wire"
	"github.com/tinyshed/tinystore/sqldb"
)

// sqlHandle is a database a session opened
type sqlHandle struct {
	name string
	db   *sqldb.DB
}

func (s *Server) sqlMethods(methods map[wire.Method]handler) {
	methods[wire.SQLOpen] = sqlOpen
	methods[wire.SQLExec] = sqlExec
	methods[wire.SQLQuery] = sqlQuery
	methods[wire.SQLBatch] = sqlBatch
}

func sqlOpen(c *call) error {
	var ask wire.SQLDatabase
	if err := ask.Decode(c.request); err != nil {
		return err
	}
	migrations, err := migrationsOf(ask.Migrations)
	if err != nil {
		return err
	}
	db, err := c.session.server.database(c.ctx, ask.Name, migrations, c.session.capability)
	if err != nil {
		return err
	}
	return respond(c, wire.Handle{Handle: c.session.sqlHandles.add(&sqlHandle{name: ask.Name, db: db})})
}

// migrationsOf is a client's migration files as the directory the engine reads
// them from, a file system in memory; nil when it sent none
func migrationsOf(files []wire.SQLMigration) (fs.FS, error) {
	if len(files) == 0 {
		return nil, nil
	}
	directory := fstest.MapFS{}
	for _, file := range files {
		switch {
		case !fs.ValidPath(file.Name) || file.Name == "." || strings.Contains(file.Name, "/"):
			return nil, fmt.Errorf("%w: a migration named %q; a name is a file's, without a directory",
				tinystore.ErrInvalid, file.Name)
		case directory[file.Name] != nil:
			return nil, fmt.Errorf("%w: two migrations named %q", tinystore.ErrInvalid, file.Name)
		}
		directory[file.Name] = &fstest.MapFile{Data: []byte(file.Text)}
	}
	return directory, nil
}

// database is the database of name, opened the first time a client asks for it:
// an admin's open applies its migrations, and a data connection's finds them
// applied or is refused. An open without migrations takes the file as it is,
// an admin's making an empty one, a data connection's only one that is there.
//
// A database opens once a store, so every later open, and an open of one the
// program passed, checks the migrations it carries against those the file
// applied.
func (s *Server) database(ctx context.Context, name string, migrations fs.FS, capability wire.Capability) (*sqldb.DB,
	error,
) {
	s.sqlOpening.Lock()
	defer s.sqlOpening.Unlock()
	if db := s.databases[name]; db != nil {
		return db, migrated(ctx, db, name, migrations, capability)
	}
	var options []sqldb.OpenOption
	if capability != wire.Admin {
		options = append(options, sqldb.ApplyNone())
	}
	db, err := sqldb.Open(ctx, s.store, name, migrations, nil, options...)
	if errors.Is(err, sqldb.ErrPending) {
		return nil, fmt.Errorf("%w: an admin connection applies its migrations: %s", errDataConnection, err.Error())
	}
	if err != nil {
		return nil, passIt(err, fmt.Sprintf("SQL[%q]", name))
	}
	s.databases[name] = db
	return db, nil
}

// migrated checks the migrations an open of a database already open carries.
// One it has not applied waits for the server's next start, when an admin's
// open applies it.
func migrated(ctx context.Context, db *sqldb.DB, name string, migrations fs.FS, capability wire.Capability) error {
	if migrations == nil {
		return nil
	}
	err := db.Migrated(ctx, migrations)
	switch {
	case !errors.Is(err, sqldb.ErrPending):
		return err
	case capability != wire.Admin:
		return fmt.Errorf("%w: an admin connection applies its migrations: %s", errDataConnection, err.Error())
	}
	return fmt.Errorf("%w: sql %q is open, and a database applies its migrations as it opens: %s",
		tinystore.ErrInUse, name, err.Error())
}

// sqlExec runs a statement on the writer, grouped with the other writes
func sqlExec(c *call) error {
	var ask wire.SQLStatement
	if err := ask.Decode(c.request); err != nil {
		return err
	}
	handle, err := c.session.sqlHandles.get(ask.Handle)
	if err != nil {
		return err
	}
	ctx, cancel := c.session.statementContext(c.ctx)
	defer cancel()
	args := argumentsOf(ask)
	if err = c.session.mayRun(ctx, handle.db, ask.SQL, args); err != nil {
		return tookTooLong(ctx, err)
	}
	result, err := handle.db.Exec(ctx, ask.SQL, args...)
	if err != nil {
		return tookTooLong(ctx, err)
	}
	done, err := doneOf(result)
	if err != nil {
		return err
	}
	return respond(c, done)
}

// sqlQuery is a download: the columns, a row a DATA, and {}. A query reads on
// a reader, and one marked write on the writer, for a returning clause.
func sqlQuery(c *call) error {
	var ask wire.SQLStatement
	if err := ask.Decode(c.request); err != nil {
		return err
	}
	handle, err := c.session.sqlHandles.get(ask.Handle)
	if err != nil {
		return err
	}
	ctx, cancel := c.session.statementContext(c.ctx)
	defer cancel()
	args := argumentsOf(ask)
	if err = c.session.mayRun(ctx, handle.db, ask.SQL, args); err != nil {
		return tookTooLong(ctx, err)
	}
	read := sqldb.Query
	if ask.Write {
		read = sqldb.ExecQuery
	}
	rows, err := read(ctx, handle.db, ask.SQL, args...)
	if err != nil {
		return tookTooLong(ctx, err)
	}
	release, err := c.holdAnswer(func() int64 { return weighRows(rows) })
	if err != nil {
		return err
	}
	defer release()

	if err = begin(c, wire.SQLColumns{Columns: rows.Columns}); err != nil {
		return err
	}
	for _, values := range rows.Values {
		if err = item(c, wire.SQLRow{Values: values}); err != nil {
			return err
		}
	}
	return trailer(c, wire.Empty{})
}

// sqlBatch runs its statements in one transaction, all or none, or with read,
// from one snapshot; a data connection's are all checked before any runs
func sqlBatch(c *call) error {
	var ask wire.SQLStatements
	if err := ask.Decode(c.request); err != nil {
		return err
	}
	handle, err := c.session.sqlHandles.get(ask.Handle)
	if err != nil {
		return err
	}
	if len(ask.Statements) == 0 && len(ask.Jobs) == 0 && len(ask.KV) == 0 {
		return fmt.Errorf("%w: a batch of no statements", tinystore.ErrInvalid)
	}
	ctx, cancel := c.session.statementContext(c.ctx)
	defer cancel()
	args := make([][]any, len(ask.Statements))
	for i, statement := range ask.Statements {
		args[i] = argumentsOf(statement)
		if err = c.session.mayRun(ctx, handle.db, statement.SQL, args[i]); err != nil {
			return opFailed(i, tookTooLong(ctx, err))
		}
	}

	changes, err := changesIn(ctx, c.session, ask)
	if err != nil {
		return err
	}
	defer func() { changes.release(err) }()

	results := make([]wire.SQLResult, len(ask.Statements))
	run := func(tx *sqldb.Tx) error {
		for i, statement := range ask.Statements {
			result, runErr := runBatched(ctx, tx, statement, args[i], ask.Read)
			if runErr != nil {
				return opFailed(i, runErr)
			}
			results[i] = result
		}
		return changes.addTo(ctx, tx, len(ask.Statements))
	}
	if ask.Read {
		err = handle.db.View(ctx, run)
	} else {
		err = handle.db.Tx(ctx, run)
	}
	if err != nil {
		return tookTooLong(ctx, err)
	}
	return respond(c, wire.SQLResults{Results: results})
}

// errNotRun is what a job or a key a batch prepared is told when the batch
// ended before its transaction took it
var errNotRun = errors.New("server: the batch ended before its change was written")

// preparedChanges are the jobs and the keys a batch writes after its
// statements, each a change its transaction writes, and how many of them the
// transaction took
type preparedChanges struct {
	changes []sqldb.Change
	added   int
}

// addTo gives the transaction each change, after the statements, which it
// tells the transaction's outcome; a refused one is named after the statements
func (p *preparedChanges) addTo(ctx context.Context, tx *sqldb.Tx, statements int) error {
	for _, change := range p.changes {
		p.added++
		if err := tx.Add(ctx, change); err != nil {
			return opFailed(statements+p.added-1, err)
		}
	}
	return nil
}

// release tells each change the transaction never took that the batch ended
func (p *preparedChanges) release(err error) {
	for _, change := range p.changes[p.added:] {
		change.Done(cmp.Or(err, errNotRun))
	}
}

// changesIn prepares the jobs and then the keys a batch writes after its
// statements. It makes them before the transaction begins: a change waits for
// its queue's or its bucket's turn and its value's memory, which a
// transaction holding the writer must not.
func changesIn(ctx context.Context, s *session, ask wire.SQLStatements) (changes *preparedChanges, err error) {
	changes = &preparedChanges{}
	if (len(ask.Jobs) > 0 || len(ask.KV) > 0) && ask.Read {
		return nil, fmt.Errorf("%w: a job or a key in a view, which reads", tinystore.ErrInvalid)
	}
	defer func() {
		if err != nil {
			changes.release(err)
		}
	}()
	failed := func(err error) error { return opFailed(len(ask.Statements)+len(changes.changes), err) }
	for _, batch := range ask.Jobs {
		handle, err := s.jobsHandles.get(batch.Handle)
		if err != nil {
			return changes, failed(err)
		}
		queue, err := handle.queueOf("enqueue")
		if err != nil {
			return changes, failed(err)
		}
		for _, job := range batch.Jobs {
			value, err := jobValue(job.Value)
			if err != nil {
				return changes, failed(err)
			}
			options, err := enqueueOptions(job)
			if err != nil {
				return changes, failed(err)
			}
			changes.changes = append(changes.changes, queue.Enqueued(ctx, value, options...))
		}
	}
	for _, keys := range ask.KV {
		handle, err := s.kvHandles.get(keys.Handle)
		if err != nil {
			return changes, failed(err)
		}
		for _, call := range keys.Calls {
			change, err := handle.change(ctx, call)
			if err != nil {
				return changes, failed(err)
			}
			changes.changes = append(changes.changes, change)
		}
	}
	return changes, nil
}

// runBatched runs one statement of a batch: a read's rows, a write's rows
// when it asks for them, and otherwise what it changed
func runBatched(ctx context.Context, tx *sqldb.Tx, statement wire.SQLStatement, args []any, read bool) (
	wire.SQLResult, error,
) {
	var rows sqldb.Rows
	var err error
	switch {
	case read:
		rows, err = sqldb.Query(ctx, tx, statement.SQL, args...)
	case statement.Rows:
		rows, err = sqldb.ExecQuery(ctx, tx, statement.SQL, args...)
	default:
		result, execErr := tx.Exec(ctx, statement.SQL, args...)
		if execErr != nil {
			return wire.SQLResult{}, execErr
		}
		done, doneErr := doneOf(result)
		return wire.SQLResult{SQLDone: done}, doneErr
	}
	if err != nil {
		return wire.SQLResult{}, err
	}
	if rows.Columns == nil {
		rows.Columns = []string{}
	}
	return wire.SQLResult{Columns: rows.Columns, Rows: rows.Values}, nil
}

// argumentsOf is a statement's arguments as the engine takes them: the
// positional ones in their order, then each named one, as sql.Named names it
func argumentsOf(statement wire.SQLStatement) []any {
	args := slices.Clip(statement.Args)
	for _, name := range slices.Sorted(maps.Keys(statement.Named)) {
		args = append(args, sql.Named(name, statement.Named[name]))
	}
	return args
}

func doneOf(result sql.Result) (wire.SQLDone, error) {
	changes, err := result.RowsAffected()
	if err != nil {
		return wire.SQLDone{}, err
	}
	last, err := result.LastInsertId()
	return wire.SQLDone{Changes: changes, LastID: last}, err
}

// mayRun lets a statement run as the connection's capability allows: an
// admin's runs as it is, and a data connection's when its check passes
func (s *session) mayRun(ctx context.Context, db *sqldb.DB, statement string, args []any) error {
	if s.capability == wire.Admin {
		return nil
	}
	return checkDataSQL(ctx, db, statement, args)
}

// errStatementTook ends a data connection's statement at its deadline, since a
// recursive query can hold a reader or the writer for good
var errStatementTook = fmt.Errorf("%w: a data connection's statement ran past its deadline", tinystore.ErrLimit)

// statementContext bounds a data connection's statement, its check included;
// an admin's runs as long as it takes
func (s *session) statementContext(ctx context.Context) (context.Context, context.CancelFunc) {
	if s.capability == wire.Admin {
		return context.WithCancel(ctx)
	}
	return context.WithTimeoutCause(ctx, s.server.limits.statement, errStatementTook)
}

// tookTooLong says that a statement ended at its deadline rather than by its
// own failure. Only the text of how it stopped is kept, not the error itself. A
// write whose commit it interrupted stays outcome unknown.
func tookTooLong(ctx context.Context, err error) error {
	if errors.Is(context.Cause(ctx), errStatementTook) && !errors.Is(err, sqldb.ErrOutcomeUnknown) {
		return fmt.Errorf("%w: %s", errStatementTook, err.Error())
	}
	return err
}
