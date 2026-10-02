package jobs

import (
	"context"
	"errors"
	"fmt"
	"runtime/debug"
	"sync"
	"sync/atomic"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/internal/sqlite"
)

// the longest the settlements of a stopping Work may take to write
const lastWrite = 10 * time.Second

// Work claims the queue's jobs as they fall due and runs handle on each, until
// ctx ends or the store closes, or, with UntilIdle, until nothing is due and
// nothing runs. It runs up to Workers goroutines, which it starts itself and
// waits for before it returns.
//
// A handler's nil acknowledges its job and an error retries it by the queue's
// policy. A handler that settles its job itself is left as it settled it, and a
// panic is an error. While a handler runs its lease is extended. A handler
// stopped by ctx or by Close gives its job back, the attempt not counted.
//
// Each worker's next job is claimed while it runs the one before, and a job
// claimed is settled in the same write that claims the next ones, so that a
// queue under load commits once for many jobs.
func (q *Queue[V]) Work(ctx context.Context, handle func(context.Context, Job[V]) error, options ...WorkOption) error {
	settings, err := collectWork(options)
	if err == nil && q.tx != nil {
		err = fmt.Errorf("%w: jobs: Work inside a transaction", tinystore.ErrInvalid)
	}
	if err != nil {
		return q.fail("", err)
	}
	hold := settings.workers * claimAhead
	if q.state.policy.maxRunning > 0 {
		hold = settings.workers
	}
	return q.work(ctx, handle, settings, hold)
}

// work runs a Work loop that holds at most hold jobs: running, or claimed for
// a worker still busy with the one before
func (q *Queue[V]) work(ctx context.Context, handle func(context.Context, Job[V]) error, settings workSettings,
	hold int,
) error {
	leave, err := q.store.admit(ctx)
	if err != nil {
		return err
	}
	defer leave()
	return newWorkLoop(q, handle, settings, hold).run(ctx)
}

// workLoop is one Work: the jobs it holds, which its workers run, and the
// settlements it has yet to write
type workLoop[V any] struct {
	q        *Queue[V]
	handle   func(context.Context, Job[V]) error
	settings workSettings
	hold     int                 // jobs the loop may hold: running, or claimed for a worker
	hand     chan handed[V]      // claimed jobs, for the workers
	finished chan settlement     // what each handler's return says
	holding  map[*lease]struct{} // the leases of the jobs it holds, which it extends
	pending  []settlement
}

// handed is a claimed job on its way to a worker, which reads its value
type handed[V any] struct {
	job    Job[V]
	row    claimedRow
	memory *claimMemory
}

type claimMemory struct {
	reserved *tinystore.Reservation
	left     atomic.Int64
}

func (m *claimMemory) releaseRow() {
	m.reserved.Shrink(m.left.Add(-1) * claimRowMemory)
}

func newWorkLoop[V any](q *Queue[V], handle func(context.Context, Job[V]) error, settings workSettings,
	hold int,
) *workLoop[V] {
	return &workLoop[V]{
		q: q, handle: handle, settings: settings, hold: hold,
		hand: make(chan handed[V], hold), finished: make(chan settlement, hold), holding: map[*lease]struct{}{},
	}
}

func (l *workLoop[V]) run(ctx context.Context) error {
	handlers, stopHandlers := context.WithCancel(ctx)
	defer stopHandlers()
	var workers sync.WaitGroup
	for range l.settings.workers {
		workers.Go(func() {
			for claimed := range l.hand {
				l.finished <- l.runOne(handlers, claimed)
			}
		})
	}

	err := l.loop(ctx)

	stopHandlers()
	l.giveBackUnstarted()
	close(l.hand)
	workers.Wait()
	l.gather()
	final, cancel := context.WithTimeout(context.WithoutCancel(ctx), lastWrite)
	defer cancel()
	if _, writeErr := l.write(final, l.q.store.clock(), 0); writeErr != nil {
		err = errors.Join(err, writeErr)
	}
	return err
}

// loop claims and settles until ctx ends, the store closes, or, with
// UntilIdle, nothing is due and nothing is held
func (l *workLoop[V]) loop(ctx context.Context) error {
	for {
		l.gather()
		if err := l.stopped(ctx); err != nil {
			return err
		}
		now := l.q.store.clock()
		l.extendDue(now)
		want := 0
		var read *alarmRead
		if free := l.hold - len(l.holding) + l.finishing(); free > 0 {
			if read = l.q.state.alarm.rung(now); read != nil {
				want = min(free, claimBatch)
			}
		}
		result, err := l.write(ctx, now, want)
		l.answer(read, result, err)
		if err != nil {
			return err
		}
		l.dispatch(result)
		if l.settings.untilIdle && l.idle(now) {
			return nil
		}
		if want > 0 && result.full {
			continue
		}
		l.wait(ctx, now)
	}
}

// answer ends the alarm read a claim made: a claim that found fewer jobs than
// it wanted knows when the queue is next due, a full or failed one does not
func (l *workLoop[V]) answer(read *alarmRead, result claimResult, err error) {
	switch {
	case read == nil:
	case err != nil || result.full:
		l.q.state.alarm.forget(read)
	default:
		l.q.state.alarm.set(read, result.next)
	}
}

// finishing is how many held jobs the next write settles for good: their
// workers are free for the jobs that write claims
func (l *workLoop[V]) finishing() int {
	finished := 0
	for _, s := range l.pending {
		if s.how != extended {
			finished++
		}
	}
	return finished
}

// idle says the loop holds nothing, has nothing to write, and no job is due.
//
// A loop whose workers were all busy has not asked the file, and its alarm,
// which a claim that came back short set, still says a job may be due.
func (l *workLoop[V]) idle(now int64) bool {
	return len(l.holding) == 0 && len(l.pending) == 0 && !l.q.state.alarm.due(now)
}

func (l *workLoop[V]) stopped(ctx context.Context) error {
	select {
	case <-l.q.store.closing:
		return errClosed
	default:
		return ctx.Err()
	}
}

// runOne reads a job's value once the store's memory holds room for it, runs
// the handler on it and says how to settle it. The room is given back when the
// handler returns.
//
// What the handler returned decides, not whether the loop has stopped since:
// the loop may stop between the handler's return and this check, and a handler
// that finished its work, or failed on its own, must not be taken for one the
// loop stopped. Only a handler that returns the cancel of a stopping loop gives
// its job back uncounted.
func (l *workLoop[V]) runOne(handlers context.Context, claimed handed[V]) settlement {
	claimed.memory.releaseRow()
	job := claimed.job
	switch {
	case job.lease.wasCancelled():
		return settlement{lease: job.lease}
	case handlers.Err() != nil:
		return settlement{lease: job.lease, how: givenBack}
	}
	reserved, err := l.q.store.reserve(handlers, claimed.row.size)
	if err != nil {
		return unstarted(handlers, job, err)
	}
	defer reserved.Release()
	if job.Value, err = l.q.valueOf(handlers, claimed.row); err != nil {
		return unstarted(handlers, job, err)
	}

	cancellable, stop := context.WithCancelCause(handlers)
	defer stop(nil)
	ctx, cancel := context.WithTimeout(cancellable, l.settings.timeout)
	defer cancel()
	if !l.q.state.watch.started(job.lease, stop) {
		return settlement{lease: job.lease} // Cancel took it before it started
	}
	err = l.call(ctx, job)
	switch {
	case job.lease.isSettled():
		return settlement{lease: job.lease}
	case err == nil:
		return settlement{lease: job.lease, how: acked}
	case handlers.Err() != nil && errors.Is(err, context.Canceled):
		return settlement{lease: job.lease, how: givenBack}
	}
	return settlement{lease: job.lease, how: retried, cause: err}
}

// unstarted settles a job whose handler never ran. A value that no longer reads
// fails it for good, a stopping loop gives it back, and anything else costs the
// attempt.
func unstarted[V any](handlers context.Context, job Job[V], err error) settlement {
	switch {
	case errors.Is(err, errUnreadable):
		return settlement{lease: job.lease, how: failedForGood, cause: err}
	case handlers.Err() != nil:
		return settlement{lease: job.lease, how: givenBack}
	}
	return settlement{lease: job.lease, how: retried, cause: err}
}

// call runs the handler, a panic becoming an error with its stack
func (l *workLoop[V]) call(ctx context.Context, job Job[V]) (err error) {
	defer func() {
		if recovered := recover(); recovered != nil {
			err = fmt.Errorf("the handler panicked: %v\n%s", recovered, debug.Stack())
			l.q.state.panics.observe(time.Now(), fmt.Sprint(recovered))
		}
	}()
	return l.handle(ctx, job)
}

// gather takes what the workers finished without waiting for more
func (l *workLoop[V]) gather() {
	for {
		select {
		case s := <-l.finished:
			l.took(s)
		default:
			return
		}
	}
}

// took is one handler's return: nothing to write when it settled its job itself
func (l *workLoop[V]) took(s settlement) {
	if s.how == 0 {
		delete(l.holding, s.lease)
		return
	}
	l.pending = append(l.pending, s)
}

// extendDue extends the leases held past half their length, so that a long
// handler keeps its job and a vanished process loses it within a lease
func (l *workLoop[V]) extendDue(now int64) {
	lease := l.q.state.policy.lease
	for held := range l.holding {
		held.mu.Lock()
		until, settled := held.until, held.settled
		held.mu.Unlock()
		if !settled && until-now <= lease.Milliseconds()/2 && !l.settling(held) {
			l.pending = append(l.pending, settlement{lease: held, how: extended, timing: settleSettings{after: lease}})
		}
	}
}

// settling says a settlement of the lease is already waiting to be written
func (l *workLoop[V]) settling(held *lease) bool {
	for _, s := range l.pending {
		if s.lease == held {
			return true
		}
	}
	return false
}

// claimResult is what a Work write claimed: the jobs, whether it took as
// many as it wanted, and otherwise when the queue next needs a claim
type claimResult struct {
	claimed []claimedRow
	full    bool
	next    int64
	memory  *claimMemory
}

// write settles what is pending and claims up to want jobs in one grouped
// write. When fewer than want were due it also reads when the queue next needs
// a claim. A settlement whose lease another claim has taken is dropped.
func (l *workLoop[V]) write(ctx context.Context, now int64, want int) (claimResult, error) {
	if len(l.pending) == 0 && want == 0 {
		return claimResult{}, nil
	}
	if capacity := l.q.store.runtime.Memory().Capacity; capacity > 0 && want > 0 {
		if capacity < claimRowMemory {
			return claimResult{}, fmt.Errorf("%w: jobs: a claim needs %d bytes of store memory", tinystore.ErrLimit,
				claimRowMemory)
		}
		if rows := capacity / claimRowMemory; rows < int64(want) {
			want = int(rows)
		}
	}
	c := claiming{
		queue: l.q.state.id, now: now, until: now + l.q.state.policy.lease.Milliseconds(), limit: want,
		maxAttempts: l.q.state.policy.maxAttempts, maxRunning: l.q.state.policy.maxRunning,
	}
	var result claimResult
	var done []settled
	var abandoned []int64
	var reserved *tinystore.Reservation
	if want > 0 {
		var reserveErr error
		reserved, reserveErr = l.q.store.reserve(ctx, want*claimRowMemory)
		if reserveErr != nil {
			return claimResult{}, reserveErr
		}
	}
	err := l.q.store.file.UpdateGrouped(ctx, 0, func(w sqlite.Writer) error {
		var err error
		result, abandoned = claimResult{}, nil
		if done, err = settleAll(ctx, w, l.pending, now); err != nil || want == 0 {
			return err
		}
		if result.claimed, abandoned, err = claimRows(ctx, w, c); err != nil {
			return err
		}
		if result.full = len(result.claimed)+len(abandoned) == want; !result.full {
			result.next, err = nextClaim(ctx, w, c)
		}
		return err
	})
	if err != nil {
		if reserved != nil {
			reserved.Release()
		}
		return claimResult{}, err
	}
	if reserved != nil {
		reserved.Shrink(int64(len(result.claimed)) * claimRowMemory)
		if len(result.claimed) > 0 {
			result.memory = &claimMemory{reserved: reserved}
			result.memory.left.Store(int64(len(result.claimed)))
		}
	}
	l.settled(done)
	l.q.state.abandoned(abandoned)
	return result, nil
}

// settleAll writes each settlement in turn; one whose lease another claim has
// taken since writes nothing and does not stop the others
func settleAll(ctx context.Context, w sqlite.Writer, pending []settlement, now int64) ([]settled, error) {
	done := make([]settled, len(pending))
	for i, s := range pending {
		result, err := s.write(ctx, w, now)
		switch {
		case errors.Is(err, tinystore.ErrConflict):
			done[i] = settled{lost: true}
		case err != nil:
			return nil, err
		default:
			done[i] = result
		}
	}
	return done, nil
}

// settled follows in memory what the written settlements changed.
//
// A lease lost while its handler runs is let go and no longer extended, but it
// still holds its worker until the handler returns. One lost to a Cancel is no
// news to log.
func (l *workLoop[V]) settled(done []settled) {
	room := false
	for i, s := range l.pending {
		done[i].lost = done[i].lost && !s.lease.wasCancelled()
		done[i].apply(l.q.state)
		done[i].applyLease(s.lease)
		switch {
		case s.how != extended:
			s.lease.markSettled()
			delete(l.holding, s.lease)
			l.q.state.watch.settled(s.lease, done[i].ended, done[i].failed)
			room = true
		case done[i].lost:
			s.lease.markLost()
		}
	}
	if room {
		l.q.state.roomMade(l.q.store.clock())
	}
	l.pending = l.pending[:0]
}

// dispatch hands claimed jobs to the workers, which read their values
func (l *workLoop[V]) dispatch(result claimResult) {
	for _, row := range result.claimed {
		job := l.q.jobOf(row)
		l.holding[job.lease] = struct{}{}
		l.q.state.watch.holding(job.lease)
		l.hand <- handed[V]{job: job, row: row, memory: result.memory}
	}
}

// wait sleeps until a handler returns, the alarm rings or is lowered, a held
// lease needs extending, or the loop must stop. With every worker busy the
// alarm does not wake it.
func (l *workLoop[V]) wait(ctx context.Context, now int64) {
	lowered, sleep := l.q.state.alarm.wait(now)
	if len(l.holding) >= l.hold {
		lowered, sleep = nil, longestAlarm
	}
	if extendIn, any := l.nextExtension(now); any {
		sleep = min(sleep, extendIn)
	}
	timer := time.NewTimer(sleep)
	defer timer.Stop()
	select {
	case s := <-l.finished:
		l.took(s)
	case <-lowered:
	case <-timer.C:
	case <-ctx.Done():
	case <-l.q.store.closing:
	}
}

// nextExtension is how long until the first held lease is half spent
func (l *workLoop[V]) nextExtension(now int64) (time.Duration, bool) {
	half := l.q.state.policy.lease.Milliseconds() / 2
	first, any := int64(0), false
	for held := range l.holding {
		held.mu.Lock()
		due, settled := held.until-half, held.settled
		held.mu.Unlock()
		if !settled && (!any || due < first) {
			first, any = due, true
		}
	}
	return time.Duration(max(first-now, 0)) * time.Millisecond, any
}

// giveBackUnstarted takes the jobs no worker started, which go back without
// having run
func (l *workLoop[V]) giveBackUnstarted() {
	for {
		select {
		case claimed := <-l.hand:
			claimed.memory.releaseRow()
			l.pending = append(l.pending, settlement{lease: claimed.job.lease, how: givenBack})
		default:
			return
		}
	}
}
