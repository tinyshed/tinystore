package spike

import (
	"context"
	"database/sql"
	"encoding/base32"
	"encoding/binary"
	"fmt"
	"math/rand/v2"
	"os"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/tinyshed/tinystore/internal/sqlite"
)

// The jobs round of docs/jobs.md measures the mechanics under the agreed API
// before a jobs package exists: where the jobs of one minute lie in the file
// and what draining a million of them costs, where a lease lives, how the file
// divides by object, what an Enqueue costs, what a Work loop and its wake-up
// cost, what a handler writing the application's database waits for, and what
// the raw Claim and Ack calls hold. The measurements need TINYSTORE_SPIKE=1;
// TINYSTORE_JOBS_DIR puts their files on a chosen disk, TINYSTORE_JOBS_SECONDS
// sets each load's length and TINYSTORE_JOBS_COUNT the jobs of a burst.

// jobsLayout is one way to keep jobs: its tables, how a batch of due jobs is
// claimed, how a claimed job is acknowledged, and what the next open runs to
// give back the leases of a process that died
type jobsLayout struct {
	name    string
	schema  []string
	insert  string // ?1 queue, ?2 next, ?3 id, ?4 key, ?5 at, ?6 value
	keyed   string // what a keyed job's insert runs after it, with the insert's arguments, when not empty
	claim   func(ctx context.Context, w sqlite.Writer, now, until int64, batch int) ([]jobsClaimed, error)
	ack     func(ctx context.Context, w sqlite.Writer, job jobsClaimed) error
	next    string // ?1 queue, ?2 now: when the next job not leased falls due
	recover string // ?1 now; empty when a lease ends by itself
}

// jobsClaimed is what a claim hands a worker: the row's place, the attempt
// its lease names, and the value
type jobsClaimed struct {
	next, id, attempt int64
	value             []byte
}

// the queue every measurement fills
const jobsQueue = 1

// the tables of a job ordered by time: the row is where its next moment puts it
var jobsTimeSchema = []string{
	`create table jobs (
		queue   integer not null,
		next    integer not null,
		id      integer not null,
		key     text,
		at      integer not null,
		attempt integer not null default 0,
		lease   integer,
		until   integer,
		value   blob,
		spill   integer,
		primary key (queue, next, id)
	) strict, without rowid`,
	`create unique index jobs_key on jobs (queue, key) where key is not null`,
	`create table leases (id integer primary key, attempt integer not null, until integer not null) strict`,
	`create table spilled (id integer primary key, value blob not null) strict`,
}

const (
	jobsTimeInsert = `insert into jobs (queue, next, id, key, at, value) values (?1, ?2, ?3, ?4, ?5, ?6)`
	jobsArrivalSQL = `insert into jobs (queue, next, id, key, at, value) values (?1, ?2, ?3, ?4, ?5, ?6)`

	// a moved row's lease is its attempt, and its next is the lease's end
	jobsMoveClaim = `update jobs set next = ?3, attempt = attempt + 1, lease = attempt + 1
		where (queue, next, id) in (
			select queue, next, id from jobs where queue = ?1 and next <= ?2
			order by next, id limit cast(?4 as integer))
		returning next, id, attempt, value`
	jobsMoveAck = `delete from jobs where queue = ?1 and next = ?2 and id = ?3 and lease = ?4`

	// a marked row stays where it is due and says until when it is leased
	jobsMarkClaim = `update jobs set attempt = attempt + 1, lease = attempt + 1, until = ?3
		where (queue, next, id) in (
			select queue, next, id from jobs where queue = ?1 and next <= ?2 and (until is null or until <= ?2)
			order by next, id limit cast(?4 as integer))
		returning next, id, attempt, value`
	jobsMarkAck     = `delete from jobs where queue = ?1 and next = ?2 and id = ?3 and lease = ?4`
	jobsMarkRecover = `update jobs set until = null where until is not null`

	// a lease in a table of its own leaves the job's row alone until its end
	jobsTableFront = `select next, id, attempt, value from jobs j
		where queue = ?1 and next <= ?2 and not exists (select 1 from leases l where l.id = j.id and l.until > ?2)
		order by next, id limit cast(?3 as integer)`
	jobsTableLease  = `insert or replace into leases (id, attempt, until) values (?1, ?2, ?3)`
	jobsTableAck    = `delete from jobs where queue = ?1 and next = ?2 and id = ?3`
	jobsTableFree   = `delete from leases where id = ?1 and attempt = ?2`
	jobsTableRecall = `delete from leases`
)

// the tables of a job kept in the order it arrived, with an index by time
var jobsArrivalSchema = []string{
	`create table jobs (
		id      integer primary key,
		queue   integer not null,
		next    integer not null,
		key     text,
		at      integer not null,
		attempt integer not null default 0,
		lease   integer,
		value   blob
	) strict`,
	`create index jobs_next on jobs (queue, next)`,
	`create unique index jobs_key on jobs (queue, key) where key is not null`,
}

const (
	jobsArrivalClaim = `update jobs set next = ?3, attempt = attempt + 1, lease = attempt + 1
		where id in (select id from jobs where queue = ?1 and next <= ?2 order by next limit cast(?4 as integer))
		returning next, id, attempt, value`
	jobsArrivalAck     = `delete from jobs where id = ?3 and lease = ?4`
	jobsArrivalRecover = `update jobs set next = ?1 where lease is not null and next > ?1`
)

// when the next job falls due: a leased row that moved is found at its lease's
// end, and one that stayed is passed over while its lease runs
const (
	jobsNextMoved  = `select next from jobs where queue = ?1 and ?2 = ?2 order by next limit 1`
	jobsNextMarked = `select next from jobs where queue = ?1 and (until is null or until <= ?2)
		order by next, id limit 1`
	jobsNextTabled = `select next from jobs j where queue = ?1
		and not exists (select 1 from leases l where l.id = j.id and l.until > ?2) order by next, id limit 1`
)

// the layouts the round compares; "time" moves a claimed row to its lease's end
var jobsLayouts = map[string]jobsLayout{
	"arrival": {
		name: "arrival", schema: jobsArrivalSchema, insert: jobsArrivalSQL,
		claim: jobsClaimBy(jobsArrivalClaim), ack: jobsAckBy(jobsArrivalAck), next: jobsNextMoved,
		recover: jobsArrivalRecover,
	},
	"time": {
		name: "time", schema: jobsTimeSchema, insert: jobsTimeInsert,
		claim: jobsClaimBy(jobsMoveClaim), ack: jobsAckBy(jobsMoveAck), next: jobsNextMoved,
	},
	"time-mark": {
		name: "time-mark", schema: jobsTimeSchema, insert: jobsTimeInsert,
		claim: jobsClaimBy(jobsMarkClaim), ack: jobsAckBy(jobsMarkAck), next: jobsNextMarked,
		recover: jobsMarkRecover,
	},
	"time-table": {
		name: "time-table", schema: jobsTimeSchema, insert: jobsTimeInsert,
		claim: jobsClaimInTable, ack: jobsAckInTable, next: jobsNextTabled, recover: jobsTableRecall,
	},
}

// jobsClaimBy claims through one statement that leases the rows it returns
func jobsClaimBy(statement string) func(context.Context, sqlite.Writer, int64, int64, int) ([]jobsClaimed, error) {
	return func(ctx context.Context, w sqlite.Writer, now, until int64, batch int) ([]jobsClaimed, error) {
		rows, err := w.QueryContext(ctx, statement, jobsQueue, now, until, batch)
		if err != nil {
			return nil, err
		}
		return jobsScan(rows)
	}
}

func jobsAckBy(statement string) func(context.Context, sqlite.Writer, jobsClaimed) error {
	return func(ctx context.Context, w sqlite.Writer, job jobsClaimed) error {
		_, err := w.ExecContext(ctx, statement, jobsQueue, job.next, job.id, job.attempt)
		return err
	}
}

func jobsClaimInTable(ctx context.Context, w sqlite.Writer, now, until int64, batch int) ([]jobsClaimed, error) {
	rows, err := w.QueryContext(ctx, jobsTableFront, jobsQueue, now, batch)
	if err != nil {
		return nil, err
	}
	claimed, err := jobsScan(rows)
	for i := range claimed {
		claimed[i].attempt++
		if err == nil {
			_, err = w.ExecContext(ctx, jobsTableLease, claimed[i].id, claimed[i].attempt, until)
		}
	}
	return claimed, err
}

func jobsAckInTable(ctx context.Context, w sqlite.Writer, job jobsClaimed) error {
	if _, err := w.ExecContext(ctx, jobsTableAck, jobsQueue, job.next, job.id); err != nil {
		return err
	}
	_, err := w.ExecContext(ctx, jobsTableFree, job.id, job.attempt)
	return err
}

func jobsScan(rows *sql.Rows) ([]jobsClaimed, error) {
	var claimed []jobsClaimed
	err := sqlite.EachRow(rows, "claimed jobs", func(rows *sql.Rows) error {
		var job jobsClaimed
		if err := rows.Scan(&job.next, &job.id, &job.attempt, &job.value); err != nil {
			return err
		}
		claimed = append(claimed, job)
		return nil
	})
	return claimed, err
}

// jobsDir is a directory for one file: under TINYSTORE_JOBS_DIR when it is
// set, so that a container writes to a volume rather than to the bind mount
func jobsDir(t *testing.T) string {
	t.Helper()
	root := os.Getenv("TINYSTORE_JOBS_DIR")
	if root == "" {
		return t.TempDir()
	}
	dir, err := os.MkdirTemp(root, "jobs-")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = os.RemoveAll(dir) })
	return dir
}

func jobsSeconds() time.Duration {
	if seconds, err := strconv.Atoi(os.Getenv("TINYSTORE_JOBS_SECONDS")); err == nil && seconds > 0 {
		return time.Duration(seconds) * time.Second
	}
	return 3 * time.Second
}

// jobsCount is the jobs of a burst, a million unless TINYSTORE_JOBS_COUNT says
func jobsCount() int {
	if count, err := strconv.Atoi(os.Getenv("TINYSTORE_JOBS_COUNT")); err == nil && count > 0 {
		return count
	}
	return 1_000_000
}

// jobsOpen opens a file through internal/sqlite, as an engine would, with 4
// KiB pages, and creates the layout; the test closes it
func jobsOpen(t *testing.T, layout jobsLayout) (*sqlite.File, string) {
	t.Helper()
	ctx := t.Context()
	path := filepath.Join(jobsDir(t), "jobs.db")
	file, err := sqlite.Open(ctx, path, sqlite.Config{Readers: 2, PageSize: 4096})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = file.Close() })
	err = file.Update(ctx, func(tx *sql.Tx) error {
		for _, statement := range layout.schema {
			if _, execErr := tx.ExecContext(ctx, statement); execErr != nil {
				return execErr
			}
		}
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
	return file, path
}

// jobsFixture is a set of jobs to enqueue: their times, and whether they carry
// keys and in what order they arrive
type jobsFixture struct {
	count     int
	from      int64         // unix milliseconds of the earliest time
	span      time.Duration // the times fall within it
	inOrder   bool          // enqueued in the order of their times, or shuffled
	keyed     bool
	valueSize int // zero: a scheduled message of about 190 bytes
	spilled   bool
}

// jobsFill enqueues a fixture, 10,000 a transaction, ids in arrival order
func jobsFill(t *testing.T, file *sqlite.File, layout jobsLayout, f jobsFixture) time.Duration {
	t.Helper()
	ctx := t.Context()
	times := jobsTimes(f)
	began := time.Now()
	for start := 0; start < f.count; start += 10_000 {
		end := min(start+10_000, f.count)
		err := file.UpdatePrepared(ctx, func(w sqlite.Writer) error {
			for i := start; i < end; i++ {
				if err := jobsInsert(ctx, w, layout, f, i, times[i]); err != nil {
					return err
				}
			}
			return nil
		})
		if err != nil {
			t.Fatal(err)
		}
	}
	return time.Since(began)
}

const (
	jobsSpill       = `insert into spilled (id, value) values (?1, ?2)`
	jobsSpillInsert = `insert into jobs (queue, next, id, key, at, spill) values (?1, ?2, ?3, ?4, ?5, ?3)`
)

func jobsInsert(ctx context.Context, w sqlite.Writer, layout jobsLayout, f jobsFixture, i int, at int64) error {
	var key any
	if f.keyed {
		key = jobsKey(i)
	}
	value := jobsValue(i, f.valueSize)
	if !f.spilled {
		_, err := w.ExecContext(ctx, layout.insert, jobsQueue, at, i+1, key, at, value)
		if err == nil && key != nil && layout.keyed != "" {
			_, err = w.ExecContext(ctx, layout.keyed, jobsQueue, at, i+1, key, at, value)
		}
		return err
	}
	if _, err := w.ExecContext(ctx, jobsSpill, i+1, value); err != nil {
		return err
	}
	_, err := w.ExecContext(ctx, jobsSpillInsert, jobsQueue, at, i+1, key, at)
	return err
}

// jobsTimes are the fixture's times in the order its jobs arrive
func jobsTimes(f jobsFixture) []int64 {
	random := rand.New(rand.NewPCG(uint64(f.count), 11))
	times := make([]int64, f.count)
	for i := range times {
		times[i] = f.from + random.Int64N(max(f.span.Milliseconds(), 1))
	}
	if f.inOrder {
		slices.Sort(times)
	}
	return times
}

var jobsWords = strings.Fields("see you tomorrow at the office do not forget the tickets happy birthday " +
	"call me when you land meeting moved to three the report is ready please review it before friday " +
	"dinner at eight bring the kids we are late again love you congrats on the new job")

// jobsValue is job i's value: a scheduled message as JSON, about 190 bytes, or
// size bytes of it when size is set
func jobsValue(i, size int) []byte {
	random := rand.New(rand.NewPCG(uint64(i), 17))
	var text strings.Builder
	for text.Len() < 150 || text.Len() < size {
		if text.Len() > 0 {
			text.WriteByte(' ')
		}
		text.WriteString(jobsWords[random.IntN(len(jobsWords))])
	}
	value := fmt.Appendf(nil, `{"ID":%q,"Chat":%d,"Author":%d,"Text":%q}`, jobsToken(uint64(i)),
		random.Int64N(100_000), random.Int64N(1_000_000), text.String())
	if size > 0 {
		value = value[:size]
	}
	return value
}

var jobsBase32 = base32.StdEncoding.WithPadding(base32.NoPadding)

// jobsToken is an id as a client spells one: 26 characters of base32
func jobsToken(seed uint64) string {
	random := rand.New(rand.NewPCG(seed, 23))
	var token [16]byte
	binary.LittleEndian.PutUint64(token[:8], random.Uint64())
	binary.LittleEndian.PutUint64(token[8:], random.Uint64())
	return jobsBase32.EncodeToString(token[:])
}

// jobsKey is a scheduled message's key: its chat, then its id
func jobsKey(i int) string {
	random := rand.New(rand.NewPCG(uint64(i), 29))
	return fmt.Sprintf("chat:%d:%s", random.Int64N(100_000), jobsToken(uint64(i)))
}

func jobsCommits(t *testing.T, file *sqlite.File) uint64 {
	t.Helper()
	counters, err := file.WriterCounters(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	return counters.Commits
}

// jobsMillis is a duration in milliseconds with a tenth
func jobsMillis(d time.Duration) string {
	return fmt.Sprintf("%8.1f ms", float64(d)/float64(time.Millisecond))
}
