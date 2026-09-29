package jobs

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"sync/atomic"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/internal/sqlite"
)

const (
	kindQueue    = "queue"
	kindSchedule = "schedule"
)

// queueState is one queue as this process knows it. Every handle on the queue
// shares it.
type queueState struct {
	// id is the queue's id in the file.
	id     int64
	name   string
	kind   string
	policy policy
	alarm  *alarm
	// waiting is how many jobs the queue holds.
	waiting  atomic.Int64
	failures *quietLog
	limits   *quietLog
	panics   *quietLog
	lost     *quietLog

	keysAfter string // where maintenance's walk over the keys left behind goes on
}

// Queue is a handle on the jobs of one type in one queue; Work and Claim hand
// its due jobs to whoever works them.
type Queue[V any] struct {
	store *Store
	state *queueState
	codec valueCodec[V]
	tx    *Tx
}

// OpenQueue opens the queue name of jobs.db, creating it on first use. Its
// name and kind are kept in the file; its options are the program's, and a
// second handle on it in one process takes the same ones or is ErrInvalid.
func OpenQueue[V any](ctx context.Context, store *Store, name string, options ...QueueOption) (*Queue[V], error) {
	p, err := collectPolicy(options)
	if err != nil {
		return nil, err
	}
	state, err := store.openQueue(ctx, name, kindQueue, p)
	if err != nil {
		return nil, err
	}
	return &Queue[V]{store: store, state: state, codec: newValueCodec[V]()}, nil
}

// OpenSchedule opens a queue whose one job, under the queue's name, repeats:
// the repeat in the program is the one kept, so a changed Daily takes effect
// on the next open. Work it as any queue; a job's At is the time it runs for.
func OpenSchedule(ctx context.Context, store *Store, name string, repeat Repeat, options ...QueueOption) (
	*Queue[struct{}], error,
) {
	if repeat.err != nil {
		return nil, repeat.err
	}
	p, err := collectPolicy(options)
	if err != nil {
		return nil, err
	}
	state, err := store.openQueue(ctx, name, kindSchedule, p)
	if err != nil {
		return nil, err
	}
	q := &Queue[struct{}]{store: store, state: state, codec: newValueCodec[struct{}]()}
	return q, q.keepRepeat(ctx, repeat)
}

const (
	registerQueue = `insert into queues (name, kind) values (?1, ?2) on conflict (name) do nothing`
	queueNamed    = `select id, kind from queues where name = ?1`
	countWaiting  = `select waiting from queues where id = ?1`
)

// openQueue finds or registers a queue. The first time this process opens it,
// it reads the count of the queue's jobs that the file keeps for MaxWaiting.
// Its alarm rings at once, so the first Work loop reads the file.
func (s *Store) openQueue(ctx context.Context, name, kind string, p policy) (*queueState, error) {
	if !validName.MatchString(name) {
		return nil, fmt.Errorf("%w: jobs: queue name %q", tinystore.ErrInvalid, name)
	}
	leave, err := s.admit(ctx)
	if err != nil {
		return nil, err
	}
	defer leave()

	s.opened.Lock()
	defer s.opened.Unlock()
	if state, found := s.queues[name]; found {
		if state.kind != kind || state.policy != p {
			return nil, fmt.Errorf("%w: jobs: queue %q is open as a %s with other options", tinystore.ErrInvalid,
				name, state.kind)
		}
		return state, nil
	}
	state, err := s.registerQueue(ctx, name, kind, p)
	if err != nil {
		return nil, err
	}
	s.queues[name] = state
	return state, nil
}

func (s *Store) registerQueue(ctx context.Context, name, kind string, p policy) (*queueState, error) {
	state := &queueState{
		name: name, kind: kind, policy: p, alarm: newAlarm(),
		failures: newQuietLog(s.log, "jobs failed for good", name),
		limits:   newQuietLog(s.log, "a queue past MaxWaiting refused jobs", name),
		panics:   newQuietLog(s.log, "a handler panicked", name),
		lost:     newQuietLog(s.log, "a Work loop lost the leases of jobs it ran", name),
	}
	var stored string
	var waiting int64
	err := s.file.UpdatePrepared(ctx, func(w sqlite.Writer) error {
		if _, err := w.ExecContext(ctx, registerQueue, name, kind); err != nil {
			return err
		}
		if err := sqlite.QueryRowByKey(ctx, w, queueNamed, name).Scan(&state.id, &stored); err != nil {
			return err
		}
		return sqlite.QueryRowByKey(ctx, w, countWaiting, state.id).Scan(&waiting)
	})
	switch {
	case err != nil:
		return nil, fmt.Errorf("jobs: open queue %q: %w", name, err)
	case stored != kind:
		return nil, fmt.Errorf("%w: jobs: %q is a %s, opened as a %s", tinystore.ErrInvalid, name, stored, kind)
	}
	state.waiting.Store(waiting)
	return state, nil
}

// WithTx is the queue inside tx: its calls join the transaction. It works in
// tx's callback only, and is ErrClosed after it.
func (q *Queue[V]) WithTx(tx *Tx) *Queue[V] {
	inside := *q
	inside.tx = tx
	return &inside
}

// write runs work in the file's writer: inside the handle's transaction, or
// grouped with the writes other goroutines are waiting to commit. Its bytes are
// held in the store's memory while they wait.
func (q *Queue[V]) write(ctx context.Context, bytes int, work func(sqlite.Writer) error) error {
	_, leave, err := q.enter(ctx, bytes)
	if err != nil {
		return err
	}
	defer leave()
	return q.commit(ctx, bytes, work)
}

// writeValue is write for a job's value, which it encodes into e once the write
// holds its turn and the memory the value may take.
//
// So a write waiting for its turn has made nothing the store has not counted.
func (q *Queue[V]) writeValue(ctx context.Context, value V, e *enqueued, work func(sqlite.Writer) error) error {
	weight := q.codec.weigh(value)
	if weight > maxValue {
		return tooLarge(weight)
	}
	reserved, leave, err := q.enter(ctx, weight)
	if err != nil {
		return err
	}
	defer leave()

	encoded, err := q.codec.encode(value)
	if err != nil {
		return err
	}
	e.value = keep(encoded)
	reserved.Shrink(int64(e.value.size()))
	return q.commit(ctx, e.value.size(), work)
}

// enter takes a write's turn and bytes of the store's memory before the write
// makes anything. Inside Tx the turn is the transaction's, and the memory only
// what is free.
func (q *Queue[V]) enter(ctx context.Context, bytes int) (*tinystore.Reservation, func(), error) {
	if q.tx != nil {
		if err := q.checkTx(); err != nil {
			return nil, nil, err
		}
		reserved, err := q.store.reserveNow(bytes)
		if err != nil {
			return nil, nil, err
		}
		return reserved, reserved.Release, nil
	}
	release, err := q.store.admitWrite(ctx)
	if err != nil {
		return nil, nil, err
	}
	reserved, err := q.store.reserve(ctx, bytes)
	if err != nil {
		release()
		return nil, nil, err
	}
	return reserved, func() {
		reserved.Release()
		release()
	}, nil
}

// commit runs a write that holds its turn: in the handle's transaction, or in
// a group that shares one commit with the writes beside it
func (q *Queue[V]) commit(ctx context.Context, bytes int, work func(sqlite.Writer) error) error {
	if q.tx != nil {
		if err := q.checkTx(); err != nil {
			return err
		}
		return q.tx.run(work)
	}
	return q.store.file.UpdateGrouped(ctx, bytes, work)
}

// committed runs what a write changes in memory once it is in the file: now,
// or when the handle's transaction commits
func (q *Queue[V]) committed(effect func()) {
	if q.tx != nil {
		q.tx.after = append(q.tx.after, effect)
		return
	}
	effect()
}

// newID is the next job id: the store's, or the transaction's own block
func (q *Queue[V]) newID(ctx context.Context) (int64, error) {
	if q.tx != nil {
		if err := q.checkTx(); err != nil {
			return 0, err
		}
		return q.tx.nextID(ctx)
	}
	return q.store.nextID(ctx)
}

func (q *Queue[V]) checkTx() error {
	if q.tx.store != q.store {
		return fmt.Errorf("%w: jobs: queue %q belongs to another store than its transaction",
			tinystore.ErrInvalid, q.state.name)
	}
	return nil
}

// fail names the queue and key a call failed on; a cancellation, a closed
// store and a JobError already made pass as they are
func (q *Queue[V]) fail(key string, err error) error {
	return nameFailure(q.state.name, key, err)
}

func nameFailure(queue, key string, err error) error {
	var named *JobError
	switch {
	case err == nil, errors.Is(err, context.Canceled), errors.Is(err, context.DeadlineExceeded),
		errors.Is(err, tinystore.ErrClosed), errors.As(err, &named):
		return err
	}
	return &JobError{Queue: queue, Key: key, Err: err}
}

// keepRepeat makes the schedule's one job repeat as the program says: added
// at its next time when it is not there, given the program's repeat when it is
func (q *Queue[V]) keepRepeat(ctx context.Context, repeat Repeat) error {
	var zero V
	encoded, err := q.codec.encode(zero)
	if err != nil {
		return err
	}
	name := q.state.name
	now := q.store.clock()
	next := repeat.next(q.store.now()).UnixMilli()
	id, err := q.newID(ctx)
	if err != nil {
		return err
	}
	err = q.write(ctx, len(encoded), func(w sqlite.Writer) error {
		return setSchedule(ctx, w, scheduled{
			queue: q.state.id, id: id, key: name, next: next, now: now,
			repeat: repeat.text, value: encoded, maxWaiting: q.state.policy.maxWaiting,
		})
	})
	if err == nil {
		q.state.alarm.lower(next)
		q.state.waiting.Store(max(q.state.waiting.Load(), 1))
	}
	return q.fail(name, err)
}

type scheduled struct {
	queue, id, next, now int64
	maxWaiting           int64
	key, repeat          string
	value                []byte
}

const (
	scheduleNamed = `select j.next, j.id, j.repeat
		from keys k join jobs j on j.queue = k.queue and j.next = k.next and j.id = k.id
		where k.queue = ?1 and k.key = ?2`
	moveSchedule = `update jobs set next = ?4, at = ?4, repeat = ?5 where queue = ?1 and next = ?2 and id = ?3`
	keepSchedule = `update jobs set repeat = ?4 where queue = ?1 and next = ?2 and id = ?3`
	addSchedule  = `insert into jobs (queue, next, id, key, at, attempt, repeat, value)
		values (?1, ?2, ?3, ?4, ?2, 0, ?5, ?6)`
)

// setSchedule adds the job, or gives the one there the program's repeat. A
// waiting one moves to the repeat's next time, and a leased one keeps its row
// where its lease names it.
func setSchedule(ctx context.Context, w sqlite.Writer, s scheduled) error {
	var there row
	err := sqlite.QueryRowByKey(ctx, w, scheduleNamed, s.queue, s.key).Scan(&there.next, &there.id, &there.repeat)
	if errors.Is(err, sql.ErrNoRows) {
		if err = checkRoom(ctx, w, enqueued{queue: s.queue, maxWaiting: s.maxWaiting}); err != nil {
			return err
		}
		if _, err = w.ExecContext(ctx, addSchedule, s.queue, s.next, s.id, s.key, s.repeat, s.value); err != nil {
			return err
		}
		return keepKey(ctx, w, s.queue, sql.NullString{String: s.key, Valid: true}, s.next, s.id)
	}
	if err != nil || there.repeat.String == s.repeat {
		return err
	}
	leased, err := leaseHeld(ctx, w, there.id, s.now)
	if err != nil {
		return err
	}
	if leased {
		_, err = w.ExecContext(ctx, keepSchedule, s.queue, there.next, there.id, s.repeat)
		return err
	}
	if _, err = w.ExecContext(ctx, moveSchedule, s.queue, there.next, there.id, s.next, s.repeat); err != nil {
		return err
	}
	return moveKeyTo(ctx, w, s.queue, s.key, s.next)
}
