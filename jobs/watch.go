package jobs

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"iter"
	"sync"
	"time"
)

// ErrCancelled is the cause a handler's context ends with when Cancel takes
// its job while it runs; what the handler returns then settles nothing.
var ErrCancelled = errors.New("jobs: the job was cancelled while it ran")

const (
	// maxProgress bounds what a handler reports of its job, as JSON.
	maxProgress = 4 << 10
	// maxAhead is as far as Ahead counts: a job further back reads it.
	maxAhead = 10_000
	// watchEvery is how often at most a watcher reads its job again, so that
	// one that falls behind a busy queue reads the latest and not each change.
	watchEvery = 200 * time.Millisecond
)

// Progress keeps v, as JSON, as what the handler reports of this attempt:
// Get and Watch show it until the attempt is settled. It lives in the store's
// memory and is not written to the file, since an attempt a restart ends runs
// again from its start. A value JSON cannot write, or one past 4 KiB, is
// dropped and logged once a quiet period.
func (j Job[V]) Progress(v any) {
	if j.lease == nil {
		return
	}
	encoded, err := json.Marshal(v)
	if err == nil && len(encoded) > maxProgress {
		err = fmt.Errorf("%d bytes of JSON, past %d", len(encoded), maxProgress)
	}
	if err != nil {
		j.lease.queue.progress.observe(time.Now(), err.Error())
		return
	}
	j.lease.queue.watch.report(j.lease, encoded)
}

// watching is what a queue's watchers follow besides the file: the jobs this
// process holds, which a handler or a Claim's caller has or a Work loop keeps
// for a busy worker, what each running one last reported, and how the jobs
// under a watched key left the queue. Every change closes a channel, so that
// each watcher reads its job again; with no watcher it costs a channel.
type watching struct {
	mu      sync.Mutex
	changed chan struct{}
	held    map[int64]*heldJob // by job id
	keys    map[string]int     // watchers by the key they watch
	ended   map[int64]endedJob // by job id, the jobs under a watched key that left the queue
}

// heldJob is a job a lease of this process holds
type heldJob struct {
	lease    *lease
	started  bool // a handler, or a Claim's caller, has it
	progress json.RawMessage
	stop     context.CancelCauseFunc // its handler's context; nil for a Claim's caller
}

// endedJob is how a job left its queue: Done, Failed or Cancelled
type endedJob struct {
	key     string
	state   State
	failure string
}

func newWatching() *watching {
	return &watching{
		changed: make(chan struct{}), held: map[int64]*heldJob{}, keys: map[string]int{},
		ended: map[int64]endedJob{},
	}
}

// changes is the channel the next change closes
func (w *watching) changes() <-chan struct{} {
	w.mu.Lock()
	defer w.mu.Unlock()
	return w.changed
}

// change wakes every watcher
func (w *watching) change() {
	w.mu.Lock()
	defer w.mu.Unlock()
	w.wake()
}

func (w *watching) wake() {
	close(w.changed)
	w.changed = make(chan struct{})
}

// holding says a Work loop keeps the job its lease holds for a busy worker
func (w *watching) holding(l *lease) {
	w.mu.Lock()
	defer w.mu.Unlock()
	w.held[l.id] = &heldJob{lease: l}
}

// started says a handler, or a Claim's caller, has the job its lease holds;
// stop ends the handler's context should Cancel take the job. False says
// Cancel took it first, and nothing may start it.
//
// Both hold the lock, so that a Cancel either sees the job started and stops
// its handler, or stops it from starting.
func (w *watching) started(l *lease, stop context.CancelCauseFunc) bool {
	w.mu.Lock()
	defer w.mu.Unlock()
	if l.wasCancelled() {
		return false
	}
	w.held[l.id] = &heldJob{lease: l, started: true, stop: stop}
	l.begin(l.store.clock())
	w.wake()
	return true
}

// settled forgets a lease that was settled, and keeps how its job left the
// queue, when it left, for the watchers of its key
func (w *watching) settled(l *lease, ended State, failure string) {
	w.mu.Lock()
	defer w.mu.Unlock()
	if h := w.held[l.id]; h != nil && h.lease == l {
		delete(w.held, l.id)
	}
	if ended != 0 && w.keys[l.key] > 0 {
		w.ended[l.id] = endedJob{key: l.key, state: ended, failure: failure}
	}
	w.wake()
}

// cancelled marks the lease of a job Cancel took, which then settles nothing,
// ends its handler's context when one runs it, and keeps the job cancelled
// for the watchers of its key
func (w *watching) cancelled(id int64, key string) {
	w.mu.Lock()
	h := w.held[id]
	delete(w.held, id)
	if h != nil {
		h.lease.markCancelled()
	}
	if w.keys[key] > 0 {
		w.ended[id] = endedJob{key: key, state: Cancelled}
	}
	w.wake()
	w.mu.Unlock()
	if h != nil && h.stop != nil {
		h.stop(ErrCancelled)
	}
}

// report keeps what a lease's handler said of its job; a lease no longer its
// job's says nothing
func (w *watching) report(l *lease, progress json.RawMessage) {
	w.mu.Lock()
	defer w.mu.Unlock()
	if h := w.held[l.id]; h != nil && h.lease == l && h.started {
		h.progress = progress
		w.wake()
	}
}

// view is what memory knows of a job whose attempt the file says a lease
// holds: whether a handler or a Claim's caller has it, rather than a Work
// loop keeping it for a busy worker, what was reported of it, and when its
// lease ends.
func (w *watching) view(id, attempt int64) (runs bool, progress json.RawMessage, until int64) {
	w.mu.Lock()
	h := w.held[id]
	w.mu.Unlock()
	if h == nil || !h.started || h.lease.attempt != attempt {
		return false, nil, 0
	}
	h.lease.mu.Lock()
	until = h.lease.until
	h.lease.mu.Unlock()
	return true, h.progress, until
}

// runningBefore counts the running jobs whose rows lie before a place in the
// order the queue runs them: the rows before a job, less these, wait before it.
// A lease that has ended runs nothing, though its holder never settled it: its
// job waits for the next claim.
func (w *watching) runningBefore(next, id, now int64) int {
	w.mu.Lock()
	defer w.mu.Unlock()
	count := 0
	for _, h := range w.held {
		if !h.started || h.lease.next > next || h.lease.next == next && h.lease.id >= id {
			continue
		}
		h.lease.mu.Lock()
		live := h.lease.until > now
		h.lease.mu.Unlock()
		if live {
			count++
		}
	}
	return count
}

// forget lets go of jobs a claim failed for good, since the leases this
// process held of them ended unsettled
func (w *watching) forget(ids []int64) {
	w.mu.Lock()
	defer w.mu.Unlock()
	for _, id := range ids {
		delete(w.held, id)
	}
	w.wake()
}

// watch counts a watcher of key until the function it returns is called; how
// the jobs under the key leave the queue meanwhile is kept for it
func (w *watching) watch(key string) (unwatch func()) {
	w.mu.Lock()
	defer w.mu.Unlock()
	w.keys[key]++
	return func() {
		w.mu.Lock()
		defer w.mu.Unlock()
		if w.keys[key]--; w.keys[key] > 0 {
			return
		}
		delete(w.keys, key)
		for id, ended := range w.ended {
			if ended.key == key {
				delete(w.ended, id)
			}
		}
	}
}

func (w *watching) endOf(id int64) (endedJob, bool) {
	w.mu.Lock()
	defer w.mu.Unlock()
	ended, ok := w.ended[id]
	return ended, ok
}

// Watch yields the job under key as it is, then again each time its state,
// place, attempt, time, progress or error changes, until it ends: done,
// failed or cancelled, the last entry it yields. A key that names no job
// yields nothing. It reads the job at most five times a second, so that a
// watcher that falls behind sees the latest, and it ends when ctx ends or the
// store closes.
func (q *Queue[V]) Watch(ctx context.Context, key string) iter.Seq2[Entry[V], error] {
	return func(yield func(Entry[V], error) bool) {
		defer q.state.watch.watch(key)()
		var followed int64 // the job's id once read: a later job under the key is another
		var last Entry[V]
		var read time.Time
		for {
			changed := q.state.watch.changes()
			if ended, ok := q.state.watch.endOf(followed); ok {
				yield(endedEntry(last, ended), nil)
				return
			}
			read = time.Now()
			s, err := q.sight(ctx, key)
			switch {
			case err != nil:
				yield(Entry[V]{}, q.fail(key, err))
				return
			case !s.found && followed == 0:
				return
			case s.found && followed == 0:
				followed = s.id
			}
			if s.found && s.id == followed && (followed != 0 && last.State == 0 || !sameView(s.entry, last)) {
				if !yield(s.entry, nil) || s.entry.State.ended() {
					return
				}
				last = s.entry
			}
			// a job gone from the file and not yet kept as ended is kept by the change that follows
			if !q.untilChanged(ctx, changed, s.until, read) {
				return
			}
		}
	}
}

// untilChanged sleeps until the queue changes or a running job's lease ends,
// then until a fifth of a second has passed since the last read; false says
// ctx ended or the store closed first
func (q *Queue[V]) untilChanged(ctx context.Context, changed <-chan struct{}, until int64, read time.Time) bool {
	var leaseEnd <-chan time.Time
	if until != 0 {
		timer := time.NewTimer(time.Duration(max(until-q.store.clock(), 0)+1) * time.Millisecond)
		defer timer.Stop()
		leaseEnd = timer.C
	}
	select {
	case <-changed:
	case <-leaseEnd:
	case <-ctx.Done():
		return false
	case <-q.store.closing:
		return false
	}
	pause := time.NewTimer(time.Until(read.Add(watchEvery)))
	defer pause.Stop()
	select {
	case <-pause.C:
		return true
	case <-ctx.Done():
		return false
	case <-q.store.closing:
		return false
	}
}

// endedEntry is the last entry a watcher saw of a job, as the job left the queue
func endedEntry[V any](last Entry[V], ended endedJob) Entry[V] {
	last.State, last.Ahead, last.Progress = ended.state, 0, nil
	if ended.failure != "" {
		last.Err = ended.failure
	}
	return last
}

// sameView says two entries of one job show a watcher the same: its value
// changes with its time or not at all for a watcher's purposes
func sameView[V any](a, b Entry[V]) bool {
	return a.State == b.State && a.Ahead == b.Ahead && a.Attempt == b.Attempt && a.At.Equal(b.At) &&
		a.Err == b.Err && a.Repeat == b.Repeat && string(a.Progress) == string(b.Progress)
}
