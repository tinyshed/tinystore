package jobs

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"math/rand/v2"
	"strings"
	"sync"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/internal/sqlite"
)

var (
	errLeaseLost  = fmt.Errorf("%w: jobs: the job's lease ended and another claim took it", tinystore.ErrConflict)
	errNotClaimed = fmt.Errorf("%w: jobs: a Job that no Claim returned", tinystore.ErrInvalid)
	errSettled    = fmt.Errorf("%w: jobs: the job was settled already", tinystore.ErrConflict)
)

// Job is a job in a worker's hands. Its lease is held until the worker settles
// it with Ack, Retry, Fail or Snooze, or the lease ends and another claim may
// take it. Copies of a Job share its lease.
type Job[V any] struct {
	Key   string
	Value V
	At    time.Time
	// Attempt is this attempt's number, one for the first.
	Attempt int
	lease   *lease
}

// Ack settles the job as done and it leaves the queue. A repeating job, or one
// an Enqueue asked to run again while it ran, waits for its next time instead.
func (j Job[V]) Ack(ctx context.Context) error {
	return j.lease.settleNow(ctx, settlement{how: acked})
}

// Retry settles a failed attempt: the job runs again after the queue's
// backoff, or at jobs.At or jobs.After, and past MaxAttempts it fails for good.
func (j Job[V]) Retry(ctx context.Context, cause error, options ...SettleOption) error {
	s, err := collectSettle(options)
	if err != nil {
		return err
	}
	return j.lease.settleNow(ctx, settlement{how: retried, cause: cause, timing: s})
}

// Fail settles the job as failed for good: no more attempts. A repeating job
// waits for its next time instead, its last error kept.
func (j Job[V]) Fail(ctx context.Context, cause error) error {
	return j.lease.settleNow(ctx, settlement{how: failedForGood, cause: cause})
}

// Snooze puts the job back until jobs.At or jobs.After without counting the
// attempt: a provider that asks for a minute, a row that says later.
func (j Job[V]) Snooze(ctx context.Context, options ...SettleOption) error {
	s, err := collectSettle(options)
	if err == nil && !s.timed {
		err = fmt.Errorf("%w: jobs: Snooze needs jobs.At or jobs.After", tinystore.ErrInvalid)
	}
	if err != nil {
		return err
	}
	return j.lease.settleNow(ctx, settlement{how: snoozed, timing: s})
}

// Extend makes the lease run d from now; Work extends its handlers' leases
// itself.
func (j Job[V]) Extend(ctx context.Context, d time.Duration) error {
	if d <= 0 {
		return fmt.Errorf("%w: jobs: Extend(%v)", tinystore.ErrInvalid, d)
	}
	return j.lease.settleNow(ctx, settlement{how: extended, timing: settleSettings{after: d, timed: true}})
}

// lease is a claimed job as its worker holds it: where its row is, the attempt
// that is its token, and whether it was settled
type lease struct {
	queue   *queueState
	store   *Store
	next    int64
	id      int64
	attempt int64
	at      int64
	key     string
	repeat  string
	spill   sql.NullInt64
	group   sql.NullString

	mu        sync.Mutex
	until     int64
	began     int64 // when a handler, or a Claim's caller, began the job; zero until one did
	settled   bool
	lost      bool // another claim, or a Cancel, took the job after the lease ended
	cancelled bool // Cancel took the job while the lease held it
}

type outcome int

const (
	acked outcome = iota + 1
	retried
	failedForGood
	snoozed
	givenBack // Work stopped while the handler ran: due at once, the attempt not counted
	extended
)

type settlement struct {
	lease  *lease
	how    outcome
	cause  error
	timing settleSettings
}

// settleNow writes one settlement grouped with the other writes waiting
func (l *lease) settleNow(ctx context.Context, s settlement) error {
	if l == nil {
		return errNotClaimed
	}
	if l.wasCancelled() {
		return fmt.Errorf("%w: %w", tinystore.ErrConflict, ErrCancelled)
	}
	if settled, lost := l.state(); lost {
		return errLeaseLost
	} else if settled {
		return errSettled
	}
	s.lease = l
	release, err := l.store.admitWrite(ctx)
	if err != nil {
		return err
	}
	defer release()

	var done settled
	err = l.store.file.UpdateGrouped(ctx, 0, func(w sqlite.Writer) (writeErr error) {
		done, writeErr = s.write(ctx, w, l.store.clock())
		return writeErr
	})
	if err != nil {
		return nameFailure(l.queue.name, l.key, err)
	}
	if s.how != extended {
		l.markSettled()
		l.queue.watch.settled(l, done.ended, done.failed)
		l.queue.roomMade(l.store.clock())
	}
	done.applyLease(l)
	done.apply(l.queue)
	return nil
}

func (l *lease) isSettled() bool {
	settled, _ := l.state()
	return settled
}

func (l *lease) state() (settled, lost bool) {
	l.mu.Lock()
	defer l.mu.Unlock()
	return l.settled, l.lost
}

func (l *lease) markSettled() {
	l.mu.Lock()
	defer l.mu.Unlock()
	l.settled = true
}

func (l *lease) markLost() {
	l.mu.Lock()
	defer l.mu.Unlock()
	l.settled, l.lost = true, true
}

// markCancelled says Cancel took the job: its lease settles nothing, and its
// losing it is no news to log
func (l *lease) markCancelled() {
	l.mu.Lock()
	defer l.mu.Unlock()
	l.settled, l.lost, l.cancelled = true, true, true
}

func (l *lease) wasCancelled() bool {
	l.mu.Lock()
	defer l.mu.Unlock()
	return l.cancelled
}

// begin says a handler, or a Claim's caller, began the job at now
func (l *lease) begin(now int64) {
	l.mu.Lock()
	defer l.mu.Unlock()
	l.began = now
}

// lastRun is the run a settlement at now records: when it began and how long
// it took, none when nothing began it
func (l *lease) lastRun(now int64) (ran, took sql.NullInt64) {
	l.mu.Lock()
	began := l.began
	l.mu.Unlock()
	if began == 0 {
		return sql.NullInt64{}, sql.NullInt64{}
	}
	return sql.NullInt64{Int64: began, Valid: true}, sql.NullInt64{Int64: max(now-began, 0), Valid: true}
}

// settled is what a settlement changed that memory follows once it commits
type settled struct {
	gone   bool  // the job left the queue's rows
	ended  State // how it left them: Done or Failed
	due    int64 // when it is due again, zero when it is not
	failed string
	lost   bool  // the lease was no longer the settlement's: nothing was written
	until  int64 // a committed lease extension
}

func (d settled) applyLease(l *lease) {
	if d.until == 0 {
		return
	}
	l.mu.Lock()
	l.until = d.until
	l.mu.Unlock()
}

func (d settled) apply(q *queueState) {
	if d.gone {
		q.waiting.Add(-1)
	}
	if d.due != 0 {
		q.alarm.lower(d.due)
	}
	if d.failed != "" {
		q.failures.observe(time.Now(), d.failed)
	}
	if d.lost {
		q.lost.observe(time.Now(), "a lease ended while its handler ran, and another claim or a Cancel took the job")
	}
}

// what a settlement reads and writes: the lease is its token, then the job's
// row moves, leaves, or becomes a failed one
const (
	dropLease   = `delete from _tinystore_jobs_leases where id = ?1 and attempt = ?2`
	extendLease = `update _tinystore_jobs_leases set until = ?3 where id = ?1 and attempt = ?2`
	jobHeld     = `select again, repeat, error, attempt from _tinystore_jobs where queue = ?1 and next = ?2 and id = ?3`
	deleteJob   = `delete from _tinystore_jobs where queue = ?1 and next = ?2 and id = ?3 returning spill`
	deleteDone  = `delete from _tinystore_jobs where queue = ?1 and next = ?2 and id = ?3
		and again is null and repeat is null
		returning spill`
	moveJob = `update _tinystore_jobs set next = ?4, at = ?5, attempt = ?6, again = null, error = ?7,
			ran = coalesce(?8, ran), took = coalesce(?9, took)
		where queue = ?1 and next = ?2 and id = ?3`
	keepDoneKey = `insert into _tinystore_jobs_done (queue, key, until) values (?1, ?2, ?3)
		on conflict (queue, key) do update set until = excluded.until`
	failJob = `insert into _tinystore_jobs_failed (queue, id, key, at, attempt, failed, error, value, spill, ran, took,
			grp)
		select queue, id, key, at, ?4, ?5, ?6, value, spill, coalesce(?7, ran), coalesce(?8, took), grp
		from _tinystore_jobs where queue = ?1 and next = ?2 and id = ?3`
	dropJobRow = `delete from _tinystore_jobs where queue = ?1 and next = ?2 and id = ?3`
)

// write applies the settlement in the writer. The lease must still be the one
// this worker holds, or it is ErrConflict and nothing changes. Its caller marks
// the lease settled once the write commits.
func (s settlement) write(ctx context.Context, w sqlite.Writer, now int64) (settled, error) {
	l := s.lease
	if s.how == extended {
		until := now + s.timing.after.Milliseconds()
		return settled{until: until}, l.extend(ctx, w, until)
	}
	if err := l.drop(ctx, w); err != nil {
		return settled{}, err
	}
	if err := l.freePlace(ctx, w, s.how); err != nil {
		return settled{}, err
	}
	if s.how == acked {
		if done, gone, err := l.ackAlone(ctx, w, now); gone || err != nil {
			return done, err
		}
	}
	held, err := l.held(ctx, w)
	if err != nil {
		return settled{}, err
	}
	return s.apply(ctx, w, held, now)
}

// ackAlone deletes an acknowledged job that neither repeats nor was asked to
// run again, reading nothing first. gone is false for a job that must move to
// its next time instead.
func (l *lease) ackAlone(ctx context.Context, w sqlite.Writer, now int64) (settled, bool, error) {
	var spill sql.NullInt64
	err := sqlite.QueryRowByKey(ctx, w, deleteDone, l.queue.id, l.next, l.id).Scan(&spill)
	if errors.Is(err, sql.ErrNoRows) {
		return settled{}, false, nil
	}
	if err != nil {
		return settled{}, false, err
	}
	done, err := l.forget(ctx, w, spill, now)
	return done, true, err
}

// freePlace gives the place a settled job took in its group to the group's
// first parked job. A job given back is due at once and takes its place again.
func (l *lease) freePlace(ctx context.Context, w sqlite.Writer, how outcome) error {
	if how == givenBack || !l.group.Valid || l.queue.policy.maxInGroup == 0 {
		return nil
	}
	return unparkOne(ctx, w, l.queue.id, l.group.String)
}

func (l *lease) extend(ctx context.Context, w sqlite.Writer, until int64) error {
	result, err := w.ExecContext(ctx, extendLease, l.id, l.attempt, until)
	if err == nil {
		err = changedOne(result)
	}
	return err
}

// drop takes the lease row, the settlement's token: none says another claim
// has taken the job since this one's lease ended
func (l *lease) drop(ctx context.Context, w sqlite.Writer) error {
	result, err := w.ExecContext(ctx, dropLease, l.id, l.attempt)
	if err != nil {
		return err
	}
	return changedOne(result)
}

func changedOne(result sql.Result) error {
	changed, err := result.RowsAffected()
	if err == nil && changed == 0 {
		return errLeaseLost
	}
	return err
}

// heldRow is what a settlement needs of the job's row
type heldRow struct {
	again   sql.NullInt64
	repeat  sql.NullString
	failure sql.NullString
	attempt int64
}

func (l *lease) held(ctx context.Context, w sqlite.Writer) (heldRow, error) {
	var h heldRow
	err := sqlite.QueryRowByKey(ctx, w, jobHeld, l.queue.id, l.next, l.id).
		Scan(&h.again, &h.repeat, &h.failure, &h.attempt)
	if errors.Is(err, sql.ErrNoRows) {
		return h, errLeaseLost
	}
	return h, err
}

// apply writes what the outcome does to the job's row
func (s settlement) apply(ctx context.Context, w sqlite.Writer, h heldRow, now int64) (settled, error) {
	l := s.lease
	switch s.how {
	case acked:
		return l.ack(ctx, w, h, now)
	case retried:
		return l.retry(ctx, w, h, now, s)
	case failedForGood:
		return l.fail(ctx, w, h, now, describe(s.cause))
	case snoozed:
		due := earliest(when(s.timing.at, s.timing.after, true, now, now), h.again)
		to := moved{next: due, at: l.at, attempt: h.attempt, failure: h.failure}
		return settled{due: due}, l.move(ctx, w, l.withRun(to, now))
	case givenBack:
		to := moved{next: earliest(l.next, h.again), at: l.at, attempt: h.attempt, failure: h.failure}
		return settled{due: now}, l.move(ctx, w, to)
	}
	return settled{}, fmt.Errorf("jobs: an outcome of %d", s.how)
}

// ack deletes the job, or moves one that repeats, or that an Enqueue asked to
// run again, to its next time
func (l *lease) ack(ctx context.Context, w sqlite.Writer, h heldRow, now int64) (settled, error) {
	if next, again := l.nextRun(h, now); again {
		if err := l.runEnded(ctx, w); err != nil {
			return settled{}, err
		}
		return settled{due: next}, l.move(ctx, w, l.withRun(moved{next: next, at: next}, now))
	}
	var spill sql.NullInt64
	if err := sqlite.QueryRowByKey(ctx, w, deleteJob, l.queue.id, l.next, l.id).Scan(&spill); err != nil {
		return settled{}, err
	}
	return l.forget(ctx, w, spill, now)
}

// forget drops the value a deleted job spilled, and remembers its key done
// when the queue keeps done keys
func (l *lease) forget(ctx context.Context, w sqlite.Writer, spill sql.NullInt64, now int64) (settled, error) {
	if err := dropSpilled(ctx, w, spill); err != nil {
		return settled{}, err
	}
	if keep := l.queue.policy.keepDone; keep > 0 && l.key != "" {
		if _, err := w.ExecContext(ctx, keepDoneKey, l.queue.id, l.key, now+keep.Milliseconds()); err != nil {
			return settled{}, err
		}
	}
	return settled{gone: true, ended: Done}, nil
}

// retry moves the job to its next attempt's time, or fails it past its attempts
func (l *lease) retry(ctx context.Context, w sqlite.Writer, h heldRow, now int64, s settlement) (settled, error) {
	cause := describe(s.cause)
	if int(l.attempt) >= l.queue.policy.maxAttempts {
		return l.fail(ctx, w, h, now, cause)
	}
	due := when(s.timing.at, s.timing.after, s.timing.timed, now, now+backoff(l.queue.policy, l.attempt))
	if next, again := l.nextRun(h, now); again {
		due = min(due, next)
	}
	failure := sql.NullString{String: cause, Valid: true}
	to := moved{next: due, at: l.at, attempt: l.attempt, failure: failure}
	return settled{due: due}, l.move(ctx, w, l.withRun(to, now))
}

// fail keeps the job as failed for good. A job that repeats, or that an Enqueue
// asked to run again, waits for that time instead, with its error kept.
func (l *lease) fail(ctx context.Context, w sqlite.Writer, h heldRow, now int64, cause string) (settled, error) {
	failure := sql.NullString{String: cause, Valid: true}
	if next, again := l.nextRun(h, now); again {
		if err := l.runEnded(ctx, w); err != nil {
			return settled{}, err
		}
		return settled{due: next}, l.move(ctx, w, l.withRun(moved{next: next, at: next, failure: failure}, now))
	}
	ran, took := l.lastRun(now)
	if _, err := w.ExecContext(ctx, failJob, l.queue.id, l.next, l.id, l.attempt, now, cause, ran, took); err != nil {
		return settled{}, err
	}
	_, err := w.ExecContext(ctx, dropJobRow, l.queue.id, l.next, l.id)
	return settled{gone: true, ended: Failed, failed: cause}, err
}

// nextRun is when a settled job runs again without a retry.
//
// It is the earlier of its repeat's next time (after its own and after now)
// and the time an Enqueue asked of it.
func (l *lease) nextRun(h heldRow, now int64) (int64, bool) {
	next := int64(0)
	if h.repeat.Valid {
		repeat, err := l.store.repeatOf(h.repeat.String)
		if err != nil {
			l.store.log.Error("a repeating job's repeat does not read", "queue", l.queue.name, "key", l.key,
				"error", err)
		} else {
			next = repeat.next(time.UnixMilli(max(l.at, now))).UnixMilli()
		}
	}
	if h.again.Valid && (next == 0 || h.again.Int64 < next) {
		next = h.again.Int64
	}
	return next, next != 0
}

// moved is where a settlement moves a job's row: its next time and the time
// it runs for, the attempts counted, its error, and the run it records, none
// for a job given back, which keeps the one before
type moved struct {
	next, at, attempt int64
	failure           sql.NullString
	ran, took         sql.NullInt64
}

// withRun is a move that records the run the lease began, as a settlement at
// now finishes it
func (l *lease) withRun(to moved, now int64) moved {
	to.ran, to.took = l.lastRun(now)
	return to
}

func (l *lease) move(ctx context.Context, w sqlite.Writer, to moved) error {
	_, err := w.ExecContext(ctx, moveJob, l.queue.id, l.next, l.id, to.next, to.at, to.attempt, to.failure, to.ran,
		to.took)
	if err == nil && to.next != l.next {
		err = moveKeyTo(ctx, w, l.queue.id, l.key, to.next)
	}
	return err
}

func earliest(due int64, again sql.NullInt64) int64 {
	if again.Valid && again.Int64 < due {
		return again.Int64
	}
	return due
}

// backoff is the wait after attempt n failed: first, doubling, never past
// longest, and a tenth longer or shorter at random so that the jobs one outage
// failed do not return in the same second.
//
//	first 1s, longest 1h: 1s, 2s, 4s … 34m8s, 1h, 1h … each ± 10 %
func backoff(p policy, attempt int64) int64 {
	wait := p.first
	for i := int64(1); i < attempt && wait < p.longest; i++ {
		wait *= 2
	}
	wait = min(wait, p.longest)
	jitter := time.Duration(rand.Int64N(int64(wait)/5+1)) - wait/10 //nolint:gosec // jitter, not a secret
	return (wait + jitter).Milliseconds()
}

// describe is a failure as a job's row keeps it: its text, bounded
func describe(cause error) string {
	if cause == nil {
		return "failed without an error"
	}
	text := cause.Error()
	if len(text) > 4096 {
		text = text[:4096] + "…"
	}
	return strings.ToValidUTF8(text, "?")
}

// what a claim reads and writes: the due jobs no live lease holds, then a
// lease each, whose attempt counts the attempt a lease that ended held
const (
	claimJobs = `select next, id, key, at, attempt, repeat, value, spill,
			coalesce(length(j.value), (select length(s.value) from _tinystore_jobs_spilled s where s.id = j.spill), 0),
			grp
		from _tinystore_jobs j
		where queue = ?1 and next <= ?2
			and not exists (select 1 from _tinystore_jobs_leases l where l.id = j.id and l.until > ?2)
		order by next, id limit cast(?3 as integer)`
	takeLease = `insert into _tinystore_jobs_leases (id, queue, next, attempt, until) values (?1, ?2, ?3, ?4, ?5)
		on conflict (id) do update set next = excluded.next, until = excluded.until,
			attempt = max(_tinystore_jobs_leases.attempt, excluded.attempt - 1) + 1
		returning attempt`
	nextDue = `select next from _tinystore_jobs j where queue = ?1 and next < ` + parkedFromText + `
		and not exists (select 1 from _tinystore_jobs_leases l where l.id = j.id and l.until > ?2)
		order by next, id limit 1`
	earliestLease = `select min(until) from _tinystore_jobs_leases where queue = ?1 and until > ?2`
	liveLeases    = `select count(*) from _tinystore_jobs_leases where queue = ?1 and until > ?2`
)

// claimedRow is a due job a claim leased. It carries the value when the row
// holds it, or where it spilled, and its size, so that memory holds room for
// it before anyone reads it.
type claimedRow struct {
	next, id, at, attempt int64
	key, repeat           sql.NullString
	value                 []byte
	spill                 sql.NullInt64
	size                  int
	group                 sql.NullString
	until                 int64 // the lease's end
}

type claiming struct {
	queue, now, until  int64
	limit, maxAttempts int
	maxRunning         int // the jobs the queue's leases may hold at once, none when zero
	maxInGroup         int // the jobs one group's leases may hold at once, none when zero
	rate               *rateLog
}

// claimed is what a claim leased, the jobs it failed for good instead, and
// whether it stopped at its bound of jobs parked with more due ones to look at
type claimed struct {
	rows      []claimedRow
	abandoned []int64
	more      bool
}

// claimRows leases up to limit due jobs until until, leaving the values they
// spilled for their workers to read outside the writer.
//
// A job whose attempts all ended without a settlement, its process dead or its
// worker gone each time, fails for good instead of running again. abandoned
// names them.
//
// A queue with MaxRunning leases no more than its room, the writer counting
// the live leases so that no two claims both see the last place free; one with
// a Rate no more than the rate lets start.
func claimRows(ctx context.Context, w sqlite.Writer, c claiming) (claimed, error) {
	limit, err := c.room(ctx, w)
	if err != nil || limit == 0 {
		return claimed{}, err
	}
	c.limit = c.rate.take(c.now, limit)
	if c.limit == 0 {
		return claimed{}, nil
	}
	var got claimed
	if c.maxInGroup > 0 {
		got, err = claimByGroup(ctx, w, c)
	} else {
		got, err = claimInOrder(ctx, w, c)
	}
	if err != nil {
		c.rate.giveBack(c.limit)
		return claimed{}, err
	}
	c.rate.giveBack(c.limit - len(got.rows))
	return got, nil
}

// claimInOrder leases the first limit due jobs in the order of their time
func claimInOrder(ctx context.Context, w sqlite.Writer, c claiming) (claimed, error) {
	due, err := dueRows(ctx, w, c, c.limit)
	if err != nil {
		return claimed{}, err
	}
	var got claimed
	for i := range due {
		if _, err = got.lease(ctx, w, c, &due[i]); err != nil {
			return got, err
		}
	}
	return got, nil
}

// dueRows reads up to limit due jobs no live lease holds, in the order of
// their time
func dueRows(ctx context.Context, w sqlite.Writer, c claiming, limit int) ([]claimedRow, error) {
	rows, err := w.QueryContext(ctx, claimJobs, c.queue, c.now, limit) //nolint:rowserrcheck // EachRow checks Err
	if err != nil {
		return nil, err
	}
	var due []claimedRow
	err = sqlite.EachRow(rows, "due jobs", func(rows *sql.Rows) error {
		var row claimedRow
		scanErr := rows.Scan(&row.next, &row.id, &row.key, &row.at, &row.attempt, &row.repeat, &row.value, &row.spill,
			&row.size, &row.group)
		row.until = c.until
		due = append(due, row)
		return scanErr
	})
	return due, err
}

const dropLeaseRow = `delete from _tinystore_jobs_leases where id = ?1`

// room is how many jobs the claim may lease: its limit, and for a queue with
// MaxRunning no more than the places its live leases leave
func (c claiming) room(ctx context.Context, w sqlite.Writer) (int, error) {
	if c.maxRunning == 0 || c.limit == 0 {
		return c.limit, nil
	}
	var held int
	if err := sqlite.QueryRow(ctx, w, liveLeases, c.queue, c.now).Scan(&held); err != nil {
		return 0, err
	}
	return max(min(c.limit, c.maxRunning-held), 0), nil
}

// full says a claim of a queue with MaxRunning found every place taken, so
// that only a settlement or a lease's end makes room
func (c claiming) full(ctx context.Context, w sqlite.Writer) (bool, error) {
	if c.maxRunning == 0 {
		return false, nil
	}
	room, err := claiming{queue: c.queue, now: c.now, limit: 1, maxRunning: c.maxRunning}.room(ctx, w)
	return room == 0, err
}

// leaseRow leases one due job, or fails it for good when this would be an
// attempt past the queue's MaxAttempts
func leaseRow(ctx context.Context, w sqlite.Writer, c claiming, row *claimedRow) (bool, error) {
	err := sqlite.QueryRowByKey(ctx, w, takeLease, row.id, c.queue, row.next, row.attempt+1, c.until).Scan(&row.attempt)
	if err != nil {
		return false, err
	}
	if int(row.attempt) > c.maxAttempts {
		return false, abandon(ctx, w, c, *row)
	}
	return true, nil
}

// abandon fails for good a job whose every attempt ended without a settlement
func abandon(ctx context.Context, w sqlite.Writer, c claiming, row claimedRow) error {
	cause := fmt.Sprintf("each of its %d attempts ended without a settlement: its process died or its worker "+
		"vanished while it ran", row.attempt-1)
	if _, err := w.ExecContext(ctx, dropLeaseRow, row.id); err != nil {
		return err
	}
	_, err := w.ExecContext(ctx, failJob, c.queue, row.next, row.id, row.attempt-1, c.now, cause, nil, nil)
	if err != nil {
		return err
	}
	_, err = w.ExecContext(ctx, dropJobRow, c.queue, row.next, row.id)
	return err
}

// nextClaim is when the queue next needs a claim: nextTime, or for a queue
// whose MaxRunning places are all taken the first live lease's end, since only
// that or a settlement, which wakes the loops itself, makes room
func nextClaim(ctx context.Context, w sqlite.Writer, c claiming) (int64, error) {
	full, err := c.full(ctx, w)
	if err != nil || !full {
		return nextTime(ctx, w, c.queue, c.now)
	}
	var until sql.NullInt64
	err = sqlite.QueryRow(ctx, w, earliestLease, c.queue, c.now).Scan(&until)
	return until.Int64, err
}

// nextTime is when the queue next needs a claim: its first due job no live
// lease holds, or the first live lease's end; zero when neither is
func nextTime(ctx context.Context, w sqlite.Writer, queue, now int64) (int64, error) {
	var due, until sql.NullInt64
	err := sqlite.QueryRow(ctx, w, nextDue, queue, now).Scan(&due)
	if err != nil && !errors.Is(err, sql.ErrNoRows) {
		return 0, err
	}
	if err = sqlite.QueryRow(ctx, w, earliestLease, queue, now).Scan(&until); err != nil {
		return 0, err
	}
	switch {
	case due.Valid && until.Valid:
		return min(due.Int64, until.Int64), nil
	case due.Valid:
		return due.Int64, nil
	}
	return until.Int64, nil
}

// Claim leases the next due job to the caller for the queue's Lease, or
// jobs.Lease's, and says whether one was due; it does not wait. The caller
// settles it, or the lease ends and another claim may take it. A job whose
// value no longer reads into V fails for good, and the claim takes the next.
func (q *Queue[V]) Claim(ctx context.Context, options ...ClaimOption) (Job[V], bool, error) {
	settings, err := collectClaim(q.state.policy, options)
	if err != nil {
		return Job[V]{}, false, q.fail("", err)
	}
	for {
		row, found, err := q.claimOne(ctx, settings.lease)
		if err != nil || !found {
			return Job[V]{}, false, err
		}

		job := q.jobOf(row)
		job.Value, err = q.valueHeld(ctx, row)
		if err == nil && q.state.watch.started(job.lease, nil) {
			return job, true, nil
		}
		if err == nil {
			continue // Cancel took it as it was claimed
		}
		if !errors.Is(err, errUnreadable) {
			return Job[V]{}, false, q.fail(job.Key, err)
		}
		if err = job.lease.settleNow(ctx, settlement{how: failedForGood, cause: err}); err != nil {
			return Job[V]{}, false, err
		}
	}
}

// claimOne leases the next due job, passing over the jobs it fails for good
// because their attempts all ended without a settlement, and those it parks
// because their groups have no room
func (q *Queue[V]) claimOne(ctx context.Context, lease time.Duration) (claimedRow, bool, error) {
	for {
		c := q.claiming(1, lease)
		var got claimed
		err := q.write(ctx, claimRowMemory, func(w sqlite.Writer) (writeErr error) {
			got, writeErr = claimRows(ctx, w, c)
			return writeErr
		})
		if err != nil {
			return claimedRow{}, false, q.fail("", err)
		}
		q.state.abandoned(got.abandoned)
		if len(got.rows) > 0 {
			q.state.alarm.lower(c.until)
			return got.rows[0], true, nil
		}
		if len(got.abandoned) == 0 && !got.more {
			return claimedRow{}, false, nil
		}
	}
}

// valueHeld reads a claimed job's value once the store's memory holds room
// for it, and gives the room back: the value is the caller's from then on
func (q *Queue[V]) valueHeld(ctx context.Context, row claimedRow) (V, error) {
	reserved, err := q.store.reserve(ctx, row.size)
	if err != nil {
		var zero V
		return zero, err
	}
	defer reserved.Release()
	return q.valueOf(ctx, row)
}

// valueOf reads a claimed job's value, from a reader when it spilled; one that
// no longer decodes is errUnreadable
func (q *Queue[V]) valueOf(ctx context.Context, row claimedRow) (V, error) {
	encoded := row.value
	if row.spill.Valid {
		err := q.store.file.Lookup(ctx, func(r sqlite.Reader) error {
			return sqlite.QueryRowByKey(ctx, r, valueSpilled, row.spill.Int64).Scan(&encoded)
		})
		if err != nil {
			var zero V
			return zero, fmt.Errorf("jobs: read the value a claimed job spilled: %w", err)
		}
	}
	return q.codec.decode(encoded)
}

// jobOf makes a claimed row a Job, its value not yet read
func (q *Queue[V]) jobOf(c claimedRow) Job[V] {
	return Job[V]{
		Key: c.key.String, At: time.UnixMilli(c.at), Attempt: int(c.attempt),
		lease: &lease{
			queue: q.state, store: q.store, next: c.next, id: c.id, attempt: c.attempt, at: c.at,
			key: c.key.String, repeat: c.repeat.String, spill: c.spill, group: c.group, until: c.until,
		},
	}
}

func (q *Queue[V]) claiming(limit int, lease time.Duration) claiming {
	return q.state.claiming(q.store.clock(), limit, lease)
}

func (q *queueState) claiming(now int64, limit int, lease time.Duration) claiming {
	return claiming{
		queue: q.id, now: now, until: now + lease.Milliseconds(), limit: limit,
		maxAttempts: q.policy.maxAttempts, maxRunning: q.policy.maxRunning, maxInGroup: q.policy.maxInGroup,
		rate: q.rate,
	}
}

// roomMade wakes the Work loops of a queue with MaxRunning or
// MaxRunningInGroup once a settlement has given a place back, since a claim
// that found none, or parked the jobs of a full group, waits for that
func (q *queueState) roomMade(now int64) {
	if q.policy.maxRunning > 0 || q.policy.maxInGroup > 0 {
		q.alarm.lower(now)
	}
}

// abandoned follows in memory the jobs a claim failed for good
func (q *queueState) abandoned(ids []int64) {
	if len(ids) == 0 {
		return
	}
	q.waiting.Add(-int64(len(ids)))
	for range ids {
		q.failures.observe(time.Now(), "its attempts ended without a settlement")
	}
	q.watch.forget(ids)
}
