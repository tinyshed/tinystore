package jobs

import (
	"context"
	"errors"
	"fmt"
	"runtime/debug"
	"sync"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/internal/sqlite"
)

// the longest the settlements of a stopping Work may take to write
const lastWrite = 10 * time.Second

// Work claims the queue's jobs as they fall due and runs handle on each, in
// up to Workers goroutines it starts and waits for before it returns, until
// ctx ends or the store closes, or, with UntilIdle, until nothing is due and
// nothing runs. A handler's nil acknowledges its job and an error retries it
// by the queue's policy; a handler that settles its job itself is left as it
// settled it, and a panic is an error. While a handler runs its lease is
// extended. A handler stopped by ctx or by Close gives its job back, the
// attempt not counted. A job claimed is settled in the same write that claims
// the next ones, so that a queue under load commits once for many jobs.
func (q *Queue[V]) Work(ctx context.Context, handle func(context.Context, Job[V]) error, options ...WorkOption) error {
	settings, err := collectWork(options)
	if err == nil && q.tx != nil {
		err = fmt.Errorf("%w: jobs: Work inside a transaction", tinystore.ErrInvalid)
	}
	if err != nil {
		return q.fail("", err)
	}
	leave, err := q.store.admit(ctx)
	if err != nil {
		return err
	}
	defer leave()

	loop := newWorkLoop(q, handle, settings)
	return loop.run(ctx)
}

// workLoop is one Work: the jobs it holds, which its workers run, and the
// settlements it has yet to write
type workLoop[V any] struct {
	q        *Queue[V]
	handle   func(context.Context, Job[V]) error
	settings workSettings
	hold     int                 // jobs the loop may hold: running, or claimed for a worker
	hand     chan Job[V]         // claimed jobs, for the workers
	finished chan settlement     // what each handler's return says
	holding  map[*lease]struct{} // the leases of the jobs it holds, which it extends
	pending  []settlement
}

func newWorkLoop[V any](q *Queue[V], handle func(context.Context, Job[V]) error, settings workSettings) *workLoop[V] {
	hold := settings.workers
	return &workLoop[V]{
		q: q, handle: handle, settings: settings, hold: hold,
		hand: make(chan Job[V], hold), finished: make(chan settlement, hold), holding: map[*lease]struct{}{},
	}
}

func (l *workLoop[V]) run(ctx context.Context) error {
	handlers, stopHandlers := context.WithCancel(ctx)
	defer stopHandlers()
	var workers sync.WaitGroup
	for range l.settings.workers {
		workers.Go(func() {
			for job := range l.hand {
				l.finished <- l.runOne(handlers, job)
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
		if free := l.hold - len(l.holding); free > 0 && l.q.state.alarm.rung(now) {
			want = min(free, claimBatch)
		}
		result, err := l.write(ctx, now, want)
		if err != nil {
			return err
		}
		l.dispatch(result.claimed, now)
		if want > 0 && !result.full {
			l.q.state.alarm.set(result.next)
		}
		if l.settings.untilIdle && len(l.holding) == 0 && len(l.pending) == 0 && (want == 0 || !result.full) {
			return nil
		}
		if want > 0 && result.full {
			continue
		}
		l.wait(ctx, now)
	}
}

func (l *workLoop[V]) stopped(ctx context.Context) error {
	select {
	case <-l.q.store.closing:
		return errClosed
	default:
		return ctx.Err()
	}
}

// runOne runs the handler on one job and says how to settle it
func (l *workLoop[V]) runOne(handlers context.Context, job Job[V]) settlement {
	if handlers.Err() != nil {
		return settlement{lease: job.lease, how: givenBack}
	}
	ctx, cancel := context.WithTimeout(handlers, l.settings.timeout)
	defer cancel()
	err := l.call(ctx, job)
	switch {
	case job.lease.isSettled():
		return settlement{lease: job.lease}
	case handlers.Err() != nil:
		return settlement{lease: job.lease, how: givenBack}
	case err == nil:
		return settlement{lease: job.lease, how: acked}
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
}

// write settles what is pending and claims up to want jobs in one grouped
// write, and when fewer than want were due reads when the queue next needs a
// claim; a settlement whose lease another claim has taken is dropped
func (l *workLoop[V]) write(ctx context.Context, now int64, want int) (claimResult, error) {
	if len(l.pending) == 0 && want == 0 {
		return claimResult{}, nil
	}
	c := claiming{
		queue: l.q.state.id, now: now, until: now + l.q.state.policy.lease.Milliseconds(), limit: want,
		maxAttempts: l.q.state.policy.maxAttempts,
	}
	var result claimResult
	var done []settled
	abandoned := 0
	err := l.q.store.file.UpdateGrouped(ctx, 0, func(w sqlite.Writer) error {
		var err error
		result, abandoned = claimResult{}, 0
		if done, err = settleAll(ctx, w, l.pending, now); err != nil || want == 0 {
			return err
		}
		if result.claimed, abandoned, err = claimRows(ctx, w, c); err != nil {
			return err
		}
		if result.full = len(result.claimed)+abandoned == want; !result.full {
			result.next, err = nextTime(ctx, w, c.queue, now)
		}
		return err
	})
	if err != nil {
		return claimResult{}, err
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
			done[i] = settled{}
		case err != nil:
			return nil, err
		default:
			done[i] = result
		}
	}
	return done, nil
}

// settled follows in memory what the written settlements changed
func (l *workLoop[V]) settled(done []settled) {
	for i, s := range l.pending {
		done[i].apply(l.q.state)
		if s.how != extended {
			s.lease.markSettled()
			delete(l.holding, s.lease)
		}
	}
	l.pending = l.pending[:0]
}

// dispatch hands claimed jobs to the workers; a value that no longer decodes
// fails its job for good in the next write
func (l *workLoop[V]) dispatch(claimed []claimedRow, now int64) {
	until := now + l.q.state.policy.lease.Milliseconds()
	for _, c := range claimed {
		job, err := l.q.jobOf(c, until)
		l.holding[job.lease] = struct{}{}
		if err != nil {
			l.pending = append(l.pending, settlement{lease: job.lease, how: failedForGood, cause: err})
			continue
		}
		l.hand <- job
	}
}

// wait sleeps until a handler returns, the alarm rings or is lowered, a held
// lease needs extending, or the loop must stop; with every worker busy the
// alarm does not wake it
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
		due := held.until - half
		held.mu.Unlock()
		if !any || due < first {
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
		case job := <-l.hand:
			l.pending = append(l.pending, settlement{lease: job.lease, how: givenBack})
		default:
			return
		}
	}
}
