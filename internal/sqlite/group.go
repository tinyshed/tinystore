package sqlite

import (
	"context"
	"errors"
	"fmt"
	"slices"
	"sync"
	"time"
)

// ErrOutcomeUnknown is a grouped write whose transaction failed to commit: it
// may or may not be in the file, and its caller reconciles before retrying.
var ErrOutcomeUnknown = errors.New("commit outcome unknown")

// what one group commits at most, and how long it may hold the writer
const (
	groupWrites = 1024
	groupBytes  = 8 << 20
	groupHold   = 10 * time.Second
)

const (
	savepointQuery  = `savepoint grouped`
	releaseQuery    = `release grouped`
	rollbackToQuery = `rollback to grouped`
)

// group is the writes waiting for the file's writer. The first caller to find
// no leader commits every write queued behind it in one transaction, each in a
// savepoint, then hands the lead to the first caller still waiting, so that a
// commit's fsync is shared by the writes that arrived while the last one ran,
// and no goroutine is started. See docs/group-commit-contract.md.
type group struct {
	mu      sync.Mutex
	queue   []*groupedWrite
	leading bool
}

// groupedWrite is one caller's write from its queueing to its answer: err is
// final once done is closed, and lead closes when its caller leads
type groupedWrite struct {
	ctx   context.Context
	bytes int
	write func(Writer) error
	state writeState
	lead  chan struct{}
	done  chan struct{}
	err   error
}

type writeState int

const (
	queued  writeState = iota
	leading            // at the head of the queue, its caller leading
	taken              // in a batch the leader commits
	answered
)

// UpdateGrouped runs write in a savepoint of a transaction it may share with
// the writes queued beside it, and returns once that transaction committed.
// A write that fails, and whose savepoint rolls back, fails alone; a caller
// whose context ends before its write starts writes nothing; a write that has
// started finishes with its group, whatever its caller's context does. bytes
// weigh the write against the group's bound, and a heavier one commits alone.
// A commit that fails is ErrOutcomeUnknown.
func (f *File) UpdateGrouped(ctx context.Context, bytes int, write func(Writer) error) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	entry := &groupedWrite{
		ctx: ctx, bytes: bytes, write: write,
		lead: make(chan struct{}), done: make(chan struct{}),
	}
	if f.writes.enqueue(entry) {
		return f.lead(entry)
	}
	return f.follow(entry)
}

// enqueue puts a write behind the others and says whether its caller leads
func (g *group) enqueue(entry *groupedWrite) (lead bool) {
	g.mu.Lock()
	defer g.mu.Unlock()
	g.queue = append(g.queue, entry)
	if g.leading {
		return false
	}
	g.leading = true
	entry.state = leading
	return true
}

// follow waits for a write's answer or for the lead; a caller whose context
// ends while its write is still queued leaves without writing
func (f *File) follow(entry *groupedWrite) error {
	select {
	case <-entry.done:
		return entry.err
	case <-entry.lead:
		return f.lead(entry)
	case <-entry.ctx.Done():
	}
	if f.writes.leave(entry) {
		return entry.ctx.Err()
	}
	select {
	case <-entry.done:
		return entry.err
	case <-entry.lead:
		return f.lead(entry)
	}
}

// leave takes a write that has not started out of the queue
func (g *group) leave(entry *groupedWrite) bool {
	g.mu.Lock()
	defer g.mu.Unlock()
	if entry.state != queued {
		return false
	}
	g.queue = slices.DeleteFunc(g.queue, func(queued *groupedWrite) bool { return queued == entry })
	return true
}

// lead commits the batch at the head of the queue, where the leader's own
// write is, answers every write in it and hands the lead on
func (f *File) lead(own *groupedWrite) error {
	batch, outcomes := f.commitGroup(own.ctx)
	f.writes.answer(batch, outcomes)
	f.writes.handOn()
	<-own.done
	return own.err
}

// take removes the batch from the head of the queue, at most groupWrites and
// groupBytes but one write at least, and answers the writes whose callers left
// before they started
func (g *group) take() []*groupedWrite {
	g.mu.Lock()
	defer g.mu.Unlock()
	var batch []*groupedWrite
	bytes, next := 0, 0
	for ; next < len(g.queue) && len(batch) < groupWrites; next++ {
		entry := g.queue[next]
		if len(batch) > 0 && bytes+entry.bytes > groupBytes {
			break
		}
		if err := entry.ctx.Err(); err != nil {
			entry.settle(err)
			continue
		}
		entry.state = taken
		batch = append(batch, entry)
		bytes += entry.bytes
	}
	g.queue = g.queue[next:]
	return batch
}

// handOn gives the lead to the first caller still waiting, or frees it
func (g *group) handOn() {
	g.mu.Lock()
	defer g.mu.Unlock()
	if len(g.queue) == 0 {
		g.leading = false
		g.queue = nil
		return
	}
	next := g.queue[0]
	next.state = leading
	close(next.lead)
}

// commitGroup takes the writer first and the batch then, so that the writes
// queued while it waited join, and runs each in its savepoint of one
// transaction, bounded by groupHold rather than by any caller
func (f *File) commitGroup(ctx context.Context) ([]*groupedWrite, []error) {
	ctx, cancel := context.WithTimeout(context.WithoutCancel(ctx), groupHold)
	defer cancel()

	var batch []*groupedWrite
	var failed []error
	started, ran := false, false
	err := f.UpdatePrepared(ctx, func(w Writer) error {
		batch, started = f.writes.take(), true
		failed = make([]error, len(batch))
		for i, entry := range batch {
			var err error
			if failed[i], err = runSavepoint(ctx, w, entry.write); err != nil {
				return err
			}
		}
		ran = true
		return nil
	})
	if !started {
		batch = f.writes.take()
		failed = make([]error, len(batch))
	}
	outcomes := make([]error, len(batch))
	for i := range batch {
		outcomes[i] = outcome(failed[i], err, ran)
	}
	return batch, outcomes
}

// runSavepoint runs one write in a savepoint: a write that fails is rolled back
// to it and fails alone, and err says that the rollback failed as well
func runSavepoint(ctx context.Context, w Writer, write func(Writer) error) (failed, err error) {
	if _, err = w.ExecContext(ctx, savepointQuery); err != nil {
		return nil, err
	}
	if failed = write(w); failed == nil {
		_, err = w.ExecContext(ctx, releaseQuery)
		return nil, err
	}
	if _, err = w.ExecContext(ctx, rollbackToQuery); err != nil {
		return failed, errors.Join(failed, err)
	}
	_, err = w.ExecContext(ctx, releaseQuery)
	return failed, err
}

// outcome is one write's answer: its own failure, which rolled it back whatever
// the group did; the group's, when the transaction ended before its commit;
// ErrOutcomeUnknown, when the commit itself failed; or none
func outcome(failed, group error, ran bool) error {
	switch {
	case failed != nil:
		return failed
	case group != nil && ran:
		return fmt.Errorf("%w: %w", ErrOutcomeUnknown, group)
	case group != nil:
		return fmt.Errorf("rolled back with its group: %w", group)
	}
	return nil
}

// answer gives each write of a batch its final error and wakes its caller
func (g *group) answer(batch []*groupedWrite, errs []error) {
	g.mu.Lock()
	defer g.mu.Unlock()
	for i, entry := range batch {
		entry.settle(errs[i])
	}
}

// settle gives a write its final error and wakes its caller; the group's lock
// is held, since leave reads the state
func (w *groupedWrite) settle(err error) {
	w.err = err
	w.state = answered
	close(w.done)
}
