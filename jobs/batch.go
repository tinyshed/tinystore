package jobs

import (
	"context"
	"fmt"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/internal/sqlite"
)

// Change is a job a batch of the database the queues live in writes with its
// rows, which Enqueued gives out: see Options.In and sqldb's Batch.Add. A
// program does not call its methods.
type Change interface {
	Bytes() (int, error)
	Apply(ctx context.Context, file *sqlite.File, w sqlite.Writer) error
	Done(err error)
}

// Enqueued is Enqueue as a change of a batch of the database the queues live
// in, so that the job commits with the batch's rows or not at all:
//
//	err := db.Batch(ctx, func(b *sqldb.Batch) error {
//		b.Exec(`update notes set body = ? where id = ?`, body, id)
//		b.Add(index.Enqueued(ctx, IndexJob{Note: id}))
//		return nil
//	})
//
// The queue's store must be opened In that database. The change takes free
// memory for its value and holds it until the batch ends. Memory that is not
// free is ErrLimit when the batch weighs the change.
func (q *Queue[V]) Enqueued(ctx context.Context, value V, options ...EnqueueOption) Change {
	c := &enqueuedJob{store: q.store, state: q.state}
	var err error
	c.e, c.leave, err = prepareEnqueued(ctx, q, value, options)
	if err != nil {
		c.err = q.fail(c.e.key.String, err)
	}
	return c
}

// prepareEnqueued does what Enqueue does before it writes: the job's options
// and id, the queue's turn and the value's memory, then the value encoded
func prepareEnqueued[V any](ctx context.Context, q *Queue[V], value V, options []EnqueueOption) (
	e enqueued, leave func(), err error,
) {
	if q.tx != nil {
		return e, nil, fmt.Errorf("%w: jobs: Enqueued on a queue inside a Tx", tinystore.ErrInvalid)
	}
	if e, err = q.prepare(options); err != nil {
		return e, nil, err
	}
	if e.id, err = q.newID(ctx); err != nil {
		return e, nil, err
	}
	weight := q.codec.weigh(value)
	if weight > maxValue {
		return e, nil, tooLarge(weight)
	}
	reserved, leave, err := q.enterChange(ctx, weight)
	if err != nil {
		return e, nil, err
	}
	encoded, err := q.codec.encode(value)
	if err != nil {
		leave()
		return e, nil, err
	}
	e.value = keep(encoded)
	reserved.Shrink(int64(e.value.size()))
	return e, leave, nil
}

func (q *Queue[V]) enterChange(ctx context.Context, bytes int) (*tinystore.Reservation, func(), error) {
	release, err := q.store.admit(ctx)
	if err != nil {
		return nil, nil, err
	}
	reserved, err := q.store.reserveNow(bytes)
	if err != nil {
		release()
		return nil, nil, err
	}
	return reserved, func() {
		reserved.Release()
		release()
	}, nil
}

// enqueuedJob is one Enqueued: the job it writes, and the turn and memory it
// holds until its batch ends
type enqueuedJob struct {
	store *Store
	state *queueState
	e     enqueued
	added bool
	leave func() // nil once let go
	err   error
}

func (c *enqueuedJob) Bytes() (int, error) {
	if c.err != nil {
		return 0, c.err
	}
	return c.e.value.size(), nil
}

// Apply writes the job with the batch's writer, in a file that must be the
// one the queue's store keeps its queues in
func (c *enqueuedJob) Apply(ctx context.Context, file *sqlite.File, w sqlite.Writer) error {
	if c.err != nil {
		return c.err
	}
	if file != c.store.file {
		return nameFailure(c.state.name, c.e.key.String, fmt.Errorf(
			"%w: jobs: the batch's database is not the one the queue lives in; open its store with Options.In",
			tinystore.ErrInvalid))
	}
	var err error
	c.added, err = enqueue(ctx, w, c.e)
	return nameFailure(c.state.name, c.e.key.String, err)
}

// Done lets go of the queue's turn and the memory, and once the job is durable
// counts it and wakes a Work waiting for it
func (c *enqueuedJob) Done(err error) {
	if err == nil && c.err == nil {
		if c.added {
			c.state.waiting.Add(1)
		}
		c.state.alarm.lower(c.e.at)
		c.state.watch.change()
	}
	if c.leave != nil {
		c.leave()
		c.leave = nil
	}
}
