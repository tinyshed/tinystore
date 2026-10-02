package jobs

import (
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"fmt"
	"iter"
	"strconv"
	"strings"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/internal/sqlite"
)

// State is where a job is in its life.
type State int

const (
	Waiting   State = iota + 1 // until its time, or due and no handler has it yet
	Running                    // a handler, or the caller of Claim, has it
	Failed                     // failed for good, kept KeepFailed
	Done                       // acknowledged; Get finds it while KeepDone keeps its key
	Cancelled                  // taken by Cancel; only a watcher sees it
)

func (s State) String() string {
	switch s {
	case Waiting:
		return "waiting"
	case Running:
		return "running"
	case Failed:
		return "failed"
	case Done:
		return "done"
	case Cancelled:
		return "cancelled"
	}
	return "any"
}

// ended says the job has left its queue, or failed for good
func (s State) ended() bool {
	return s == Failed || s == Done || s == Cancelled
}

// Entry is a job as the queue holds it.
type Entry[V any] struct {
	Key   string
	Value V
	// At is the time the job runs for.
	At time.Time
	// Attempt counts the job's attempts, a running one included.
	Attempt int
	State   State
	// Ahead counts the jobs that run before a waiting one, up to 10,000, so
	// that 10,000 reads as that many or more. Get and Watch count it; Scan
	// leaves it zero.
	Ahead int
	// Progress is what the handler of a running job last reported of it, as
	// JSON, until the attempt is settled.
	Progress json.RawMessage
	// Err is the job's last failure.
	Err string
	// Ran is when the last run a handler finished began, and Took how long it
	// took: acknowledged, retried, failed or snoozed. Both are zero before a
	// handler finished one, and a run given back records none.
	Ran  time.Time
	Took time.Duration
	// Repeat is a repeating job's repeat as jobs.db keeps it: cron text and
	// zone, or an interval.
	Repeat string
}

// Query asks Scan for a page of the queue's jobs.
//
// With a Prefix it lists the keys under it in the byte order of their text.
// With State Failed and no Prefix it lists the failed jobs instead, the last
// failed first, keyed or not.
type Query struct {
	Prefix string
	// State narrows either listing to Waiting, Running or Failed jobs.
	State State
	// After is where the page before ended. Page.Next carries it.
	After string
	Limit int // jobs a page returns: 100 when zero, at most 1000
}

// Page is one page of a Scan, from one snapshot.
type Page[V any] struct {
	Entries []Entry[V]
	// More says the limit, or the page's 4 MiB of values, ended the page before
	// the jobs did.
	More bool
	// Next is the query that reads on.
	Next Query
}

// JobError is a call refused because of one job: its queue and key, empty for
// a job without one. errors.Is finds the store's sentinel in it.
type JobError struct {
	Queue, Key string
	Err        error
}

func (e *JobError) Error() string {
	if e.Key == "" {
		return fmt.Sprintf("jobs: queue %q: %v", e.Queue, e.Err)
	}
	return fmt.Sprintf("jobs: queue %q, key %q: %v", e.Queue, e.Key, e.Err)
}

func (e *JobError) Unwrap() error { return e.Err }

// ErrOutcomeUnknown is returned for a write whose group's commit failed. The
// write may or may not be in the file, so its caller reads it back before
// writing again.
var ErrOutcomeUnknown = sqlite.ErrOutcomeUnknown

type found struct {
	key      sql.NullString
	time, id int64 // next or, for a failed job, when it failed
	at       int64
	attempt  int64
	leased   sql.NullInt64 // the running attempt, when a lease holds the job
	failed   bool
	repeat   sql.NullString
	failure  sql.NullString
	value    []byte
	spill    sql.NullInt64
	size     int // the value's bytes, in the row or spilled
	ran      sql.NullInt64
	took     sql.NullInt64
}

// A job is found through its key, whose row names where the job lies. A key a
// job left behind names nothing.
//
// A value's size is read without its bytes, since SQLite's length() of a blob
// column takes it from the row's header and leaves its overflow pages alone.
const (
	keyedColumns = `j.key, j.next, j.id, j.at, j.attempt, l.attempt, 0, j.repeat, j.error, j.value, j.spill,
			coalesce(length(j.value), (select length(s.value) from _tinystore_jobs_spilled s where s.id = j.spill), 0),
			j.ran, j.took
		from _tinystore_jobs_keys k join _tinystore_jobs j on j.queue = k.queue and j.next = k.next and j.id = k.id
		left join _tinystore_jobs_leases l on l.id = j.id and l.until > ?9`
	failedColumns = `f.key, f.failed, f.id, f.at, f.attempt, null, 1, null, f.error, f.value, f.spill,
			coalesce(length(f.value), (select length(s.value) from _tinystore_jobs_spilled s where s.id = f.spill), 0),
			f.ran, f.took
		from _tinystore_jobs_failed f`
	getWaiting   = `select ` + keyedColumns + ` where k.queue = ?1 and k.key = ?2`
	getFailed    = `select ` + failedColumns + ` where f.queue = ?1 and f.key = ?2`
	valueSpilled = `select value from _tinystore_jobs_spilled where id = ?1`
	// the rows before a job in the order the queue runs them, at most ?4
	rowsBefore = `select count(*) from (select 1 from _tinystore_jobs
		where queue = ?1 and (next, id) < (?2, ?3) limit cast(?4 as integer))`
)

// Get reads the job under key from one snapshot: waiting, running or failed,
// or done while KeepDone keeps its key.
func (q *Queue[V]) Get(ctx context.Context, key string) (Entry[V], bool, error) {
	s, err := q.sight(ctx, key)
	return s.entry, s.found, q.fail(key, err)
}

// sighting is what one read of a key found: its job's entry and id, and for a
// running job when its lease ends, at which a watcher reads again
type sighting[V any] struct {
	entry Entry[V]
	found bool
	id    int64
	until int64
}

// sight reads the job under key from one snapshot, counting the jobs ahead of
// a waiting one
func (q *Queue[V]) sight(ctx context.Context, key string) (sighting[V], error) {
	var s sighting[V]
	err := q.read(ctx, maxValue, func(r sqlite.Reader) error {
		// each statement is given the parameters it has, up to the highest it names
		for _, lookup := range []struct {
			query string
			args  []any
		}{
			{getWaiting, []any{q.state.id, key, nil, nil, nil, nil, nil, nil, q.store.clock()}},
			{getFailed, []any{q.state.id, key}},
		} {
			rows, err := r.QueryContext(ctx, lookup.query, lookup.args...)
			if err != nil {
				return err
			}
			var jobs []found
			if jobs, err = scanFound(rows); err != nil || len(jobs) == 0 {
				if err != nil {
					return err
				}
				continue
			}
			s.found, s.id = true, jobs[0].id
			if s.entry, s.until, err = q.entryOf(ctx, r, jobs[0]); err == nil && s.entry.State == Waiting {
				s.entry.Ahead, err = q.ahead(ctx, r, jobs[0])
			}
			return err
		}
		return q.sightDone(ctx, r, key, &s)
	})
	return s, err
}

// sightDone finds a key KeepDone keeps, which names a job that was done
func (q *Queue[V]) sightDone(ctx context.Context, r sqlite.Reader, key string, s *sighting[V]) error {
	if q.state.policy.keepDone == 0 {
		return nil
	}
	var one int
	err := sqlite.QueryRowByKey(ctx, r, doneKey, q.state.id, key, q.store.clock()).Scan(&one)
	if errors.Is(err, sql.ErrNoRows) {
		return nil
	}
	s.entry, s.found = Entry[V]{Key: key, State: Done}, err == nil
	return err
}

// ahead counts the jobs that run before a waiting one: the rows before it, at
// most maxAhead past those a handler runs, which wait before nothing
func (q *Queue[V]) ahead(ctx context.Context, r sqlite.Reader, job found) (int, error) {
	if job.leased.Valid {
		return 0, nil // held for a busy worker, which runs it next
	}
	running := q.state.watch.runningBefore(job.time, job.id, q.store.clock())
	var rows int
	err := sqlite.QueryRow(ctx, r, rowsBefore, q.state.id, job.time, job.id, maxAhead+running).Scan(&rows)
	return min(max(rows-running, 0), maxAhead), err
}

// the pages of a Scan: keys under a prefix, both tables merged in key order,
// or the failed jobs by when they failed, the last first
const (
	scanKeys = `select * from (
			select ` + keyedColumns + ` where k.queue = ?1 and k.key > ?2 and k.key >= ?3 and (?4 is null or k.key < ?4)
				and (?5 = 0 or ?5 = 1 or (?5 = 2 and l.attempt is not null))
			union all
			select ` + failedColumns + ` where f.queue = ?1 and f.key > ?2 and f.key >= ?3
				and (?4 is null or f.key < ?4)
				and (?5 = 0 or ?5 = 3)
		) order by 1 limit cast(?6 as integer)`
	scanFailed = `select ` + failedColumns + ` where f.queue = ?1 and (f.failed, f.id) < (?7, ?8)
		order by f.failed desc, f.id desc limit cast(?6 as integer)`
)

// what a Scan holds at most: its page's values and the rows it read past them
const scanHeld = scanBytes + (maxScanLimit+1)*inlineValue

// Scan reads a page of the queue's jobs from one snapshot; see Query.
func (q *Queue[V]) Scan(ctx context.Context, query Query) (Page[V], error) {
	limit, err := checkQuery(query)
	if err != nil {
		return Page[V]{}, q.fail("", err)
	}
	var page Page[V]
	err = q.read(ctx, scanHeld, func(r sqlite.Reader) error {
		jobs, scanErr := q.scanRows(ctx, r, query, limit+1)
		if scanErr != nil {
			return scanErr
		}
		var pageErr error
		page, pageErr = q.pageOf(ctx, r, query, jobs, limit)
		return pageErr
	})
	return page, q.fail("", err)
}

// All walks what query names a page at a time, holding no snapshot between
// pages, so a slow loop keeps no reader open. A job written during the walk may
// or may not be met.
func (q *Queue[V]) All(ctx context.Context, query Query) iter.Seq2[Entry[V], error] {
	return func(yield func(Entry[V], error) bool) {
		for {
			page, err := q.Scan(ctx, query)
			if err != nil {
				yield(Entry[V]{}, err)
				return
			}
			for _, entry := range page.Entries {
				if !yield(entry, nil) {
					return
				}
			}
			if !page.More {
				return
			}
			query = page.Next
		}
	}
}

func checkQuery(query Query) (int, error) {
	limit := query.Limit
	switch {
	case limit == 0:
		limit = scanLimit
	case limit < 0 || limit > maxScanLimit:
		return 0, fmt.Errorf("%w: jobs: a page of %d jobs, not 1 to 1000", tinystore.ErrInvalid, limit)
	}
	if query.State < 0 || query.State > Failed {
		return 0, fmt.Errorf("%w: jobs: a state of %d", tinystore.ErrInvalid, query.State)
	}
	return limit, nil
}

func (q *Queue[V]) scanRows(ctx context.Context, r sqlite.Reader, query Query, limit int) ([]found, error) {
	now := q.store.clock()
	if query.Prefix == "" && query.State == Failed {
		before, beforeID, err := failedCursor(query.After)
		if err != nil {
			return nil, err
		}
		rows, err := r.QueryContext(ctx, scanFailed, q.state.id, nil, nil, nil, nil, limit, before, beforeID)
		if err != nil {
			return nil, err
		}
		return scanFound(rows)
	}
	upper, bounded := prefixEnd(query.Prefix)
	var end any = upper
	if !bounded {
		end = nil
	}
	rows, err := r.QueryContext(ctx, scanKeys, q.state.id, query.After, query.Prefix, end, int(query.State), limit,
		nil, nil, now)
	if err != nil {
		return nil, err
	}
	return scanFound(rows)
}

// pageOf makes the entries of a page, ending it before the value that would
// take it past 4 MiB, a spilled value's bytes counted as a row's are. The first
// entry is taken whatever its size, so that every page moves on.
//
// Whether a held job runs is memory's to say, so a listing of waiting or
// running jobs leaves out the rows it read that turn out the other, and its
// page may hold fewer than its limit.
func (q *Queue[V]) pageOf(ctx context.Context, r sqlite.Reader, query Query, jobs []found, limit int) (Page[V], error) {
	page := Page[V]{Next: query}
	bytes := 0
	for i, job := range jobs {
		if i == limit || i > 0 && bytes+job.size > scanBytes {
			page.More = true
			break
		}
		page.Next.After = cursorOf(query, job)
		if (query.State == Waiting || query.State == Running) && q.stateOf(job) != query.State {
			continue
		}
		entry, _, err := q.entryOf(ctx, r, job)
		if err != nil {
			return page, err
		}
		page.Entries = append(page.Entries, entry)
		bytes += job.size
	}
	return page, nil
}

// cursorOf is where the next page starts after job: its key, or for failed
// jobs listed by time, when it failed and its id
func cursorOf(query Query, job found) string {
	if query.Prefix == "" && query.State == Failed {
		return strconv.FormatInt(job.time, 10) + "." + strconv.FormatInt(job.id, 10)
	}
	return job.key.String
}

// failedCursor reads a failed page's cursor; none starts at the last failed
func failedCursor(after string) (int64, int64, error) {
	if after == "" {
		return 1<<63 - 1, 1<<63 - 1, nil
	}
	failedText, idText, found := strings.Cut(after, ".")
	failed, failedErr := strconv.ParseInt(failedText, 10, 64)
	id, idErr := strconv.ParseInt(idText, 10, 64)
	if !found || failedErr != nil || idErr != nil {
		return 0, 0, fmt.Errorf("%w: jobs: a cursor of %q", tinystore.ErrInvalid, after)
	}
	return failed, id, nil
}

// prefixEnd is the first text after every text starting with prefix, in the
// byte order SQLite compares keys by, or none when the prefix is empty or
// only bytes that cannot grow; the bound need not be UTF-8
//
//	"aé"    61 c3 a9  → "aê"  61 c3 aa
//	"a\xff" 61 ff     → "b"   62
func prefixEnd(prefix string) (string, bool) {
	end := []byte(prefix)
	for i := len(end) - 1; i >= 0; i-- {
		if end[i] < 0xff {
			end[i]++
			return string(end[:i+1]), true
		}
	}
	return "", false
}

func scanFound(rows *sql.Rows) ([]found, error) {
	var jobs []found
	err := sqlite.EachRow(rows, "jobs", func(rows *sql.Rows) error {
		var job found
		var failed int
		err := rows.Scan(&job.key, &job.time, &job.id, &job.at, &job.attempt, &job.leased, &failed, &job.repeat,
			&job.failure, &job.value, &job.spill, &job.size, &job.ran, &job.took)
		job.failed = failed == 1
		jobs = append(jobs, job)
		return err
	})
	return jobs, err
}

// stateOf is where a job is: a row a lease holds runs once its handler, or a
// Claim's caller, has it, and until then waits in a Work loop for a busy worker
func (q *Queue[V]) stateOf(job found) State {
	switch {
	case job.failed:
		return Failed
	case job.leased.Valid:
		if runs, _, _ := q.state.watch.view(job.id, job.leased.Int64); runs {
			return Running
		}
	}
	return Waiting
}

// entryOf makes a row an entry, and says when the lease of a running job ends
func (q *Queue[V]) entryOf(ctx context.Context, r sqlite.Reader, job found) (Entry[V], int64, error) {
	entry := Entry[V]{
		Key: job.key.String, At: time.UnixMilli(job.at), Attempt: int(job.attempt), State: q.stateOf(job),
		Err: job.failure.String, Repeat: job.repeat.String, Took: time.Duration(job.took.Int64) * time.Millisecond,
	}
	if job.ran.Valid {
		entry.Ran = time.UnixMilli(job.ran.Int64)
	}
	var until int64
	if entry.State == Running {
		_, entry.Progress, until = q.state.watch.view(job.id, job.leased.Int64)
		entry.Attempt = int(job.leased.Int64)
	}
	encoded := job.value
	if job.spill.Valid {
		if err := sqlite.QueryRowByKey(ctx, r, valueSpilled, job.spill.Int64).Scan(&encoded); err != nil {
			return entry, until, fmt.Errorf("%w: jobs: the value spilled by a job is gone: %w", tinystore.ErrCorrupt,
				err)
		}
	}
	value, err := q.codec.decode(encoded)
	entry.Value = value
	if err != nil {
		return entry, until, fmt.Errorf("%w: jobs: %w", tinystore.ErrCorrupt, err)
	}
	return entry, until, nil
}

// read runs work on one snapshot of jobs.db: inside the handle's transaction,
// which sees its own writes, or on a reader once the store's memory holds the
// bytes it may read. Inside a transaction it waits for no memory, since it
// holds the writer that the writes holding memory wait for: it takes what is
// free, and past that it is ErrLimit.
func (q *Queue[V]) read(ctx context.Context, bytes int, work func(sqlite.Reader) error) error {
	if q.tx != nil {
		if err := q.checkTx(); err != nil {
			return err
		}
		reserved, err := q.store.reserveNow(bytes)
		if err != nil {
			return err
		}
		defer reserved.Release()
		return q.tx.run(func(w sqlite.Writer) error { return work(w) })
	}
	leave, err := q.store.admit(ctx)
	if err != nil {
		return err
	}
	defer leave()
	reserved, err := q.store.reserve(ctx, bytes)
	if err != nil {
		return err
	}
	defer reserved.Release()
	return q.store.file.ViewPrepared(ctx, work)
}
