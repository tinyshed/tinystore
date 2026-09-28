package spike

import (
	"container/list"
	"context"
	"database/sql"
	"errors"
	"fmt"
	"math/rand/v2"
	"path/filepath"
	"testing"
	"time"

	sqlite3 "modernc.org/sqlite"
)

// sqldbStatementCache keeps at most size prepared statements of one
// connection, the least recently used closed first, as internal/sqlite's
// readers do with thirty-two
type sqldbStatementCache struct {
	conn       *sql.Conn
	size       int
	statements map[string]*list.Element
	order      *list.List // most recent at the front
	prepares   int
}

type sqldbCached struct {
	query     string
	statement *sql.Stmt
}

func (c *sqldbStatementCache) get(ctx context.Context, query string) (*sql.Stmt, error) {
	if element, ok := c.statements[query]; ok {
		c.order.MoveToFront(element)
		cached, _ := element.Value.(*sqldbCached)
		return cached.statement, nil
	}
	if c.order.Len() == c.size {
		oldest := c.order.Back()
		cached, _ := oldest.Value.(*sqldbCached)
		if err := cached.statement.Close(); err != nil {
			return nil, err
		}
		delete(c.statements, cached.query)
		c.order.Remove(oldest)
	}
	statement, err := c.conn.PrepareContext(ctx, query)
	if err != nil {
		return nil, err
	}
	c.prepares++
	c.statements[query] = c.order.PushFront(&sqldbCached{query: query, statement: statement})
	return statement, nil
}

// read reads the note id through the cache's statement for query
func (c *sqldbStatementCache) read(ctx context.Context, query, id string) error {
	statement, err := c.get(ctx, query) //nolint:sqlclosecheck // the cache keeps it until it evicts it
	if err != nil {
		return err
	}
	return sqldbScanNote(statement.QueryRowContext(ctx, id).Scan)
}

// sqldbPrepareAndRun prepares query on conn and reads the note id through it
// runs times, keeping the statement for the caller to close
func sqldbPrepareAndRun(ctx context.Context, conn *sql.Conn, query, id string, runs int) (*sql.Stmt, error) {
	statement, err := conn.PrepareContext(ctx, query)
	if err != nil {
		return nil, err
	}
	for range runs {
		if err = sqldbScanNote(statement.QueryRowContext(ctx, id).Scan); err != nil {
			return nil, errors.Join(err, statement.Close()) //nolint:sqlclosecheck // kept unless its run failed
		}
	}
	return statement, nil
}

// sqldbCompileAndClose prepares query on conn and closes it at once: what a
// cache's miss costs before its statement runs
func sqldbCompileAndClose(ctx context.Context, conn *sql.Conn, query string) (err error) {
	statement, err := conn.PrepareContext(ctx, query)
	if err != nil {
		return err
	}
	defer func() { err = errors.Join(err, statement.Close()) }()
	return nil
}

// sqldbStatementBytes is what SQLite says the connection's prepared
// statements hold
func sqldbStatementBytes(ctx context.Context, conn *sql.Conn) (int, error) {
	return sqldbStatus(conn, sqlite3.DBStatusStmtUsed, false)
}

// sqldbStatus reads one of the connection's counters, and resets it when asked
func sqldbStatus(conn *sql.Conn, op sqlite3.DBStatusOp, reset bool) (int, error) {
	var current int
	err := conn.Raw(func(driverConn any) error {
		status, ok := driverConn.(sqlite3.DBStatus)
		if !ok {
			return fmt.Errorf("the connection reports no status")
		}
		var err error
		current, _, err = status.Status(op, reset)
		return err
	})
	return current, err
}

// TestSQLDBStatementCache runs an application's point reads spread over 16 to
// 1024 distinct texts, uniformly, on one reader connection whose cache keeps
// 32, 128 or 512 prepared statements, and against preparing each call; it
// reports a read's time, how often it compiled, and what the kept statements
// hold. The texts differ in a comment, so that each compiles as much as the
// others
func TestSQLDBStatementCache(t *testing.T) {
	sqldbMeasuring(t)
	const count = 100_000
	path := filepath.Join(sqldbDir(t), "app.db")
	file := sqldbOpen(t, path, 1)
	sqldbCreate(t, file, count)
	ctx := t.Context()
	pool := kvReaderPool(t, path, "cache_size(-1024)")

	for _, texts := range []int{16, 128, 1024} {
		queries := make([]string, texts)
		for i := range queries {
			queries[i] = fmt.Sprintf("%s /* read %d */", sqldbNoteByID, i)
		}
		for _, size := range []int{0, 32, 128, 512} {
			conn, err := pool.Conn(ctx)
			if err != nil {
				t.Fatal(err)
			}
			cache := &sqldbStatementCache{
				conn: conn, size: max(size, 1), statements: map[string]*list.Element{},
				order: list.New(),
			}
			random := rand.New(rand.NewPCG(3, 9))
			reads := 0
			for _, op := range []sqlite3.DBStatusOp{sqlite3.DBStatusLookasideHit, sqlite3.DBStatusLookasideMissFull} {
				if _, err = sqldbStatus(conn, op, true); err != nil {
					t.Fatal(err)
				}
			}
			result := kvLoad(1, sqldbSeconds(), func(int) error {
				reads++
				query := queries[random.IntN(texts)]
				id := sqldbNoteID(random.IntN(count))
				if size == 0 {
					return sqldbScanNote(conn.QueryRowContext(ctx, query, id).Scan)
				}
				return cache.read(ctx, query, id)
			})
			held, err := sqldbStatementBytes(ctx, conn)
			if err != nil {
				t.Fatal(err)
			}
			hits, err := sqldbStatus(conn, sqlite3.DBStatusLookasideHit, false)
			if err != nil {
				t.Fatal(err)
			}
			full, err := sqldbStatus(conn, sqlite3.DBStatusLookasideMissFull, false)
			if err != nil {
				t.Fatal(err)
			}
			compiled := cache.prepares
			if size == 0 {
				compiled = reads
			}
			t.Logf("%4d texts  cache %3d  compiled %5.1f%%  statements hold %6.1f KiB  "+
				"small allocations past lookaside %5.1f%%  reads %s",
				texts, size, 100*float64(compiled)/float64(reads), float64(held)/1024,
				100*float64(full)/float64(max(hits+full, 1)), result)
			for _, element := range cache.statements {
				cached, _ := element.Value.(*sqldbCached)
				_ = cached.statement.Close()
			}
			if err = conn.Close(); err != nil {
				t.Fatal(err)
			}
		}
	}
}

// TestSQLDBCompileBesideKept compiles and closes a point read 5,000 times on a
// connection that keeps 0 to 1024 other prepared statements, to see whether a
// compile costs more beside a larger cache
func TestSQLDBCompileBesideKept(t *testing.T) {
	sqldbMeasuring(t)
	path := filepath.Join(sqldbDir(t), "app.db")
	file := sqldbOpen(t, path, 1)
	sqldbCreate(t, file, 1000)
	ctx := t.Context()
	pool := kvReaderPool(t, path, "cache_size(-1024)")
	for _, kept := range []int{0, 32, 128, 512, 1024} {
		conn, err := pool.Conn(ctx)
		if err != nil {
			t.Fatal(err)
		}
		statements := make([]*sql.Stmt, kept)
		for i := range statements {
			//nolint:sqlclosecheck // kept beside the compiles, closed below
			statements[i], err = sqldbPrepareAndRun(ctx, conn, fmt.Sprintf("%s /* kept %d */", sqldbNoteByID, i),
				sqldbNoteID(i%1000), 1)
			if err != nil {
				t.Fatal(err)
			}
		}
		const compiled = 5000
		began := time.Now()
		for i := range compiled {
			if err = sqldbCompileAndClose(ctx, conn, fmt.Sprintf("%s /* fresh %d */", sqldbNoteByID, i)); err != nil {
				t.Fatal(err)
			}
		}
		elapsed := time.Since(began)
		var direct time.Duration
		began = time.Now()
		for i := range compiled {
			if err = sqldbScanNote(conn.QueryRowContext(ctx, fmt.Sprintf("%s /* direct %d */", sqldbNoteByID, i),
				sqldbNoteID(i%1000)).Scan); err != nil {
				t.Fatal(err)
			}
		}
		direct = time.Since(began)
		t.Logf("%4d statements kept: a compile and close %6.1f µs; a read compiled for itself %6.1f µs", kept,
			float64(elapsed.Nanoseconds())/compiled/1000, float64(direct.Nanoseconds())/compiled/1000)
		for _, statement := range statements {
			_ = statement.Close()
		}
		if err = conn.Close(); err != nil {
			t.Fatal(err)
		}
	}
}

// TestSQLDBClosingAStatement times closing 5,000 prepared point reads that
// never ran, that ran once, and that ran twenty times, to find what an
// evicting cache pays for each statement it lets go
func TestSQLDBClosingAStatement(t *testing.T) {
	sqldbMeasuring(t)
	path := filepath.Join(sqldbDir(t), "app.db")
	file := sqldbOpen(t, path, 1)
	sqldbCreate(t, file, 1000)
	ctx := t.Context()
	pool := kvReaderPool(t, path, "cache_size(-1024)")
	conn, err := pool.Conn(ctx)
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = conn.Close() }()
	for _, runs := range []int{0, 1, 20} {
		const count = 5000
		statements := make([]*sql.Stmt, count)
		for i := range statements {
			//nolint:sqlclosecheck // closed together below, which is what this times
			statements[i], err = sqldbPrepareAndRun(ctx, conn, fmt.Sprintf("%s /* %d ran %d */", sqldbNoteByID, i, runs),
				sqldbNoteID(i%1000), runs)
			if err != nil {
				t.Fatal(err)
			}
		}
		began := time.Now()
		for _, statement := range statements {
			if err = statement.Close(); err != nil {
				t.Fatal(err)
			}
		}
		t.Logf("a statement that ran %2d times closes in %6.1f µs", runs,
			float64(time.Since(began).Nanoseconds())/count/1000)
	}
}

// TestSQLDBStatementSize prepares statements of growing weight on one
// connection and reports what each holds, so that a cache's size can be said
// in bytes
func TestSQLDBStatementSize(t *testing.T) {
	sqldbMeasuring(t)
	path := filepath.Join(sqldbDir(t), "app.db")
	file := sqldbOpen(t, path, 1)
	sqldbCreate(t, file, 1000)
	ctx := t.Context()
	pool := kvReaderPool(t, path, "cache_size(-1024)")
	conn, err := pool.Conn(ctx)
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = conn.Close() }()

	for _, query := range []struct{ name, text string }{
		{"a point read", sqldbNoteByID},
		{"an author's page", `select id, title, created_at from notes where author_id = ? order by created_at desc limit 20`},
		{"a report by day", `select date(created_at / 1000, 'unixepoch') as day, count(*) as notes, sum(done) as done
			from notes where created_at between ? and ? group by day order by day`},
		{"a join of four", `select a.id, b.title, c.title, d.title from notes a
			join notes b on b.author_id = a.author_id and b.id <> a.id
			join notes c on c.author_id = b.author_id and c.created_at > b.created_at
			join notes d on d.author_id = c.author_id and d.done = 1
			where a.author_id = ? and json_array_length(a.tags) > 0 order by d.created_at desc limit 10`},
	} {
		before, err := sqldbStatementBytes(ctx, conn)
		if err != nil {
			t.Fatal(err)
		}
		const kept, compiled = 10, 2000
		statements := make([]*sql.Stmt, kept)
		for i := range statements {
			//nolint:sqlclosecheck // kept to be weighed, closed below
			if statements[i], err = sqldbPrepareAndRun(ctx, conn, fmt.Sprintf("%s /* %d */", query.text, i), "", 0); err != nil {
				t.Fatal(err)
			}
		}
		after, err := sqldbStatementBytes(ctx, conn)
		if err != nil {
			t.Fatal(err)
		}
		began := time.Now()
		for i := range compiled {
			if err = sqldbCompileAndClose(ctx, conn, fmt.Sprintf("%s /* again %d */", query.text, i)); err != nil {
				t.Fatal(err)
			}
		}
		compile := time.Since(began) / compiled
		t.Logf("%-16s %6.1f KiB a statement, compiled and closed in %5.1f µs", query.name,
			float64(after-before)/kept/1024, float64(compile.Nanoseconds())/1000)
		for _, statement := range statements {
			_ = statement.Close()
		}
	}
}
