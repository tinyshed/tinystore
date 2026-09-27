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

// Job is a job in a worker's hands: its lease is held until the worker
// settles it with Ack, Retry, Fail or Snooze, or the lease ends and another
// claim may take it. Attempt is this attempt's number, one for the first.
// Copies of a Job share its lease.
type Job[V any] struct {
	Key     string
	Value   V
	At      time.Time
	Attempt int
	lease   *lease
}

// Ack settles the job as done: it leaves the queue, or a repeating job, or
// one an Enqueue asked to run again while it ran, waits for its next time.
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

	mu      sync.Mutex
	until   int64
	settled bool
}

// how a lease is settled
type outcome int

const (
	acked outcome = iota + 1
	retried
	failedForGood
	snoozed
	givenBack // Work stopped while the handler ran: due at once, the attempt not counted
	extended
)

// settlement is one outcome for one lease, with the error and time it names
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
	if l.isSettled() {
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
	}
	done.apply(l.queue)
	return nil
}

func (l *lease) isSettled() bool {
	l.mu.Lock()
	defer l.mu.Unlock()
	return l.settled
}

func (l *lease) markSettled() {
	l.mu.Lock()
	defer l.mu.Unlock()
	l.settled = true
}

// settled is what a settlement changed that memory follows once it commits
type settled struct {
	gone   bool  // the job left the queue's rows
	due    int64 // when it is due again, zero when it is not
	failed string
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
}

// what a settlement reads and writes: the lease is its token, then the job's
// row moves, leaves, or becomes a failed one
const (
	dropLease   = `delete from leases where id = ?1 and attempt = ?2`
	extendLease = `update leases set until = ?3 where id = ?1 and attempt = ?2`
	jobHeld     = `select again, repeat, error, attempt from jobs where queue = ?1 and next = ?2 and id = ?3`
	deleteJob   = `delete from jobs where queue = ?1 and next = ?2 and id = ?3 returning spill`
	moveJob     = `update jobs set next = ?4, at = ?5, attempt = ?6, again = null, error = ?7
		where queue = ?1 and next = ?2 and id = ?3`
	keepDoneKey = `insert into done (queue, key, until) values (?1, ?2, ?3)
		on conflict (queue, key) do update set until = excluded.until`
	failJob = `insert into failed (queue, id, key, at, attempt, failed, error, value, spill)
		select queue, id, key, at, ?4, ?5, ?6, value, spill from jobs where queue = ?1 and next = ?2 and id = ?3`
	dropJobRow = `delete from jobs where queue = ?1 and next = ?2 and id = ?3`
)

// write applies the settlement in the writer: the lease must still be the
// one this worker holds, or it is ErrConflict and nothing changes; its caller
// marks the lease settled once the write commits
func (s settlement) write(ctx context.Context, w sqlite.Writer, now int64) (settled, error) {
	l := s.lease
	if s.how == extended {
		return settled{}, l.extend(ctx, w, now+s.timing.after.Milliseconds())
	}
	if err := l.drop(ctx, w); err != nil {
		return settled{}, err
	}
	held, err := l.held(ctx, w)
	if err != nil {
		return settled{}, err
	}
	return s.apply(ctx, w, held, now)
}

func (l *lease) extend(ctx context.Context, w sqlite.Writer, until int64) error {
	result, err := w.ExecContext(ctx, extendLease, l.id, l.attempt, until)
	if err == nil {
		err = changedOne(result)
	}
	if err == nil {
		l.mu.Lock()
		l.until = until
		l.mu.Unlock()
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
	err := sqlite.QueryRow(ctx, w, jobHeld, l.queue.id, l.next, l.id).Scan(&h.again, &h.repeat, &h.failure, &h.attempt)
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
		return settled{due: due}, l.move(ctx, w, due, l.at, h.attempt, h.failure)
	case givenBack:
		return settled{due: now}, l.move(ctx, w, earliest(l.next, h.again), l.at, h.attempt, h.failure)
	}
	return settled{}, fmt.Errorf("jobs: an outcome of %d", s.how)
}

// ack deletes the job, or moves one that repeats, or that an Enqueue asked to
// run again, to its next time
func (l *lease) ack(ctx context.Context, w sqlite.Writer, h heldRow, now int64) (settled, error) {
	if next, again := l.nextRun(h, now); again {
		return settled{due: next}, l.move(ctx, w, next, next, 0, sql.NullString{})
	}
	var spill sql.NullInt64
	if err := sqlite.QueryRow(ctx, w, deleteJob, l.queue.id, l.next, l.id).Scan(&spill); err != nil {
		return settled{}, err
	}
	if err := dropSpilled(ctx, w, spill); err != nil {
		return settled{}, err
	}
	if keep := l.queue.policy.keepDone; keep > 0 && l.key != "" {
		if _, err := w.ExecContext(ctx, keepDoneKey, l.queue.id, l.key, now+keep.Milliseconds()); err != nil {
			return settled{}, err
		}
	}
	return settled{gone: true}, nil
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
	return settled{due: due}, l.move(ctx, w, due, l.at, l.attempt, failure)
}

// fail keeps the job as failed for good, unless it repeats, or an Enqueue
// asked it to run again, when it waits for that time with its error kept
func (l *lease) fail(ctx context.Context, w sqlite.Writer, h heldRow, now int64, cause string) (settled, error) {
	failure := sql.NullString{String: cause, Valid: true}
	if next, again := l.nextRun(h, now); again {
		return settled{due: next}, l.move(ctx, w, next, next, 0, failure)
	}
	if _, err := w.ExecContext(ctx, failJob, l.queue.id, l.next, l.id, l.attempt, now, cause); err != nil {
		return settled{}, err
	}
	_, err := w.ExecContext(ctx, dropJobRow, l.queue.id, l.next, l.id)
	return settled{gone: true, failed: cause}, err
}

// nextRun is when a settled job runs again without a retry: the next time of
// its repeat after its own and after now, or the time an Enqueue asked of it
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

func (l *lease) move(ctx context.Context, w sqlite.Writer, next, at, attempt int64, failure sql.NullString) error {
	_, err := w.ExecContext(ctx, moveJob, l.queue.id, l.next, l.id, next, at, attempt, failure)
	return err
}

func earliest(due int64, again sql.NullInt64) int64 {
	if again.Valid && again.Int64 < due {
		return again.Int64
	}
	return due
}

// backoff is the wait after attempt n failed: first, doubling, never past
// longest, a tenth longer or shorter at random so that the jobs one outage
// failed do not return in the same second
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
	claimJobs = `select next, id, key, at, attempt, repeat, value, spill from jobs j
		where queue = ?1 and next <= ?2 and not exists (select 1 from leases l where l.id = j.id and l.until > ?2)
		order by next, id limit cast(?3 as integer)`
	takeLease = `insert into leases (id, queue, next, attempt, until) values (?1, ?2, ?3, ?4, ?5)
		on conflict (id) do update set next = excluded.next, until = excluded.until,
			attempt = max(leases.attempt, excluded.attempt - 1) + 1
		returning attempt`
	nextDue = `select next from jobs j where queue = ?1
		and not exists (select 1 from leases l where l.id = j.id and l.until > ?2)
		order by next, id limit 1`
	earliestLease = `select min(until) from leases where queue = ?1 and until > ?2`
)

// claimedRow is a due job a claim leased, before its value is decoded
type claimedRow struct {
	next, id, at, attempt int64
	key, repeat           sql.NullString
	value                 []byte
	spill                 sql.NullInt64
}

// claiming is one claim's facts: its queue, its time, its lease's end, how
// many jobs it takes and how many attempts a job has
type claiming struct {
	queue, now, until  int64
	limit, maxAttempts int
}

// claimRows leases up to limit due jobs until until, and reads the values
// they spilled. A job whose attempts all ended without a settlement, its
// process dead or its worker gone each time, fails for good instead of
// running again; abandoned counts them
func claimRows(ctx context.Context, w sqlite.Writer, c claiming) (claimed []claimedRow, abandoned int, err error) {
	rows, err := w.QueryContext(ctx, claimJobs, c.queue, c.now, c.limit) //nolint:rowserrcheck // EachRow checks Err
	if err != nil {
		return nil, 0, err
	}
	var due []claimedRow
	err = sqlite.EachRow(rows, "due jobs", func(rows *sql.Rows) error {
		var row claimedRow
		scanErr := rows.Scan(&row.next, &row.id, &row.key, &row.at, &row.attempt, &row.repeat, &row.value, &row.spill)
		due = append(due, row)
		return scanErr
	})
	for i := range due {
		if err != nil {
			break
		}
		var leased bool
		if leased, err = leaseRow(ctx, w, c, &due[i]); leased {
			claimed = append(claimed, due[i])
		} else if err == nil {
			abandoned++
		}
	}
	return claimed, abandoned, err
}

const dropLeaseRow = `delete from leases where id = ?1`

// leaseRow leases one due job, or fails it for good when this would be an
// attempt past the queue's MaxAttempts
func leaseRow(ctx context.Context, w sqlite.Writer, c claiming, row *claimedRow) (bool, error) {
	err := sqlite.QueryRow(ctx, w, takeLease, row.id, c.queue, row.next, row.attempt+1, c.until).Scan(&row.attempt)
	if err != nil {
		return false, err
	}
	if int(row.attempt) > c.maxAttempts {
		return false, abandon(ctx, w, c, *row)
	}
	if !row.spill.Valid {
		return true, nil
	}
	return true, sqlite.QueryRow(ctx, w, valueSpilled, row.spill.Int64).Scan(&row.value)
}

// abandon fails for good a job whose every attempt ended without a settlement
func abandon(ctx context.Context, w sqlite.Writer, c claiming, row claimedRow) error {
	cause := fmt.Sprintf("each of its %d attempts ended without a settlement: its process died or its worker "+
		"vanished while it ran", row.attempt-1)
	if _, err := w.ExecContext(ctx, dropLeaseRow, row.id); err != nil {
		return err
	}
	if _, err := w.ExecContext(ctx, failJob, c.queue, row.next, row.id, row.attempt-1, c.now, cause); err != nil {
		return err
	}
	_, err := w.ExecContext(ctx, dropJobRow, c.queue, row.next, row.id)
	return err
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
// settles it, or the lease ends and another claim may take it.
func (q *Queue[V]) Claim(ctx context.Context, options ...ClaimOption) (Job[V], bool, error) {
	settings, err := collectClaim(q.state.policy, options)
	if err != nil {
		return Job[V]{}, false, q.fail("", err)
	}
	for {
		c := q.claiming(1, settings.lease)
		var claimed []claimedRow
		abandoned := 0
		err = q.write(ctx, 0, func(w sqlite.Writer) (writeErr error) {
			claimed, abandoned, writeErr = claimRows(ctx, w, c)
			return writeErr
		})
		if err != nil {
			return Job[V]{}, false, q.fail("", err)
		}
		q.state.abandoned(abandoned)
		if len(claimed) == 0 && abandoned > 0 {
			continue
		}
		if len(claimed) == 0 {
			return Job[V]{}, false, nil
		}
		q.state.alarm.lower(c.until)
		job, decodeErr := q.jobOf(claimed[0], c.until)
		if decodeErr == nil {
			return job, true, nil
		}
		if err = job.lease.settleNow(ctx, settlement{how: failedForGood, cause: decodeErr}); err != nil {
			return Job[V]{}, false, err
		}
	}
}

// jobOf makes a claimed row a Job; a value that no longer decodes leaves the
// Job with its lease, for the caller to fail
func (q *Queue[V]) jobOf(c claimedRow, until int64) (Job[V], error) {
	job := Job[V]{
		Key: c.key.String, At: time.UnixMilli(c.at), Attempt: int(c.attempt),
		lease: &lease{
			queue: q.state, store: q.store, next: c.next, id: c.id, attempt: c.attempt, at: c.at,
			key: c.key.String, repeat: c.repeat.String, spill: c.spill, until: until,
		},
	}
	var err error
	job.Value, err = q.codec.decode(c.value)
	return job, err
}

// claiming is a claim of up to limit jobs now, leased for lease
func (q *Queue[V]) claiming(limit int, lease time.Duration) claiming {
	now := q.store.clock()
	return claiming{
		queue: q.state.id, now: now, until: now + lease.Milliseconds(), limit: limit,
		maxAttempts: q.state.policy.maxAttempts,
	}
}

// abandoned follows in memory the jobs a claim failed for good
func (q *queueState) abandoned(count int) {
	if count == 0 {
		return
	}
	q.waiting.Add(-int64(count))
	for range count {
		q.failures.observe(time.Now(), "its attempts ended without a settlement")
	}
}
