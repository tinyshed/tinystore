package sqlite

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"slices"
	"sync"
	"time"
)

// ErrOutcomeUnknown is a grouped write whose transaction failed to commit: it
// may or may not be in the file, and its caller reconciles before retrying.
var ErrOutcomeUnknown = errors.New("commit outcome unknown")

// A group commits at most groupWrites writes and groupBytes bytes, and may hold
// the writer for groupHold once it holds it.
//
// writerPatience is how long a grouped write waits for the writer before its
// engine is told.
const (
	groupWrites    = 1024
	groupBytes     = 8 << 20
	groupHold      = 10 * time.Second
	writerPatience = 10 * time.Second
)

// A leader gathers for at most a quarter of the last commit and never more
// than gatherMost.
//
// The callers a commit answers need tens of microseconds to wake and write
// again, and a leader that took the queue at once left them all to the next
// commit: 64 writers made groups of about 32, half the writes a sync could
// carry, tinyshed/research tinystore/reports/compare-2026-09-30.md.
const (
	gatherShare = 4
	gatherMost  = 2 * time.Millisecond
)

const (
	savepointQuery  = `savepoint grouped`
	releaseQuery    = `release grouped`
	rollbackToQuery = `rollback to grouped`
)

// group is the writes waiting for the file's writer.
//
// The first caller to find no leader commits every write queued behind it in
// one transaction, each in a savepoint, then hands the lead to the first caller
// still waiting. A commit's fsync is therefore shared by the writes that
// arrived while the last one ran, and no goroutine is started.
//
// See research's design/group-commit-contract.md.
type group struct {
	mu      sync.Mutex
	queue   []*groupedWrite
	leading bool
	last    int           // writes the last batch answered, whose callers may be about to write again
	held    time.Duration // how long the last batch held the writer
	grew    chan struct{} // told of a write queued, for a leader gathering
}

// groupedWrite is one caller's write from its queueing to its answer: err is
// final once done is closed, and lead closes when its caller leads
type groupedWrite struct {
	ctx   context.Context
	label string
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
// writes queued beside it, and returns once that transaction commits.
//
// A failed write rolls back its savepoint and fails alone. If a caller's
// context ends before its write starts, that write never runs, whether the
// caller waits behind a leader or leads the wait for the writer. Once started,
// a write finishes with its group whatever its caller's context does.
//
// A statement run through UntilDeadline is different: it ends at its caller's
// deadline, and if the deadline passes while SQLite is executing it, SQLite
// rolls back the whole transaction and the group fails with it.
//
// A group holds the writer for at most Config.GroupHold, counted from when it
// acquires the writer. Waiting for the writer does not count, so a long
// transaction ahead fails none of the writes queued behind it. bytes counts
// toward the group's size bound, and a write heavier than the bound commits
// alone. A failed commit is ErrOutcomeUnknown.
func (f *File) UpdateGrouped(ctx context.Context, bytes int, write func(Writer) error) error {
	return f.UpdateGroupedAs(ctx, "", bytes, write)
}

// UpdateGroupedAs is UpdateGrouped with the label Config.Waited is told.
func (f *File) UpdateGroupedAs(ctx context.Context, label string, bytes int, write func(Writer) error) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	entry := &groupedWrite{
		ctx: ctx, label: label, bytes: bytes, write: write,
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
		select {
		case g.grew <- struct{}{}:
		default:
		}
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

// lead waits for the writer as its own caller, commits the batch at the head of
// the queue (where its own write is), answers every write in it and hands the
// lead on.
//
// A caller who leaves before the writer is free hands the lead on at once.
func (f *File) lead(own *groupedWrite) error {
	if err := f.waitForWriter(own); err != nil {
		f.writes.resign(own, err)
		return err
	}
	f.writes.gather()
	began := time.Now()
	batch, outcomes := f.commitGroup(own.ctx)
	f.writes.held = time.Since(began) // only the leader, which holds the writer, reads or writes it
	f.writes.answer(batch, outcomes)
	f.writes.handOn()
	<-own.done
	return own.err
}

// waitForWriter takes the writer's slot for the group while the leader's
// caller still waits, and tells the engine once when the wait passes patience
func (f *File) waitForWriter(own *groupedWrite) error {
	select {
	case f.writeSlots <- struct{}{}:
		return nil
	default:
	}
	patience := time.NewTimer(f.patience)
	defer patience.Stop()
	for {
		select {
		case f.writeSlots <- struct{}{}:
			return nil
		case <-own.ctx.Done():
			return own.ctx.Err()
		case <-patience.C:
			if f.waited != nil {
				f.waited(own.label)
			}
		}
	}
}

// resign takes a leader whose caller left before the writer was free out of
// the queue, answers it and hands the lead on
func (g *group) resign(own *groupedWrite, err error) {
	g.mu.Lock()
	defer g.mu.Unlock()
	g.queue = slices.DeleteFunc(g.queue, func(queued *groupedWrite) bool { return queued == own })
	own.settle(err)
	g.passLead()
}

// gather lets the writes the last batch answered join the next before it is
// taken: it waits until the queue holds as many as that batch did, or for a
// quarter of that batch's commit, whichever comes first. One writer alone
// finds itself queued and never waits.
func (g *group) gather() {
	g.mu.Lock()
	want, wait := g.last, min(g.held/gatherShare, gatherMost)
	if len(g.queue) >= want || wait <= 0 {
		g.mu.Unlock()
		return
	}
	if g.grew == nil {
		g.grew = make(chan struct{}, 1)
	}
	grew := g.grew
	g.mu.Unlock()

	timer := time.NewTimer(wait)
	defer timer.Stop()
	for {
		select {
		case <-grew:
		case <-timer.C:
			return
		}
		g.mu.Lock()
		gathered := len(g.queue) >= want
		g.mu.Unlock()
		if gathered {
			return
		}
	}
}

// take removes the batch from the head of the queue: at most groupWrites and
// groupBytes, but one write at least. It answers the writes whose callers left
// before they started.
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

func (g *group) handOn() {
	g.mu.Lock()
	defer g.mu.Unlock()
	g.passLead()
}

// passLead gives the lead to the first caller still waiting, or frees it; the
// group's lock is held
func (g *group) passLead() {
	if len(g.queue) == 0 {
		g.leading = false
		g.queue = nil
		return
	}
	next := g.queue[0]
	next.state = leading
	close(next.lead)
}

// commitGroup runs the batch at the head of the queue, each write in its own
// savepoint of one transaction.
//
// It holds the writer waitForWriter took and takes the batch only then, so the
// writes queued while it waited join. The transaction is bounded by the file's
// hold from now, not by any caller.
func (f *File) commitGroup(ctx context.Context) ([]*groupedWrite, []error) {
	defer freeSlot(f.writeSlots)
	ctx, cancel := context.WithTimeout(context.WithoutCancel(ctx), f.hold)
	defer cancel()

	var batch []*groupedWrite
	var failed []error
	started, ran := false, false
	err := f.holding(ctx, preparedTransaction(ctx, func(w Writer) error {
		batch, started = f.writes.take(), true
		failed = make([]error, len(batch))
		grouped := &groupedWriter{Writer: w}
		for i, entry := range batch {
			var err error
			if failed[i], err = runSavepoint(ctx, grouped, entry.write); err != nil {
				return err
			}
		}
		ran = true
		return nil
	}))
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

// preparedTransaction runs write in one transaction of the writer, whose
// statements it prepares on the connection and keeps
func preparedTransaction(ctx context.Context, write func(Writer) error) func(*writeConnection) (bool, error) {
	return func(connection *writeConnection) (bool, error) {
		return transactReusable(ctx, connection.conn, func(*sql.Tx) error { return write(connection) })
	}
}

// runSavepoint runs one write in a savepoint while the group's hold lasts.
//
// A write that fails is rolled back to the savepoint and fails alone; err says
// that the rollback failed as well. The savepoint's own statements take no time
// and a rollback must not be interrupted, so they run without the hold's
// deadline.
func runSavepoint(ctx context.Context, w *groupedWriter, write func(Writer) error) (failed, err error) {
	if err = ctx.Err(); err != nil {
		return nil, err
	}
	instant := context.WithoutCancel(ctx)
	if _, err = w.Writer.ExecContext(instant, savepointQuery); err != nil {
		return nil, err
	}
	failed = write(w)
	w.letGo()
	if failed == nil {
		_, err = w.Writer.ExecContext(instant, releaseQuery)
		return nil, err
	}
	if _, err = w.Writer.ExecContext(instant, rollbackToQuery); err != nil {
		return failed, errors.Join(failed, err)
	}
	_, err = w.Writer.ExecContext(instant, releaseQuery)
	return failed, err
}

// groupedWriter runs a grouped write's statements without their caller's
// cancel or deadline.
//
// SQLite rolls back the whole transaction when it interrupts a write statement,
// and every write of the group with it. An engine's own statements are bounded
// and never worth that.
type groupedWriter struct {
	Writer
	deadlines []context.CancelFunc
}

func (w *groupedWriter) ExecContext(ctx context.Context, query string, args ...any) (sql.Result, error) {
	return w.Writer.ExecContext(context.WithoutCancel(ctx), query, args...)
}

func (w *groupedWriter) QueryContext(ctx context.Context, query string, args ...any) (*sql.Rows, error) {
	return w.Writer.QueryContext(context.WithoutCancel(ctx), query, args...)
}

// UntilDeadline is a grouped write's writer whose statements end at their
// caller's deadline, for SQL that may run without end, such as an
// application's.
//
// If the deadline passes while SQLite is executing one of them, SQLite rolls
// back the whole transaction and the group fails with it. A cancel still lets
// the statement finish. Any other writer comes back as it is.
func UntilDeadline(w Writer) Writer {
	if grouped, ok := w.(*groupedWriter); ok {
		return deadlineWriter{grouped}
	}
	return w
}

type deadlineWriter struct{ grouped *groupedWriter }

func (w deadlineWriter) ExecContext(ctx context.Context, query string, args ...any) (sql.Result, error) {
	return w.grouped.Writer.ExecContext(w.grouped.untilDeadline(ctx), query, args...)
}

func (w deadlineWriter) QueryContext(ctx context.Context, query string, args ...any) (*sql.Rows, error) {
	return w.grouped.Writer.QueryContext(w.grouped.untilDeadline(ctx), query, args...)
}

// untilDeadline is ctx without its cancel, ending at its deadline when it has
// one. The rows of a query keep that context until the write returns.
func (w *groupedWriter) untilDeadline(ctx context.Context) context.Context {
	deadline, ok := ctx.Deadline()
	if !ok {
		return context.WithoutCancel(ctx)
	}
	bounded, stop := context.WithDeadline(context.WithoutCancel(ctx), deadline)
	w.deadlines = append(w.deadlines, stop)
	return bounded
}

// letGo stops the deadlines of the write that has returned
func (w *groupedWriter) letGo() {
	for _, stop := range w.deadlines {
		stop()
	}
	w.deadlines = w.deadlines[:0]
}

// outcome is one write's answer:
//
//	its own failure      it rolled back whatever the group did
//	the group's failure  the transaction ended before its commit
//	ErrOutcomeUnknown    the commit itself failed
//	nil                  none of the above
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
	g.last = len(batch)
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
