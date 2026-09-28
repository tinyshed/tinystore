package spike

import (
	"context"
	"database/sql"
	"encoding/binary"
	"encoding/hex"
	"errors"
	"math/rand/v2"
	"os"
	"path/filepath"
	"strconv"
	"testing"
	"time"

	"github.com/tinyshed/tinystore/internal/sqlite"
)

// The sqldb round of docs/sqldb.md measures the mechanics under its frozen
// first version before it is built: where a point read's time goes, what a
// bound limit and the planner's statistics cost a prepared statement, how many
// statements a connection should keep, what decoding a row into a struct and
// a struct into arguments costs, what a uuid key costs as text and as bytes,
// and what a migration does to a table's children and to the text of its
// checks. The measurements need TINYSTORE_SPIKE=1; TINYSTORE_SQLDB_DIR puts
// their files on a chosen disk and TINYSTORE_SQLDB_SECONDS sets each load's
// length.

// the notes table docs/sqldb.md prints for its model, without the reference
// to users, which no measurement here needs
var sqldbSchema = []string{
	`create table notes (
		id         text not null primary key,
		author_id  integer not null,
		title      text not null,
		done       integer not null default 0 check (done in (0, 1)),
		tags       text not null default '[]' check (json_valid(tags)),
		due        text check (due is date(due)),
		created_at integer not null
	) strict`,
	`create index notes_author_id_created_at on notes (author_id, created_at)`,
}

const (
	sqldbInsertNote = `insert into notes (id, author_id, title, done, tags, due, created_at)
		values (?, ?, ?, ?, ?, ?, ?)`
	sqldbNoteByID = `select id, author_id, title, done, tags, due, created_at from notes where id = ?`
)

// authors the notes are spread over, a hundred notes each in a million
const sqldbAuthors = 10_000

// the first note's time; each later note is a second after the one before
var sqldbEpoch = time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC).UnixMilli()

func sqldbMeasuring(t *testing.T) {
	t.Helper()
	if os.Getenv("TINYSTORE_SPIKE") != "1" {
		t.Skip("set TINYSTORE_SPIKE=1 to measure")
	}
}

// sqldbDir is a directory for one file: under TINYSTORE_SQLDB_DIR when it is
// set, so that a container writes to a volume rather than to the bind mount
func sqldbDir(t *testing.T) string {
	t.Helper()
	root := os.Getenv("TINYSTORE_SQLDB_DIR")
	if root == "" {
		return t.TempDir()
	}
	dir, err := os.MkdirTemp(root, "sqldb-")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = os.RemoveAll(dir) })
	return dir
}

func sqldbSeconds() time.Duration {
	if seconds, err := strconv.Atoi(os.Getenv("TINYSTORE_SQLDB_SECONDS")); err == nil && seconds > 0 {
		return time.Duration(seconds) * time.Second
	}
	return 3 * time.Second
}

// sqldbOpen opens a file through internal/sqlite, as the engine does; the
// test closes it
func sqldbOpen(t *testing.T, path string, readers int) *sqlite.File {
	t.Helper()
	file, err := sqlite.Open(t.Context(), path, sqlite.Config{Readers: readers})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = file.Close() })
	return file
}

// sqldbCreate makes the notes table and writes count notes to it, ten thousand
// a transaction
func sqldbCreate(t *testing.T, file *sqlite.File, count int) {
	t.Helper()
	ctx := t.Context()
	err := file.Update(ctx, func(tx *sql.Tx) error {
		for _, statement := range sqldbSchema {
			if _, err := tx.ExecContext(ctx, statement); err != nil {
				return err
			}
		}
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
	for first := 0; first < count; first += 10_000 {
		last := min(first+10_000, count)
		err = file.UpdatePrepared(ctx, func(w sqlite.Writer) error { return sqldbInsertNotes(ctx, w, first, last) })
		if err != nil {
			t.Fatal(err)
		}
	}
}

// sqldbInsertNotes writes the notes numbered first up to last
func sqldbInsertNotes(ctx context.Context, w sqlite.Writer, first, last int) error {
	for i := first; i < last; i++ {
		if _, err := w.ExecContext(ctx, sqldbInsertNote, sqldbNoteArgs(i)...); err != nil {
			return err
		}
	}
	return nil
}

// sqldbNoteArgs are the i-th note's columns, the same in every run: a third of
// the notes done, one in four with a due date
func sqldbNoteArgs(i int) []any {
	var due any
	if i%4 == 0 {
		due = time.UnixMilli(sqldbEpoch).AddDate(0, 0, i%365).Format(time.DateOnly)
	}
	return []any{
		sqldbNoteID(i),
		i % sqldbAuthors,
		"a note about the station at nine, number " + strconv.Itoa(i),
		i % 3 / 2,
		`["home","errands"]`,
		due,
		sqldbEpoch + int64(i)*1000,
	}
}

// sqldbNoteID is the i-th note's id, a version 4 uuid in its text form
func sqldbNoteID(i int) string {
	return sqldbUUIDText(sqldbUUID(i))
}

func sqldbUUID(i int) [16]byte {
	random := rand.New(rand.NewPCG(uint64(i), 17))
	var id [16]byte
	binary.LittleEndian.PutUint64(id[:8], random.Uint64())
	binary.LittleEndian.PutUint64(id[8:], random.Uint64())
	id[6] = id[6]&0x0f | 0x40
	id[8] = id[8]&0x3f | 0x80
	return id
}

// sqldbUUIDText spells a uuid as its text: 8-4-4-4-12 lower-case hex digits
func sqldbUUIDText(id [16]byte) string {
	digits := hex.EncodeToString(id[:])
	return digits[0:8] + "-" + digits[8:12] + "-" + digits[12:16] + "-" + digits[16:20] + "-" + digits[20:32]
}

// sqldbScanNote reads a note's row through whatever scans it, and says a
// missing row is wrong: every measured read asks for a note that exists
func sqldbScanNote(scan func(dest ...any) error) error {
	var (
		id, title, tags string
		author, done    int64
		created         int64
		due             sql.NullString
	)
	err := scan(&id, &author, &title, &done, &tags, &due, &created)
	if errors.Is(err, sql.ErrNoRows) {
		return errKVWrongAnswer
	}
	return err
}

// sqldbReadWay is one way to run the same point read
type sqldbReadWay struct {
	name string
	read func(ctx context.Context, id string) error
}

// sqldbReadWays are the three paths a point read can take through internal/sqlite:
// what sqldb does today, a transaction around a prepared statement, and the
// prepared statement alone
func sqldbReadWays(file *sqlite.File) []sqldbReadWay {
	return []sqldbReadWay{
		{"transaction, compiled each call", func(ctx context.Context, id string) error {
			return file.View(ctx, func(tx *sql.Tx) error {
				return sqldbScanNote(tx.QueryRowContext(ctx, sqldbNoteByID, id).Scan)
			})
		}},
		{"transaction, prepared", func(ctx context.Context, id string) error {
			return file.ViewPrepared(ctx, func(r sqlite.Reader) error {
				return sqldbScanNote(sqlite.QueryRow(ctx, r, sqldbNoteByID, id).Scan)
			})
		}},
		{"statement, prepared", func(ctx context.Context, id string) error {
			return file.Lookup(ctx, func(r sqlite.Reader) error {
				return sqldbScanNote(sqlite.QueryRow(ctx, r, sqldbNoteByID, id).Scan)
			})
		}},
	}
}

// TestSQLDBPointReads reads a note by its id from 1, 8 and 64 goroutines in a
// file of a million notes, through four readers and through eight, each way a
// read can take
func TestSQLDBPointReads(t *testing.T) {
	sqldbMeasuring(t)
	const count = 1_000_000
	path := filepath.Join(sqldbDir(t), "app.db")
	began := time.Now()
	writer := sqldbOpen(t, path, 1)
	sqldbCreate(t, writer, count)
	t.Logf("%d notes written in %v; %d MiB with the log", count, time.Since(began).Round(time.Second),
		kvFileBytes(path)>>20)

	for _, readers := range []int{4, 8} {
		file := sqldbOpen(t, path, readers)
		for _, way := range sqldbReadWays(file) {
			for _, workers := range []int{1, 8, 64} {
				randoms := make([]*rand.Rand, workers)
				for worker := range randoms {
					randoms[worker] = rand.New(rand.NewPCG(uint64(worker), 23))
				}
				result := kvLoad(workers, sqldbSeconds(), func(worker int) error {
					return way.read(t.Context(), sqldbNoteID(randoms[worker].IntN(count)))
				})
				t.Logf("%d readers  %-32s %2d goroutines  reads %s", readers, way.name, workers, result)
			}
		}
		if err := file.Close(); err != nil {
			t.Fatal(err)
		}
	}
}

// TestSQLDBBoundLimits pages through one author's notes on a single reader,
// the statement prepared once, with its limit written as a literal, as a bare
// parameter and as a parameter cast to an integer; and a range read by an
// indexed column. Each runs before the file has statistics and after analyze,
// whose sqlite_stat4 lets the planner read bound values
func TestSQLDBBoundLimits(t *testing.T) {
	sqldbMeasuring(t)
	const count = 1_000_000
	path := filepath.Join(sqldbDir(t), "app.db")
	file := sqldbOpen(t, path, 1)
	sqldbCreate(t, file, count)

	const page = `select id, title, created_at from notes where author_id = ? order by created_at desc `
	queries := []struct {
		name, query string
		args        func(random *rand.Rand) []any
	}{
		{"limit 20", page + `limit 20`, func(r *rand.Rand) []any { return []any{r.IntN(sqldbAuthors)} }},
		{"limit ?", page + `limit ?`, func(r *rand.Rand) []any { return []any{r.IntN(sqldbAuthors), 20} }},
		{
			"limit cast(? as integer)", page + `limit cast(? as integer)`,
			func(r *rand.Rand) []any { return []any{r.IntN(sqldbAuthors), 20} },
		},
		{
			"a range, limit 20", `select id, title from notes where author_id = ? and created_at > ?
			order by created_at limit 20`,
			func(r *rand.Rand) []any {
				return []any{r.IntN(sqldbAuthors), sqldbEpoch + int64(r.IntN(count))*1000}
			},
		},
	}
	measure := func(phase string) {
		for _, query := range queries {
			random := rand.New(rand.NewPCG(5, 7))
			result := kvLoad(1, sqldbSeconds(), func(int) error {
				return file.Lookup(t.Context(), func(r sqlite.Reader) error {
					//nolint:rowserrcheck // EachRow checks Err
					rows, err := r.QueryContext(t.Context(), query.query, query.args(random)...)
					if err != nil {
						return err
					}
					return sqlite.EachRow(rows, "notes", func(*sql.Rows) error { return nil })
				})
			})
			t.Logf("%-15s %-26s %s", phase, query.name, result)
		}
	}

	measure("no statistics")
	if err := file.Update(t.Context(), func(tx *sql.Tx) error {
		_, err := tx.ExecContext(t.Context(), `analyze`)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	var samples int
	if err := file.View(t.Context(), func(tx *sql.Tx) error {
		return tx.QueryRowContext(t.Context(), `select count(*) from sqlite_stat4`).Scan(&samples)
	}); err != nil {
		t.Fatalf("sqlite_stat4 after analyze: %v", err)
	}
	t.Logf("analyze wrote %d sqlite_stat4 samples", samples)
	measure("after analyze")
}
