package sqldb

import (
	"context"
	"database/sql"
	"encoding/binary"
	"fmt"
	"math/rand/v2"
	"os"
	"path/filepath"
	"runtime"
	"slices"
	"strconv"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/tinyshed/tinystore/internal/sqlite"
)

// The engine's round: what the built sqldb costs on the paths the mechanics
// round measured in the prototype, on the same notes, in the same run as the
// prototype's own paths. Skipped unless TINYSTORE_SPIKE=1; TINYSTORE_SQL_DIR
// puts the files on a volume of their own, TINYSTORE_SQL_SECONDS sets a load.

func measuring(t *testing.T) {
	t.Helper()
	if os.Getenv("TINYSTORE_SPIKE") != "1" {
		t.Skip("a measurement: set TINYSTORE_SPIKE=1")
	}
}

func loadLength() time.Duration {
	if seconds, err := strconv.Atoi(os.Getenv("TINYSTORE_SQL_SECONDS")); err == nil && seconds > 0 {
		return time.Duration(seconds) * time.Second
	}
	return 3 * time.Second
}

// TestExecsMeasured inserts a message a call into the application's database
// from 1 to 512 goroutines for three seconds each, through Exec, whose writes
// share a commit, and through a Tx a call, which is what Exec was before: a
// commit, and its fsync, each.
func TestExecsMeasured(t *testing.T) {
	measuring(t)
	const insert = `insert into notes (title, body) values (?, ?)`
	for _, workers := range []int{1, 8, 64, 512} {
		for _, way := range []string{"a Tx each", "Exec"} {
			db := openMeasuredNotes(t)
			var next atomic.Int64
			result := measure(workers, loadLength(), func(int) error {
				n := next.Add(1)
				body := fmt.Sprint("see you at nine by the station, message ", n)
				if way == "Exec" {
					_, err := db.Exec(t.Context(), insert, fmt.Sprint("note ", n), body)
					return err
				}
				return db.Tx(t.Context(), func(tx *Tx) error {
					_, err := tx.Exec(t.Context(), insert, fmt.Sprint("note ", n), body)
					return err
				})
			})
			t.Logf("%3d goroutines  %-9s  inserts %s", workers, way, result)
		}
	}
}

func openMeasuredNotes(t *testing.T) *DB {
	t.Helper()
	db, err := Open(t.Context(), openStore(t, measuredDir(t)), "app", notesMigrations, nil)
	if err != nil {
		t.Fatal(err)
	}
	return db
}

func measuredDir(t *testing.T) string {
	t.Helper()
	root := os.Getenv("TINYSTORE_SQL_DIR")
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

// result is a load's calls a second and their latencies
type result struct {
	calls           int
	elapsed         time.Duration
	p50, p99, worst time.Duration
	failed          int
	firstErr        error
}

func (r result) String() string {
	text := fmt.Sprintf("%9.0f/s  p50 %8.1f µs  p99 %8.1f µs  worst %9.1f µs", float64(r.calls)/r.elapsed.Seconds(),
		micros(r.p50), micros(r.p99), micros(r.worst))
	if r.failed > 0 {
		text += fmt.Sprintf("  %d failed, first: %v", r.failed, r.firstErr)
	}
	return text
}

func micros(d time.Duration) float64 { return float64(d.Nanoseconds()) / 1000 }

// measure calls call from workers goroutines until length has passed
func measure(workers int, length time.Duration, call func(worker int) error) result {
	latencies := make([][]time.Duration, workers)
	failures := make([]error, workers)
	failed := make([]int, workers)
	started := time.Now()
	deadline := started.Add(length)
	var running sync.WaitGroup
	for worker := range workers {
		running.Go(func() {
			for {
				began := time.Now()
				if !began.Before(deadline) {
					return
				}
				if err := call(worker); err != nil {
					failed[worker]++
					if failures[worker] == nil {
						failures[worker] = err
					}
				}
				latencies[worker] = append(latencies[worker], time.Since(began))
			}
		})
	}
	running.Wait()
	all := slices.Concat(latencies...)
	slices.Sort(all)
	r := result{calls: len(all), elapsed: time.Since(started)}
	for worker := range workers {
		r.failed += failed[worker]
		if r.firstErr == nil {
			r.firstErr = failures[worker]
		}
	}
	if len(all) > 0 {
		r.p50, r.p99, r.worst = all[len(all)/2], all[len(all)*99/100], all[len(all)-1]
	}
	return r
}

// measuredNotes is a file of the design's notes, count of them written a
// transaction of ten thousand at a time, as the mechanics round's were: ids
// version 4 uuids as text, a hundred notes an author, a third done, one in four
// due
func measuredNotes(t *testing.T, count int) (*DB, design, string) {
	t.Helper()
	d := declareDesign(t)
	dir := measuredDir(t)
	db, err := Open(t.Context(), openStore(t, dir), "app", designMigrations(t), d.schema)
	if err != nil {
		t.Fatal(err)
	}
	ctx := t.Context()
	authors := max(count/100, 1)
	err = db.Tx(ctx, func(tx *Tx) error {
		for author := range authors {
			if _, insertErr := tx.Exec(ctx, `insert into users (id, email, display_name, created_at) values (?, ?, ?, 0)`,
				author, fmt.Sprint(author, "@example.com"), fmt.Sprint("author ", author)); insertErr != nil {
				return insertErr
			}
		}
		return nil
	})
	for first := 0; err == nil && first < count; first += 10_000 {
		err = db.Tx(ctx, func(tx *Tx) error {
			for i := first; i < min(first+10_000, count); i++ {
				if _, insertErr := tx.Exec(ctx, `insert into notes (id, author_id, title, done, tags, due, created_at)
					values (?, ?, ?, ?, ?, ?, ?)`, noteArguments(i, authors)...); insertErr != nil {
					return insertErr
				}
			}
			return nil
		})
	}
	if err != nil {
		t.Fatal(err)
	}
	return db, d, filepath.Join(dir, "sql", "app.db")
}

var measuredEpoch = time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC)

func noteArguments(i, authors int) []any {
	var due any
	if i%4 == 0 {
		due = DateOf(measuredEpoch.AddDate(0, 0, i%365))
	}
	return []any{
		noteID(i), i % authors, "a note about the station at nine, number " + strconv.Itoa(i),
		i%3 == 2, JSONOf([]string{"home", "errands"}), due, measuredEpoch.Add(time.Duration(i) * time.Second),
	}
}

func noteID(i int) UUID {
	random := rand.New(rand.NewPCG(uint64(i), 17))
	var id UUID
	binary.LittleEndian.PutUint64(id[:8], random.Uint64())
	binary.LittleEndian.PutUint64(id[8:], random.Uint64())
	id[6] = id[6]&0x0f | 0x40
	id[8] = id[8]&0x3f | 0x80
	return id
}

const noteByID = `select * from notes where id = ?`

// scanNote reads a note's row by hand, as the mechanics round's floor did:
// the scan, and the conversions a typed read makes
func scanNote(scan func(dest ...any) error) (Note, error) {
	var (
		note          Note
		id, tags      string
		done, created int64
		due           sql.NullString
	)
	if err := scan(&id, &note.AuthorID, &note.Title, &done, &tags, &due, &created); err != nil {
		return note, err
	}
	if err := parseUUID(id, note.ID[:]); err != nil {
		return note, err
	}
	if err := note.Tags.readJSON([]byte(tags)); err != nil {
		return note, err
	}
	if due.Valid {
		day, err := parseDate(due.String)
		if err != nil {
			return note, err
		}
		note.Due = &day
	}
	note.Done, note.CreatedAt = done == 1, time.UnixMilli(created).UTC()
	return note, nil
}

// TestPointReadsMeasured reads a note by its id from 1, 8 and 64 goroutines
// in a file of a million: through sqldb.One, and in the same run through the
// prototype's two paths, a prepared statement scanned by hand and a
// transaction compiling its statement each call, which was sqldb's before
func TestPointReadsMeasured(t *testing.T) {
	measuring(t)
	const count = 1_000_000
	began := time.Now()
	db, _, _ := measuredNotes(t, count)
	t.Logf("%d notes written in %v", count, time.Since(began).Round(time.Second))

	ways := []struct {
		name string
		read func(ctx context.Context, id UUID) error
	}{
		{"sqldb.One[Note]", func(ctx context.Context, id UUID) error {
			_, found, err := One[Note](ctx, db, noteByID, id)
			if err == nil && !found {
				err = sql.ErrNoRows
			}
			return err
		}},
		{"statement, scanned by hand", func(ctx context.Context, id UUID) error {
			return db.file.Lookup(ctx, func(r sqlite.Reader) error {
				_, err := scanNote(sqlite.QueryRow(ctx, r, noteByID, uuidText(id[:])).Scan)
				return err
			})
		}},
		{"transaction, compiled each call", func(ctx context.Context, id UUID) error {
			return db.file.View(ctx, func(tx *sql.Tx) error {
				_, err := scanNote(tx.QueryRowContext(ctx, noteByID, uuidText(id[:])).Scan)
				return err
			})
		}},
	}
	for _, way := range ways {
		for _, workers := range []int{1, 8, 64} {
			randoms := make([]*rand.Rand, workers)
			for worker := range randoms {
				randoms[worker] = rand.New(rand.NewPCG(uint64(worker), 23))
			}
			r := measure(workers, loadLength(), func(worker int) error {
				return way.read(context.Background(), noteID(randoms[worker].IntN(count)))
			})
			t.Logf("%-32s %2d goroutines  reads %s", way.name, workers, r)
		}
	}
}

// TestDecodingMeasured reads 200,000 notes, twice each way: All and Each into
// the model, and by hand through the same reader, and reports a row's time
// and allocations
func TestDecodingMeasured(t *testing.T) {
	measuring(t)
	const count = 200_000
	db, _, _ := measuredNotes(t, count)
	ctx := context.Background()
	const every = `select * from notes`
	ways := []struct {
		name string
		read func() (int, error)
	}{
		{"All[Note]", func() (int, error) {
			notes, err := All[Note](ctx, db, every)
			return len(notes), err
		}},
		{"Each[Note]", func() (int, error) {
			read := 0
			for _, err := range Each[Note](ctx, db, every) {
				if err != nil {
					return read, err
				}
				read++
			}
			return read, nil
		}},
		{"by hand", func() (read int, err error) {
			err = db.file.Lookup(ctx, func(r sqlite.Reader) error {
				rows, queryErr := r.QueryContext(ctx, every) //nolint:rowserrcheck // EachRow checks Err
				if queryErr != nil {
					return queryErr
				}
				return sqlite.EachRow(rows, "notes", func(rows *sql.Rows) error {
					_, scanErr := scanNote(rows.Scan)
					read++
					return scanErr
				})
			})
			return read, err
		}},
	}
	for range 2 {
		for _, way := range ways {
			var before, after runtime.MemStats
			runtime.GC()
			runtime.ReadMemStats(&before)
			began := time.Now()
			read, err := way.read()
			elapsed := time.Since(began)
			runtime.ReadMemStats(&after)
			if err != nil || read != count {
				t.Fatalf("%s read %d notes: %v", way.name, read, err)
			}
			t.Logf("%-10s %6.0f ns a row  %5.1f allocations a row", way.name,
				float64(elapsed.Nanoseconds())/float64(read), float64(after.Mallocs-before.Mallocs)/float64(read))
		}
	}
}

// TestInsertsMeasured inserts a note a call from 1 to 512 goroutines, all
// grouped: through Insert, which encodes the model's fields; through ExecOne
// and Exec of the same insert returning the row, and through Exec of it
// returning nothing, the columns already encoded, which is the floor
func TestInsertsMeasured(t *testing.T) {
	measuring(t)
	const insert = `insert into notes (id, author_id, title, done, tags, due, created_at) values (?, ?, ?, ?, ?, ?, ?)`
	encoded := func(i int) []any {
		id := noteID(i)
		return []any{
			uuidText(id[:]), 0, "a note about the station at nine", 0, `["home","errands"]`, "2026-01-01",
			measuredEpoch.UnixMilli(),
		}
	}
	ways := []struct {
		name  string
		write func(db *DB, d design, i int) error
	}{
		{"Insert", func(db *DB, d design, i int) error {
			due := DateOf(measuredEpoch)
			_, err := Insert(context.Background(), db, d.notes, Note{
				ID: noteID(i), Title: "a note about the station at nine",
				Tags: JSONOf([]string{"home", "errands"}), Due: &due, CreatedAt: measuredEpoch,
			})
			return err
		}},
		{"ExecOne, returning *", func(db *DB, _ design, i int) error {
			_, _, err := ExecOne[Note](context.Background(), db, insert+` returning *`, encoded(i)...)
			return err
		}},
		{"Exec, returning *", func(db *DB, _ design, i int) error {
			_, err := db.Exec(context.Background(), insert+` returning *`, encoded(i)...)
			return err
		}},
		{"Exec, encoded", func(db *DB, _ design, i int) error {
			_, err := db.Exec(context.Background(), insert, encoded(i)...)
			return err
		}},
	}
	for _, workers := range []int{1, 8, 64, 512} {
		for _, way := range ways {
			db, d, _ := measuredNotes(t, 100)
			var next atomic.Int64
			r := measure(workers, loadLength(), func(int) error {
				return way.write(db, d, int(next.Add(1))+1_000_000)
			})
			t.Logf("%3d goroutines  %-20s  inserts %s", workers, way.name, r)
		}
	}
}

// TestAsAProgramOpensItMeasured reads and writes the same notes through
// database/sql and modernc as a program that opens the file itself does, one
// pool for reads and writes, a busy timeout of five seconds, each query
// compiled for itself and each Exec a transaction of its own; and in the same
// run through sqldb, whose reads the pool's are compared with and whose
// grouped writes its Execs are
func TestAsAProgramOpensItMeasured(t *testing.T) {
	measuring(t)
	const count = 100_000
	db, _, path := measuredNotes(t, count)
	pool, err := sql.Open("sqlite", path+"?_pragma=busy_timeout(5000)")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = pool.Close() })
	ctx := context.Background()

	reads := []struct {
		name string
		read func(id UUID) error
	}{
		{"sqldb.One[Note]", func(id UUID) error {
			_, _, err := One[Note](ctx, db, noteByID, id)
			return err
		}},
		{"database/sql, as a program opens it", func(id UUID) error {
			_, err := scanNote(pool.QueryRowContext(ctx, noteByID, uuidText(id[:])).Scan)
			return err
		}},
	}
	for _, workers := range []int{1, 8, 64} {
		for _, way := range reads {
			randoms := make([]*rand.Rand, workers)
			for worker := range randoms {
				randoms[worker] = rand.New(rand.NewPCG(uint64(worker), 29))
			}
			r := measure(workers, loadLength(), func(worker int) error {
				return way.read(noteID(randoms[worker].IntN(count)))
			})
			t.Logf("%-36s %3d goroutines  reads   %s", way.name, workers, r)
		}
	}

	const insert = `insert into notes (id, author_id, title, done, tags, due, created_at) values (?, ?, ?, ?, ?, ?, ?)`
	var next atomic.Int64
	next.Store(2_000_000)
	writes := []struct {
		name string
		exec func(args []any) error
	}{
		{"sqldb Exec, grouped", func(args []any) error { _, err := db.Exec(ctx, insert, args...); return err }},
		{"database/sql, as a program opens it", func(args []any) error {
			_, err := pool.ExecContext(ctx, insert, args...)
			return err
		}},
	}
	for _, workers := range []int{1, 8, 64, 512} {
		for _, way := range writes {
			r := measure(workers, loadLength(), func(int) error {
				id := noteID(int(next.Add(1)))
				return way.exec([]any{
					uuidText(id[:]), 0, "a note about the station at nine", 0, `["home","errands"]`,
					nil, measuredEpoch.UnixMilli(),
				})
			})
			t.Logf("%-36s %3d goroutines  inserts %s", way.name, workers, r)
		}
	}
}

// TestOpenCheckMeasured opens a file of the design's schema twenty times with
// the schema to check it against and twenty without, and reports the median
func TestOpenCheckMeasured(t *testing.T) {
	measuring(t)
	d := declareDesign(t)
	dir := measuredDir(t)
	for _, way := range []struct {
		name   string
		schema *SchemaDef
	}{{"without a schema", nil}, {"checked against it", d.schema}} {
		var took []time.Duration
		for range 20 {
			store := openStore(t, dir)
			began := time.Now()
			if _, err := Open(t.Context(), store, "app", designMigrations(t), way.schema); err != nil {
				t.Fatal(err)
			}
			took = append(took, time.Since(began))
			if err := store.Close(t.Context()); err != nil {
				t.Fatal(err)
			}
		}
		slices.Sort(took)
		t.Logf("Open %-18s median %6.2f ms, slowest %6.2f ms", way.name, micros(took[10])/1000, micros(took[19])/1000)
	}
}
