package spike

import (
	"cmp"
	"context"
	"database/sql"
	"encoding/base32"
	"encoding/binary"
	"errors"
	"fmt"
	"math/rand/v2"
	"net/url"
	"os"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/tinyshed/tinystore/internal/sqlite"
)

// The kv round of docs/kv.md measures the mechanics under the agreed API before
// a kv package exists: what a durable Set costs, what a point Get costs, how
// fast a LoseAtMost flush writes, how the layout divides the file, what Clear
// holds the writer for, and what production traffic those ceilings meet. The
// measurements need TINYSTORE_SPIKE=1; TINYSTORE_KV_DIR puts their files on a
// chosen disk, TINYSTORE_KV_SECONDS sets each load's length and
// TINYSTORE_KV_LARGE=1 adds ten million keys to the point reads.

// the layout docs/kv.md proposes: every kind in one narrow table, its expiry
// index, values past the inline threshold, and the revision's high-water mark
var kvSchema = []string{
	`create table cells (
		bucket  integer not null,
		path    blob    not null,
		version integer not null,
		expires integer,
		value,
		spill   integer,
		primary key (bucket, path)
	) without rowid`,
	`create index cells_expiry on cells (expires, bucket, path) where expires is not null`,
	`create table spilled (id integer primary key, value blob not null)`,
	`create table meta (name text primary key, value) without rowid`,
	`insert into meta (name, value) values ('revision', 0)`,
}

const (
	kvUpsert = `insert into cells (bucket, path, version, expires, value) values (?1, ?2, ?3, ?4, ?5)
		on conflict (bucket, path) do update set
			version = excluded.version, expires = excluded.expires, value = excluded.value`
	kvRevision = `update meta set value = ?1 where name = 'revision'`
	kvGet      = `select version, expires, value, spill from cells
		where bucket = ?1 and path = ?2 and (expires is null or expires > ?3)`
)

// seeds of the populations a measurement draws keys from
const (
	kvSeedStored = 1 // the sessions a file holds
	kvSeedAbsent = 2 // keys a file does not hold
	kvSeedNew    = 3 // keys a write adds
)

// readers a point-read measurement opens, as many as a store would give kv
const kvReaders = 8

func kvMeasuring(t *testing.T) {
	t.Helper()
	if os.Getenv("TINYSTORE_SPIKE") != "1" {
		t.Skip("set TINYSTORE_SPIKE=1 to measure")
	}
}

// kvDir is a directory for one file: under TINYSTORE_KV_DIR when it is set, so
// that a container writes to a volume rather than to the bind mount
func kvDir(t *testing.T) string {
	t.Helper()
	root := os.Getenv("TINYSTORE_KV_DIR")
	if root == "" {
		return t.TempDir()
	}
	dir, err := os.MkdirTemp(root, "kv-")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = os.RemoveAll(dir) })
	return dir
}

func kvSeconds() time.Duration {
	if seconds, err := strconv.Atoi(os.Getenv("TINYSTORE_KV_SECONDS")); err == nil && seconds > 0 {
		return time.Duration(seconds) * time.Second
	}
	return 3 * time.Second
}

// kvOpen opens a file through internal/sqlite, as an engine would, and creates
// the layout; the test closes it
func kvOpen(t *testing.T, path string, pageSize, readers int) *sqlite.File {
	t.Helper()
	ctx := t.Context()
	file, err := sqlite.Open(ctx, path, sqlite.Config{Readers: readers, PageSize: pageSize})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = file.Close() })
	if err = file.Update(ctx, func(tx *sql.Tx) error { return kvCreate(ctx, tx) }); err != nil {
		t.Fatal(err)
	}
	return file
}

func kvCreate(ctx context.Context, tx *sql.Tx) error {
	for _, statement := range kvSchema {
		if _, err := tx.ExecContext(ctx, statement); err != nil {
			return err
		}
	}
	return nil
}

// kvReaderPool opens connections to path as internal/sqlite opens its readers,
// query_only and deferred, with the given cache pragma, for statements that run
// without a transaction of their own
func kvReaderPool(t *testing.T, path, cache string) *sql.DB {
	t.Helper()
	abs, err := filepath.Abs(path)
	if err != nil {
		t.Fatal(err)
	}
	arguments := url.Values{"mode": {"rw"}, "_txlock": {"deferred"}}
	for _, pragma := range []string{"foreign_keys(1)", "busy_timeout(5000)", "synchronous(FULL)", cache, "query_only(1)"} {
		arguments.Add("_pragma", pragma)
	}
	uri := &url.URL{Scheme: "file", Path: filepath.ToSlash(abs)}
	if !strings.HasPrefix(uri.Path, "/") {
		uri.Path = "/" + uri.Path
	}
	uri.RawQuery = arguments.Encode()
	db, err := sql.Open("sqlite", uri.String())
	if err != nil {
		t.Fatal(err)
	}
	db.SetMaxOpenConns(kvReaders)
	db.SetMaxIdleConns(kvReaders)
	t.Cleanup(func() { _ = db.Close() })
	return db
}

// kvPath writes owners and a key as docs/kv.md does, so that a branch is one
// range and a name may hold any byte:
//
//	Of("tenant-7", "42"), "iPhone" → 01 tenant-7 00 · 01 42 00 · 02 iPhone
func kvPath(key string, owners ...string) []byte {
	var path []byte
	for _, owner := range owners {
		path = append(path, 0x01)
		path = kvEscape(path, owner)
		path = append(path, 0x00)
	}
	path = append(path, 0x02)
	return kvEscape(path, key)
}

// kvEscape writes a 00 inside a name as 00 FF
func kvEscape(path []byte, name string) []byte {
	for i := range len(name) {
		path = append(path, name[i])
		if name[i] == 0x00 {
			path = append(path, 0xff)
		}
	}
	return path
}

var kvTokens = base32.StdEncoding.WithPadding(base32.NoPadding)

// kvSession is session i of a population: user i/3, and a token spelled as
// crypto/rand.Text spells one
func kvSession(seed uint64, i int) []byte {
	random := rand.New(rand.NewPCG(seed, uint64(i)))
	var token [16]byte
	binary.LittleEndian.PutUint64(token[:8], random.Uint64())
	binary.LittleEndian.PutUint64(token[8:], random.Uint64())
	return kvPath(kvTokens.EncodeToString(token[:]), strconv.Itoa(i/3))
}

// kvValue is value i: size bytes nothing compresses, as SQLite would not anyway
func kvValue(i, size int) []byte {
	random := rand.New(rand.NewPCG(uint64(i), 7))
	value := make([]byte, size)
	var word [8]byte
	for at := 0; at < size; at += len(word) {
		binary.LittleEndian.PutUint64(word[:], random.Uint64())
		copy(value[at:], word[:])
	}
	return value
}

// kvExpiry is a session's: thirty days from now, spread over a day by i so that
// the expiry index is not one value
func kvExpiry(i int) int64 {
	return time.Now().Add(30*24*time.Hour).UnixMilli() + int64(i%86_400)*1000
}

// kvPreload writes sessions 0 to count-1 of kvSeedStored in a scattered order,
// as users sign in, fifty thousand a transaction
func kvPreload(t *testing.T, file *sqlite.File, count, valueSize int) {
	t.Helper()
	ctx := t.Context()
	step := kvStride(count)
	for start := 0; start < count; start += 50_000 {
		end := min(start+50_000, count)
		err := file.UpdatePrepared(ctx, func(w sqlite.Writer) error {
			for n := start; n < end; n++ {
				i := n * step % count
				if _, err := w.ExecContext(ctx, kvUpsert, 1, kvSession(kvSeedStored, i), i+1, kvExpiry(i),
					kvValue(i, valueSize)); err != nil {
					return err
				}
			}
			_, err := w.ExecContext(ctx, kvRevision, count)
			return err
		})
		if err != nil {
			t.Fatal(err)
		}
	}
}

// kvStride is a step near count/φ with no factor in common with count, so that
// n·step mod count visits every session once, scattered
func kvStride(count int) int {
	step := max(int(float64(count)*0.618), 1)
	for kvDivisor(step, count) != 1 {
		step++
	}
	return step
}

func kvDivisor(a, b int) int {
	for b != 0 {
		a, b = b, a%b
	}
	return a
}

// kvResult is one load: operations finished, the time they took, and the
// latency of one operation from its call to its return, waiting included
type kvResult struct {
	ops, failed     int
	firstErr        error
	elapsed         time.Duration
	p50, p99, worst time.Duration
}

func (r kvResult) perSecond() float64 {
	return float64(r.ops) / r.elapsed.Seconds()
}

func (r kvResult) String() string {
	line := fmt.Sprintf("%10.0f/s  p50 %s  p99 %s  worst %s", r.perSecond(), kvMicros(r.p50), kvMicros(r.p99),
		kvMicros(r.worst))
	if r.failed > 0 {
		line += fmt.Sprintf("  %d failed, first: %v", r.failed, r.firstErr)
	}
	return line
}

func kvMicros(d time.Duration) string {
	return fmt.Sprintf("%9.1f µs", float64(d)/float64(time.Microsecond))
}

// kvLoad runs op from workers goroutines until the time is up
func kvLoad(workers int, duration time.Duration, op func(worker int) error) kvResult {
	latencies := make([][]time.Duration, workers)
	failures := make([]error, workers)
	failed := make([]int, workers)
	start := time.Now()
	deadline := start.Add(duration)
	var wg sync.WaitGroup
	for worker := range workers {
		wg.Go(func() {
			for {
				began := time.Now()
				if !began.Before(deadline) {
					return
				}
				if err := op(worker); err != nil {
					failed[worker]++
					failures[worker] = cmp.Or(failures[worker], err)
				}
				latencies[worker] = append(latencies[worker], time.Since(began))
			}
		})
	}
	wg.Wait()
	all := slices.Concat(latencies...)
	slices.Sort(all)
	result := kvResult{ops: len(all), elapsed: time.Since(start)}
	for worker := range workers {
		result.failed += failed[worker]
		result.firstErr = cmp.Or(result.firstErr, failures[worker])
	}
	if len(all) > 0 {
		result.p50 = all[len(all)/2]
		result.p99 = all[len(all)*99/100]
		result.worst = all[len(all)-1]
	}
	return result
}

// kvFileBytes is the file and its write-ahead log
func kvFileBytes(path string) int64 {
	var total int64
	for _, name := range []string{path, path + "-wal"} {
		if info, err := os.Stat(name); err == nil {
			total += info.Size()
		}
	}
	return total
}

var errKVWrongAnswer = errors.New("a lookup found what it should not have, or missed what it should have found")

// kvGetOn reads one key through a statement that runs without a transaction of
// its own
func kvGetOn(ctx context.Context, statement *sql.Stmt, path []byte) (bool, error) {
	var (
		version        int64
		expires, spill sql.NullInt64
		value          []byte
	)
	err := statement.QueryRowContext(ctx, 1, path, time.Now().UnixMilli()).Scan(&version, &expires, &value, &spill)
	if errors.Is(err, sql.ErrNoRows) {
		return false, nil
	}
	return err == nil, err
}
