package jobs

import (
	"context"
	"database/sql"
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
	Waiting State = iota + 1 // until its time, or due and not yet claimed
	Leased                   // claimed, its lease running
	Failed                   // failed for good, kept KeepFailed
)

func (s State) String() string {
	switch s {
	case Waiting:
		return "waiting"
	case Leased:
		return "leased"
	case Failed:
		return "failed"
	}
	return "any"
}

// Entry is a job as the queue holds it. At is the time it runs for; Attempt
// counts its attempts, a running one included; Err is its last failure, and
// Repeat a repeating job's cron text and zone.
type Entry[V any] struct {
	Key     string
	Value   V
	At      time.Time
	Attempt int
	State   State
	Err     string
	Repeat  string
}

// Query asks Scan for a page: the keys under Prefix in the byte order of
// their text, or, with State Failed and no prefix, the failed jobs, the last
// failed first, keyed or not. State narrows either; After is where the page
// before ended, and Page.Next carries it.
type Query struct {
	Prefix string
	State  State
	After  string
	Limit  int // jobs a page returns: 100 when zero, at most 1000
}

// Page is one page of a Scan, from one snapshot. More says the limit, or the
// page's 4 MiB of values, ended it before the jobs did, and Next reads on.
type Page[V any] struct {
	Entries []Entry[V]
	More    bool
	Next    Query
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

// ErrOutcomeUnknown is a write whose group's commit failed: it may or may not
// be in the file, and its caller reads it back before writing again.
var ErrOutcomeUnknown = sqlite.ErrOutcomeUnknown

// found is a row a read found, of a waiting or leased job or a failed one
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
}

// a value's size is read without its bytes: SQLite's length() of a blob
// column takes it from the row's header and leaves its overflow pages alone
const (
	waitingColumns = `j.key, j.next, j.id, j.at, j.attempt, l.attempt, 0, j.repeat, j.error, j.value, j.spill,
			coalesce(length(j.value), (select length(s.value) from spilled s where s.id = j.spill), 0)
		from jobs j left join leases l on l.id = j.id and l.until > ?9`
	failedColumns = `f.key, f.failed, f.id, f.at, f.attempt, null, 1, null, f.error, f.value, f.spill,
			coalesce(length(f.value), (select length(s.value) from spilled s where s.id = f.spill), 0)
		from failed f`
	getWaiting   = `select ` + waitingColumns + ` where j.queue = ?1 and j.key = ?2`
	getFailed    = `select ` + failedColumns + ` where f.queue = ?1 and f.key = ?2`
	valueSpilled = `select value from spilled where id = ?1`
)

// Get reads the job under key from one snapshot: waiting, leased or failed.
func (q *Queue[V]) Get(ctx context.Context, key string) (Entry[V], bool, error) {
	var entry Entry[V]
	var there bool
	err := q.read(ctx, maxValue, func(r sqlite.Reader) error {
		for _, query := range []string{getWaiting, getFailed} {
			rows, err := r.QueryContext(ctx, query, q.state.id, key, nil, nil, nil, nil, nil, nil, q.store.clock())
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
			there = true
			entry, err = q.entryOf(ctx, r, jobs[0])
			return err
		}
		return nil
	})
	return entry, there, q.fail(key, err)
}

// the pages of a Scan: keys under a prefix, both tables merged in key order,
// or the failed jobs by when they failed, the last first
const (
	scanKeys = `select * from (
			select ` + waitingColumns + ` where j.queue = ?1 and j.key > ?2 and j.key >= ?3 and j.key < ?4
				and (?5 = 0 or (?5 = 1 and l.attempt is null) or (?5 = 2 and l.attempt is not null))
			union all
			select ` + failedColumns + ` where f.queue = ?1 and f.key > ?2 and f.key >= ?3 and f.key < ?4
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
// pages, so a slow loop keeps no reader open; a job written during the walk
// may or may not be met.
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
	case query.State < 0 || query.State > Failed:
		return 0, fmt.Errorf("%w: jobs: a state of %d", tinystore.ErrInvalid, query.State)
	}
	return limit, nil
}

// scanRows reads up to limit rows of the page query names
func (q *Queue[V]) scanRows(ctx context.Context, r sqlite.Reader, query Query, limit int) ([]found, error) {
	now := q.store.clock()
	if query.Prefix == "" && query.State == Failed {
		before, beforeID, err := failedCursor(query.After)
		if err != nil {
			return nil, err
		}
		rows, err := r.QueryContext(ctx, scanFailed, q.state.id, nil, nil, nil, nil, limit, before, beforeID, now)
		if err != nil {
			return nil, err
		}
		return scanFound(rows)
	}
	upper, bounded := prefixEnd(query.Prefix)
	if !bounded {
		upper = "\U0010FFFF\U0010FFFF\U0010FFFF\U0010FFFF"
	}
	rows, err := r.QueryContext(ctx, scanKeys, q.state.id, query.After, query.Prefix, upper, int(query.State), limit,
		nil, nil, now)
	if err != nil {
		return nil, err
	}
	return scanFound(rows)
}

// pageOf makes the entries of a page, ending it before the value that would
// take it past 4 MiB, a spilled value's bytes counted as a row's are; the
// first entry is taken whatever its size, so that every page moves on
func (q *Queue[V]) pageOf(ctx context.Context, r sqlite.Reader, query Query, jobs []found, limit int) (Page[V], error) {
	page := Page[V]{Next: query}
	bytes := 0
	for i, job := range jobs {
		if i == limit || i > 0 && bytes+job.size > scanBytes {
			page.More = true
			break
		}
		entry, err := q.entryOf(ctx, r, job)
		if err != nil {
			return page, err
		}
		page.Entries = append(page.Entries, entry)
		bytes += job.size
		page.Next.After = cursorOf(query, job)
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

// prefixEnd is the first text after every text starting with prefix, or none
// when the prefix is empty or only bytes that cannot grow
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
			&job.failure, &job.value, &job.spill, &job.size)
		job.failed = failed == 1
		jobs = append(jobs, job)
		return err
	})
	return jobs, err
}

// entryOf decodes a row a read found, with its spilled value
func (q *Queue[V]) entryOf(ctx context.Context, r sqlite.Reader, job found) (Entry[V], error) {
	entry := Entry[V]{
		Key: job.key.String, At: time.UnixMilli(job.at), Attempt: int(job.attempt), State: Waiting,
		Err: job.failure.String, Repeat: job.repeat.String,
	}
	switch {
	case job.failed:
		entry.State = Failed
	case job.leased.Valid:
		entry.State, entry.Attempt = Leased, int(job.leased.Int64)
	}
	encoded := job.value
	if job.spill.Valid {
		if err := sqlite.QueryRow(ctx, r, valueSpilled, job.spill.Int64).Scan(&encoded); err != nil {
			return entry, fmt.Errorf("%w: jobs: the value spilled by a job is gone: %w", tinystore.ErrCorrupt, err)
		}
	}
	value, err := q.codec.decode(encoded)
	entry.Value = value
	if err != nil {
		return entry, fmt.Errorf("%w: jobs: %w", tinystore.ErrCorrupt, err)
	}
	return entry, nil
}

// read runs work on one snapshot of jobs.db: inside the handle's transaction,
// which sees its own writes, or on a reader once the store's memory holds the
// bytes it may read. A read inside a transaction waits for no memory, since it
// holds the writer that the writes holding memory wait for; one transaction
// runs at a time, so it holds at most one read's bytes beyond the budget
func (q *Queue[V]) read(ctx context.Context, bytes int, work func(sqlite.Reader) error) error {
	if q.tx != nil {
		return q.tx.run(func(w sqlite.Writer) error { return work(w) })
	}
	leave, err := q.store.admit(ctx)
	if err != nil {
		return err
	}
	defer leave()
	unreserve, err := q.store.reserve(ctx, bytes)
	if err != nil {
		return err
	}
	defer unreserve()
	return q.store.file.ViewPrepared(ctx, work)
}
