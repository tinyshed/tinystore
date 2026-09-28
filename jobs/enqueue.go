package jobs

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"sync/atomic"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/internal/sqlite"
)

var (
	errNoLongerWaits = fmt.Errorf("%w: jobs: the job no longer waits: it runs, ran or was cancelled",
		tinystore.ErrConflict)
	errTxClosed = fmt.Errorf("jobs: a handle used after its transaction: %w", tinystore.ErrClosed)
)

// Enqueue adds a job that runs at jobs.At or jobs.After, or now. Under a key
// whose job waits it adds nothing, and can bring that job forward but never
// back; under one whose job runs it asks for one run more after it; under a
// failed one it starts the job again. A repeat needs a key. The call returns
// once the job is in the file.
func (q *Queue[V]) Enqueue(ctx context.Context, value V, options ...EnqueueOption) error {
	e, err := q.prepare(options)
	if err == nil {
		e.id, err = q.newID(ctx)
	}
	if err != nil {
		return q.fail(e.key.String, err)
	}

	added := false
	err = q.writeValue(ctx, value, &e, func(w sqlite.Writer) (writeErr error) {
		added, writeErr = enqueue(ctx, w, e)
		return writeErr
	})
	if err != nil {
		q.reportLimit(err)
		return q.fail(e.key.String, err)
	}

	q.committed(func() {
		if added {
			q.state.waiting.Add(1)
		}
		q.state.alarm.lower(e.at)
	})
	return nil
}

// Update gives a job that still waits, or failed, a new value, and a new time
// or repeat when options name one; a failed job waits again, its attempts from
// zero, now unless they name a time. A job that runs, ran or was cancelled, or
// a key that names none, is tinystore.ErrConflict.
func (q *Queue[V]) Update(ctx context.Context, key string, value V, options ...EnqueueOption) error {
	e, err := q.prepare(append(options, Key(key)))
	if err == nil {
		e.id, err = q.newID(ctx)
	}
	if err != nil {
		return q.fail(key, err)
	}

	next := int64(0)
	requeued := false
	err = q.writeValue(ctx, value, &e, func(w sqlite.Writer) (writeErr error) {
		next, requeued, writeErr = update(ctx, w, e)
		return writeErr
	})
	if err != nil {
		q.reportLimit(err)
		return q.fail(key, err)
	}

	q.committed(func() {
		if requeued {
			q.state.waiting.Add(1)
		}
		q.state.alarm.lower(next)
	})
	return nil
}

// Cancel removes a job that waits or failed and says whether it did; one that
// runs is left to its worker, and false says it is too late.
func (q *Queue[V]) Cancel(ctx context.Context, key string) (bool, error) {
	if key == "" || len(key) > maxKey {
		return false, q.fail(key, fmt.Errorf("%w: jobs: a key of %d bytes, not 1 to 1024", tinystore.ErrInvalid,
			len(key)))
	}
	now := q.store.clock()
	var waited, failed bool
	err := q.write(ctx, 0, func(w sqlite.Writer) (writeErr error) {
		waited, failed, writeErr = cancel(ctx, w, q.state.id, key, now)
		return writeErr
	})
	if err != nil {
		return false, q.fail(key, err)
	}
	if waited {
		q.committed(func() { q.state.waiting.Add(-1) })
	}
	return waited || failed, nil
}

// enqueued is an Enqueue's or an Update's facts, gathered before it waits for
// the writer, and its value once the write holds room for it
type enqueued struct {
	queue, id   int64
	maxWaiting  int64
	at, now     int64
	timed       bool
	key, repeat sql.NullString
	value       kept
	keepDone    bool
}

func (q *Queue[V]) prepare(options []EnqueueOption) (enqueued, error) {
	s, err := collectEnqueue(options)
	e := enqueued{
		queue: q.state.id, maxWaiting: q.state.policy.maxWaiting, now: q.store.clock(),
		timed: s.timed, keepDone: q.state.policy.keepDone > 0,
		key: sql.NullString{String: s.key, Valid: s.keyed},
	}
	switch {
	case err != nil:
		return e, err
	case s.repeat != nil && !s.keyed:
		return e, fmt.Errorf("%w: jobs: a repeat without a key, which alone could stop it", tinystore.ErrInvalid)
	}
	e.at = when(s.at, s.after, s.timed, e.now, e.now)
	if s.repeat != nil {
		e.repeat = sql.NullString{String: s.repeat.text, Valid: true}
		if !s.timed {
			e.at = s.repeat.next(time.UnixMilli(e.now)).UnixMilli()
		}
	}
	return e, nil
}

type waitingLimit struct{ limit int64 }

func (e waitingLimit) Error() string {
	return fmt.Sprintf("jobs: queue holds %d jobs, its MaxWaiting", e.limit)
}

func (waitingLimit) Unwrap() error { return tinystore.ErrLimit }

// checkRoom refuses a job past the queue's MaxWaiting; the count is the
// queue's row, which triggers keep, so that a Tx sees its own enqueues
func checkRoom(ctx context.Context, w sqlite.Writer, e enqueued) error {
	var waiting int64
	if err := sqlite.QueryRow(ctx, w, countWaiting, e.queue).Scan(&waiting); err != nil {
		return err
	}
	if waiting >= e.maxWaiting {
		return waitingLimit{limit: e.maxWaiting}
	}
	return nil
}

func (q *Queue[V]) reportLimit(err error) {
	var full waitingLimit
	if errors.As(err, &full) {
		q.state.limits.observe(q.store.now(), fmt.Sprintf("%d jobs", full.limit))
	}
}

type row struct {
	next, id, at, attempt int64
	again                 sql.NullInt64
	repeat, failure       sql.NullString
	spill                 sql.NullInt64
}

const (
	jobByKey = `select j.next, j.id, j.at, j.attempt, j.again, j.repeat, j.error, j.spill
		from keys k join jobs j on j.queue = k.queue and j.next = k.next and j.id = k.id
		where k.queue = ?1 and k.key = ?2`
	leaseOf = `select 1 from leases where id = ?1 and until > ?2`
	doneKey = `select 1 from done where queue = ?1 and key = ?2 and until > ?3`

	insertJob = `insert into jobs (queue, next, id, key, at, attempt, repeat, value, spill)
		values (?1, ?2, ?3, ?4, ?2, 0, ?5, ?6, ?7)`
	insertSpilled = `insert into spilled (id, value) values (?1, ?2)`
	deleteSpilled = `delete from spilled where id = ?1`
	bringForward  = `update jobs set next = ?4, at = ?4 where queue = ?1 and next = ?2 and id = ?3`
	askAgain      = `update jobs set again = min(coalesce(again, ?4), ?4) where queue = ?1 and next = ?2 and id = ?3`
	dropFailed    = `delete from failed where queue = ?1 and key = ?2 returning spill`
	updateWaiting = `update jobs set next = ?4, at = ?4, value = ?5, spill = ?6, repeat = coalesce(?7, repeat)
		where queue = ?1 and next = ?2 and id = ?3`
	cancelWaiting = `delete from jobs where (queue, next, id) in (
			select k.queue, k.next, k.id from keys k where k.queue = ?1 and k.key = ?2)
		and not exists (select 1 from leases l where l.id = jobs.id and l.until > ?3)
		returning id, spill`
	dropLeaseOf = `delete from leases where id = ?1`
)

// a key's row: where its job lies, replacing a key a job left behind, then
// following the job as it moves; a job gone leaves it for maintenance
const (
	insertKey = `insert into keys (queue, key, next, id) values (?1, ?2, ?3, ?4)
		on conflict (queue, key) do update set next = excluded.next, id = excluded.id`
	moveKey = `update keys set next = ?3 where queue = ?1 and key = ?2`
	dropKey = `delete from keys where queue = ?1 and key = ?2`
)

// keepKey names where a keyed job's row lies
func keepKey(ctx context.Context, w sqlite.Writer, queue int64, key sql.NullString, next, id int64) error {
	if !key.Valid {
		return nil
	}
	_, err := w.ExecContext(ctx, insertKey, queue, key.String, next, id)
	return err
}

// moveKeyTo follows a keyed job's row to its next time
func moveKeyTo(ctx context.Context, w sqlite.Writer, queue int64, key string, next int64) error {
	if key == "" {
		return nil
	}
	_, err := w.ExecContext(ctx, moveKey, queue, key, next)
	return err
}

func enqueue(ctx context.Context, w sqlite.Writer, e enqueued) (added bool, err error) {
	if !e.key.Valid {
		if roomErr := checkRoom(ctx, w, e); roomErr != nil {
			return false, roomErr
		}
		return true, insertRow(ctx, w, e)
	}
	there, found, err := findByKey(ctx, w, e.queue, e.key.String)
	if err != nil {
		return false, err
	}
	if found {
		return false, e.onto(ctx, w, there)
	}
	if e.keepDone {
		ran, doneErr := exists(ctx, w, doneKey, e.queue, e.key.String, e.now)
		if doneErr != nil || ran {
			return false, doneErr
		}
	}
	if _, err = dropFailedJob(ctx, w, e.queue, e.key.String); err != nil {
		return false, err
	}
	if err = checkRoom(ctx, w, e); err != nil {
		return false, err
	}
	return true, insertRow(ctx, w, e)
}

// onto is an Enqueue under a key whose job is there: a waiting job can come
// forward, a running one is asked for one run more, unless the queue keeps its
// keys once
func (e enqueued) onto(ctx context.Context, w sqlite.Writer, there row) error {
	leased, err := leaseHeld(ctx, w, there.id, e.now)
	switch {
	case err != nil:
		return err
	case leased && e.keepDone:
		return nil
	case leased:
		_, err = w.ExecContext(ctx, askAgain, e.queue, there.next, there.id, e.at)
	case e.at < there.next:
		if _, err = w.ExecContext(ctx, bringForward, e.queue, there.next, there.id, e.at); err == nil {
			err = moveKeyTo(ctx, w, e.queue, e.key.String, e.at)
		}
	}
	return err
}

// update gives the job under a key its new value, and its time or repeat, or
// starts a failed one again; it returns when the job is due
func update(ctx context.Context, w sqlite.Writer, e enqueued) (next int64, requeued bool, err error) {
	there, found, err := findByKey(ctx, w, e.queue, e.key.String)
	if err != nil {
		return 0, false, err
	}
	if !found {
		return requeue(ctx, w, e)
	}
	leased, err := leaseHeld(ctx, w, there.id, e.now)
	switch {
	case err != nil:
		return 0, false, err
	case leased:
		return 0, false, errNoLongerWaits
	}
	next = there.next
	if e.timed || e.repeat.Valid {
		next = e.at
	}
	if err = dropSpilled(ctx, w, there.spill); err != nil {
		return 0, false, err
	}
	spill, err := spillValue(ctx, w, there.id, e.value)
	if err == nil {
		_, err = w.ExecContext(ctx, updateWaiting, e.queue, there.next, there.id, next, inlineBytes(e.value), spill,
			e.repeat)
	}
	if err == nil && next != there.next {
		err = moveKeyTo(ctx, w, e.queue, e.key.String, next)
	}
	return next, false, err
}

// requeue starts a failed job again with an Update's value, now unless it
// named a time; with no failed job under the key there is nothing to update
func requeue(ctx context.Context, w sqlite.Writer, e enqueued) (int64, bool, error) {
	failed, err := dropFailedJob(ctx, w, e.queue, e.key.String)
	switch {
	case err != nil:
		return 0, false, err
	case !failed:
		return 0, false, errNoLongerWaits
	}
	if err := checkRoom(ctx, w, e); err != nil {
		return 0, false, err
	}
	return e.at, true, insertRow(ctx, w, e)
}

// cancel deletes the waiting or failed job under a key, with the lease an
// ended claim left, the value it spilled and its key
func cancel(ctx context.Context, w sqlite.Writer, queue int64, key string, now int64) (waited, failed bool, err error) {
	var id int64
	var spill sql.NullInt64
	err = sqlite.QueryRow(ctx, w, cancelWaiting, queue, key, now).Scan(&id, &spill)
	switch {
	case err == nil:
		if _, err = w.ExecContext(ctx, dropLeaseOf, id); err == nil {
			err = dropSpilled(ctx, w, spill)
		}
		if err == nil {
			_, err = w.ExecContext(ctx, dropKey, queue, key)
		}
		return true, false, err
	case !errors.Is(err, sql.ErrNoRows):
		return false, false, err
	}
	failed, err = dropFailedJob(ctx, w, queue, key)
	return false, failed, err
}

func insertRow(ctx context.Context, w sqlite.Writer, e enqueued) error {
	spill, err := spillValue(ctx, w, e.id, e.value)
	if err == nil {
		_, err = w.ExecContext(ctx, insertJob, e.queue, e.at, e.id, e.key, e.repeat, inlineBytes(e.value), spill)
	}
	if err == nil {
		err = keepKey(ctx, w, e.queue, e.key, e.at, e.id)
	}
	return err
}

// spillValue writes a value past the inline bound to a row of its own under
// the job's id, and returns that id, or none for a value kept in the row
func spillValue(ctx context.Context, w sqlite.Writer, id int64, value kept) (sql.NullInt64, error) {
	if value.spilled == nil {
		return sql.NullInt64{}, nil
	}
	_, err := w.ExecContext(ctx, insertSpilled, id, value.spilled)
	return sql.NullInt64{Int64: id, Valid: true}, err
}

func inlineBytes(value kept) any {
	if value.spilled != nil {
		return nil
	}
	return value.inline
}

func dropSpilled(ctx context.Context, w sqlite.Writer, spill sql.NullInt64) error {
	if !spill.Valid {
		return nil
	}
	_, err := w.ExecContext(ctx, deleteSpilled, spill.Int64)
	return err
}

// dropFailedJob deletes the failed job under a key and its spilled value
func dropFailedJob(ctx context.Context, w sqlite.Writer, queue int64, key string) (bool, error) {
	var spill sql.NullInt64
	err := sqlite.QueryRow(ctx, w, dropFailed, queue, key).Scan(&spill)
	if errors.Is(err, sql.ErrNoRows) {
		return false, nil
	}
	if err != nil {
		return false, err
	}
	return true, dropSpilled(ctx, w, spill)
}

func findByKey(ctx context.Context, r sqlite.Reader, queue int64, key string) (row, bool, error) {
	var there row
	err := sqlite.QueryRow(ctx, r, jobByKey, queue, key).Scan(&there.next, &there.id, &there.at, &there.attempt,
		&there.again, &there.repeat, &there.failure, &there.spill)
	if errors.Is(err, sql.ErrNoRows) {
		return row{}, false, nil
	}
	return there, err == nil, err
}

// leaseHeld says whether a claim holds the job now; a lease that has ended
// holds nothing, and the next claim takes the job
func leaseHeld(ctx context.Context, r sqlite.Reader, id, now int64) (bool, error) {
	return exists(ctx, r, leaseOf, id, now)
}

func exists(ctx context.Context, r sqlite.Reader, query string, args ...any) (bool, error) {
	var one int
	err := sqlite.QueryRow(ctx, r, query, args...).Scan(&one)
	if errors.Is(err, sql.ErrNoRows) {
		return false, nil
	}
	return err == nil, err
}

// Tx is one transaction of jobs.db, given to the function passed to Store.Tx
// and used by the goroutine that runs it; a queue works in it through WithTx.
type Tx struct {
	store  *Store
	writer sqlite.Writer
	open   atomic.Bool
	ids    idRange
	after  []func()
}

// Tx runs work in one writer transaction over any queues of jobs.db: nil
// commits, an error or a panic rolls back. It never spans two engines. A
// program enqueuing many jobs at once, a push to each member of a group, pays
// one commit for all of them.
func (s *Store) Tx(ctx context.Context, work func(*Tx) error) error {
	release, err := s.admitWrite(ctx)
	if err != nil {
		return err
	}
	defer release()

	var tx *Tx
	err = s.file.UpdatePrepared(ctx, func(w sqlite.Writer) error {
		tx = &Tx{store: s, writer: w}
		tx.open.Store(true)
		defer tx.open.Store(false)
		return work(tx)
	})
	if err == nil && tx != nil {
		for _, effect := range tx.after {
			effect()
		}
	}
	return err
}

func (tx *Tx) run(work func(sqlite.Writer) error) error {
	if !tx.open.Load() {
		return errTxClosed
	}
	return work(tx.writer)
}

// nextID takes an id from a block the transaction reserves itself, so that a
// rollback gives back no id another write has used
func (tx *Tx) nextID(ctx context.Context) (int64, error) {
	if !tx.open.Load() {
		return 0, errTxClosed
	}
	if tx.ids.next == tx.ids.end {
		var end int64
		if err := sqlite.QueryRow(ctx, tx.writer, reserveIDs, idBlock).Scan(&end); err != nil {
			return 0, fmt.Errorf("jobs: reserve ids: %w", err)
		}
		tx.ids.next, tx.ids.end = end-idBlock, end
	}
	tx.ids.next++
	return tx.ids.next, nil
}
