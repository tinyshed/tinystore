// Package jobs is work an application must do later, or now but outside the
// request that asked for it, as docs/guides/jobs/README.md has it: queues of values run
// in the order of their time, ids that find a job again, repeats, schedules
// and limits. A worker's handlers run in this process while the store claims
// the queue's jobs and hands each over on one stream.
//
//	emails, err := jobs.OpenQueue[Email](store, "emails")
//	_, err = emails.Add(ctx, Email{To: to, Subject: subject})
//	err = emails.Work(ctx, func(ctx context.Context, email Email, run *jobs.Run) error {
//		return smtp.Send(ctx, email)
//	})
package jobs

import (
	"context"
	"encoding/json"
	"errors"
	"iter"
	"time"

	"tinyshed.org/tinystore"
	"tinyshed.org/tinystore/internal/core"
	"tinyshed.org/tinystore/internal/wire"
	"tinyshed.org/tinystore/sqldb"
)

// State is where a job stands: done while its queue's dedupe keeps its id,
// cancelled only a watcher sees.
type State string

// A job's states.
const (
	StateScheduled State = "scheduled"
	StateWaiting   State = "waiting"
	StateRunning   State = "running"
	StateDone      State = "done"
	StateFailed    State = "failed"
	StateCancelled State = "cancelled"
)

// Job is a job as its queue holds it.
type Job[T any] struct {
	// ID is "" for a job added without one.
	ID    string
	Value T
	State State
	// At is when it runs next; for one done or failed, when its last run was for.
	At time.Time
	// Attempt counts its attempts, a running one included.
	Attempt int
	// Ahead counts the jobs that run before a waiting one, up to 10,000.
	Ahead int
	// Progress is what its handler last reported of a running job, as JSON.
	Progress json.RawMessage
	// Error is why its last run failed.
	Error string
	Group string
	// Repeat is a repeating job's repeat as it keeps it: "@every 30s +6178ms", "10 3 * * * Europe/Berlin".
	Repeat string
	// LastRun is when the last run a handler finished started and ended; nil before one.
	LastRun *LastRun
}

// LastRun is when a run started and ended.
type LastRun struct {
	StartedAt, EndedAt time.Time
}

// Page is a page of a queue's jobs, and where the next starts: "" on the last.
type Page[T any] struct {
	Jobs []Job[T]
	Next string
}

// source is what a queue's or a schedule's calls and workers need of it.
type source struct {
	store    *core.Store
	describe string
	opener   *core.Opener
	via      core.Via
	timeout  time.Duration
	home     core.Database
	// tx is the store's transaction the queue was taken into, whose commit makes its writes
	tx *core.StoreTx
	// err is what In refused, which every call answers
	err error
}

func (s *source) call(ctx context.Context, method uint16, body func(handle uint64) []byte, idempotence core.Idempotence) ([]byte, error) {
	if s.err != nil {
		return nil, s.err
	}
	return s.via.Call(ctx, s.opener, method, body, idempotence)
}

// Queue is a queue of jobs of one type, run in the order of their time.
type Queue[T any] struct {
	source
	name string
}

// OpenQueue opens a queue of jobs of type T, kept as JSON, on a store or on a
// database whose file then keeps it. It opens at its first call; its error
// is a name or an option it refuses.
//
//	emails, err := jobs.OpenQueue[Email](store, "emails", jobs.Attempts(20), jobs.Backoff{Initial: 5 * time.Second, Max: 30 * time.Minute})
func OpenQueue[T any](home tinystore.Home, name string, opts ...QueueOption) (*Queue[T], error) {
	var o queueOptions
	for _, opt := range opts {
		opt.queue(&o)
	}
	of := home.CoreHome()
	describe := "jobs queue " + name
	if of.Database != nil {
		describe = of.Database.Describe() + ": " + describe
	}
	if err := core.CheckName(name, "queue"); err != nil {
		return nil, err
	}
	if o.err != nil {
		return nil, core.Wrap(core.Invalid, o.err, "%s", describe)
	}
	message := wire.JobsQueueOpen{Name: name, Attempts: o.attempts, Backoff: o.backoff, Concurrency: o.concurrency, Rate: o.rate, MaxWaiting: o.maxWaiting}
	var err error
	if message.Timeout, err = millis(o.timeout, describe+": timeout"); err != nil {
		return nil, err
	}
	if message.Dedupe, err = millis(o.dedupe, describe+": dedupe"); err != nil {
		return nil, err
	}
	if message.Keep, err = millis(o.keep, describe+": keep"); err != nil {
		return nil, err
	}
	q := &Queue[T]{name: name, source: source{store: of.Store, describe: describe, timeout: time.Minute, home: of.Database}}
	if o.timeout != nil {
		q.timeout = *o.timeout
	}
	body := wire.Encode(&message)
	if of.Database == nil {
		q.opener = &core.Opener{Method: wire.MethodJobsQueueOpen, Body: body}
		q.via = core.ViaLink{Link: of.Store.Link}
	} else {
		db := of.Database
		q.opener = db.Keep(wire.MethodJobsQueueOpen, body, func(ctx context.Context, c *core.Connection) ([]byte, error) {
			handle, err := db.Handle(ctx, c)
			if err != nil {
				return nil, err
			}
			inDB := message
			inDB.Database = &handle
			return wire.Encode(&inDB), nil
		})
		q.via = db.Via("jobs queue " + name)
	}
	return q, nil
}

func millis(d *time.Duration, of string) (*uint64, error) {
	if d == nil {
		return nil, nil
	}
	ms, err := core.Millis(*d, of)
	return &ms, err
}

// Name is the queue's name.
func (q *Queue[T]) Name() string { return q.name }

// In is the queue inside a transaction: the store's, whose commit makes its
// writes with the others, all or none, or a database's, whose writes commit
// with its rows.
func (q *Queue[T]) In(tx tinystore.InTx) *Queue[T] {
	in := *q
	of := tx.CoreTx()
	switch {
	case of.Store != nil:
		in.tx = of.Store
		if in.err == nil {
			in.err = of.Store.Takes(q.store.Link, q.home, "jobs.db", q.describe)
		}
	case of.Database != nil:
		if q.home == nil || q.home != of.Database.Database() {
			in.err = core.Errorf(core.Invalid, "%s: kept outside the database, in a transaction of %s: open it from the database",
				q.describe, of.Database.Database().Describe())
		}
		in.via = of.Database.Via()
	}
	return &in
}

// Add adds a job unless its id is taken, and says whether it added one. An
// id is taken by a job scheduled, waiting, running or failed, or done while
// the queue's dedupe keeps it. It returns once the job is on disk; a job
// without an id is always added. Inside the store's transaction it is kept
// for the commit, and says true.
func (q *Queue[T]) Add(ctx context.Context, value T, opts ...AddOption) (bool, error) {
	var o jobOptions
	for _, opt := range opts {
		opt.add(&o)
	}
	return q.changed(ctx, wire.MethodJobsAdd, value, o)
}

// Adding is a job's add for a batch of the database the queue was opened
// from: db.Batch writes it with its statements, so the job commits with the
// rows or not at all, and shares the commit with every other write of the
// file, where db.Tx holds the writer and pays a sync of its own. It adds as
// Add does, unless its id is taken.
//
//	err = db.Batch(ctx, sqldb.SQL(`update notes set body = ? where id = ?`, body, id), indexing.Adding(Index{NoteID: id}))
func (q *Queue[T]) Adding(value T, opts ...AddOption) sqldb.Statement {
	var o jobOptions
	for _, opt := range opts {
		opt.add(&o)
	}
	call, err := q.callOf(value, o)
	switch {
	case q.err != nil:
		err = q.err
	case q.tx != nil:
		err = core.Errorf(core.Invalid, "%s: a queue taken into a transaction adds there, not in a batch", q.describe)
	}
	body := func(handle uint64) []byte {
		c := call
		c.Handle = handle
		return wire.Encode(&c)
	}
	return sqldb.CoreLent(core.Lent{Home: q.home, What: "jobs queue " + q.name, Opener: q.opener, Method: wire.MethodJobsAdd, Body: body, Err: err})
}

// Set makes the id's job this value at this time, whatever it was: one
// waiting is made new, a failed one starts again, and a running one runs
// once more after this run, with this value.
func (q *Queue[T]) Set(ctx context.Context, id string, value T, opts ...SetOption) error {
	o := jobOptions{id: &id}
	for _, opt := range opts {
		opt.set(&o)
	}
	_, err := q.changed(ctx, wire.MethodJobsSet, value, o)
	return err
}

// Update changes the id's job when it has not started, and says whether it did.
func (q *Queue[T]) Update(ctx context.Context, id string, value T, opts ...SetOption) (bool, error) {
	o := jobOptions{id: &id}
	for _, opt := range opts {
		opt.set(&o)
	}
	return q.changed(ctx, wire.MethodJobsUpdate, value, o)
}

func (q *Queue[T]) changed(ctx context.Context, method uint16, value T, o jobOptions) (bool, error) {
	call, err := q.callOf(value, o)
	if err != nil {
		return false, err
	}
	if q.tx != nil || q.err != nil {
		if q.err != nil {
			return false, q.err
		}
		return true, q.tx.KeepJob(core.JobOp{Opener: q.opener, Method: method, Call: &call})
	}
	answer, err := q.call(ctx, method, func(handle uint64) []byte {
		c := call
		c.Handle = handle
		return wire.Encode(&c)
	}, core.Writing)
	return changedOf(answer, err)
}

func changedOf(answer []byte, err error) (bool, error) {
	if err != nil {
		return false, err
	}
	var changed wire.JobsChanged
	if err := decode(answer, &changed); err != nil {
		return false, err
	}
	return changed.Changed, nil
}

func (q *Queue[T]) callOf(value T, o jobOptions) (wire.JobsCall, error) {
	text, err := json.Marshal(value)
	if err != nil {
		return wire.JobsCall{}, core.Wrap(core.Invalid, err, "%s: a value JSON cannot write", q.describe)
	}
	call := wire.JobsCall{Id: o.id, Value: string(text), Group: o.group}
	if o.at != nil {
		at := o.at.UnixMilli()
		call.At = &at
	}
	if call.Delay, err = millis(o.delay, "a job's delay"); err != nil {
		return call, err
	}
	if call.Every, err = millis(o.every, "a job's repeat"); err != nil {
		return call, err
	}
	if o.cron != nil {
		call.Cron, call.TimeZone = &o.cron.expr, &o.cron.timeZone
	}
	return call, nil
}

// Cancel takes the job under id, whatever its state, and says whether there
// was one: a running one's ctx ends, and what its handler returns then
// settles nothing; a repeating one stops.
func (q *Queue[T]) Cancel(ctx context.Context, id string) (bool, error) {
	if q.tx != nil || q.err != nil {
		if q.err != nil {
			return false, q.err
		}
		return true, q.tx.KeepJob(core.JobOp{Opener: q.opener, Method: wire.MethodJobsCancel, ID: id})
	}
	return cancel(ctx, &q.source, id)
}

func cancel(ctx context.Context, s *source, id string) (bool, error) {
	return changedOf(s.call(ctx, wire.MethodJobsCancel, func(handle uint64) []byte {
		return wire.Encode(&wire.JobsId{Handle: handle, Id: id})
	}, core.Writing))
}

// Get is where the job under id is, and whether there is one.
func (q *Queue[T]) Get(ctx context.Context, id string) (Job[T], bool, error) {
	return get[T](ctx, &q.source, id)
}

func get[T any](ctx context.Context, s *source, id string) (Job[T], bool, error) {
	answer, err := s.call(ctx, wire.MethodJobsGet, func(handle uint64) []byte {
		return wire.Encode(&wire.JobsId{Handle: handle, Id: id})
	}, core.Reading)
	if err != nil {
		return Job[T]{}, false, err
	}
	var found wire.JobsJob
	if err := decode(answer, &found); err != nil || !found.Found {
		return Job[T]{}, false, err
	}
	job, err := jobOf[T](s, found)
	return job, err == nil, err
}

// jobOf is a job as the store sent it, its value read as the queue reads one.
func jobOf[T any](s *source, found wire.JobsJob) (Job[T], error) {
	job := Job[T]{
		ID: text(found.Id), State: State(found.State), At: core.MomentOf(found.At), Attempt: int(found.Attempt),
		Ahead: int(found.Ahead), Error: text(found.Error), Group: text(found.Group), Repeat: text(found.Repeat),
	}
	if found.Value != nil && *found.Value != "" {
		if err := json.Unmarshal([]byte(*found.Value), &job.Value); err != nil {
			return job, core.Wrap(core.Corrupt, err, "%s: id %q: a value that does not read", s.describe, job.ID)
		}
	}
	if found.Progress != nil {
		job.Progress = json.RawMessage(*found.Progress)
	}
	if found.StartedAt != nil {
		ended := found.StartedAt
		if found.EndedAt != nil {
			ended = found.EndedAt
		}
		job.LastRun = &LastRun{core.Moment(*found.StartedAt), core.Moment(*ended)}
	}
	return job, nil
}

func text(s *string) string {
	if s == nil {
		return ""
	}
	return *s
}

// List is a page of the queue's jobs whose ids start with a prefix, or of
// its failed ones, the last first.
func (q *Queue[T]) List(ctx context.Context, opts ...ListOption) (Page[T], error) {
	var o listOptions
	for _, opt := range opts {
		opt.list(&o)
	}
	answer, err := q.call(ctx, wire.MethodJobsList, func(handle uint64) []byte {
		return wire.Encode(&wire.JobsList{Handle: handle, Prefix: o.prefix, State: o.state, After: o.after, Limit: o.limit})
	}, core.Reading)
	if err != nil {
		return Page[T]{}, err
	}
	var page wire.JobsPage
	if err := decode(answer, &page); err != nil {
		return Page[T]{}, err
	}
	out := Page[T]{Next: text(page.Next)}
	for _, found := range page.Jobs {
		job, err := jobOf[T](&q.source, found)
		if err != nil {
			return Page[T]{}, err
		}
		out.Jobs = append(out.Jobs, job)
	}
	return out, nil
}

// All is every job the options name, a page at a time, holding nothing between pages.
func (q *Queue[T]) All(ctx context.Context, opts ...ListOption) iter.Seq2[Job[T], error] {
	return func(yield func(Job[T], error) bool) {
		next := ""
		for {
			page, err := q.List(ctx, append(opts, After(next))...)
			if err != nil {
				yield(Job[T]{}, err)
				return
			}
			for _, job := range page.Jobs {
				if !yield(job, nil) {
					return
				}
			}
			if page.Next == "" {
				return
			}
			next = page.Next
		}
	}
}

// Watch is the job under id as it is, then again each time it changes; it
// ends with the job, done, failed or cancelled, a state only a watcher sees.
// An id with no job yields nothing. A watch whose connection is lost watches
// again on the next; a job that ended meanwhile ends it without its last state.
func (q *Queue[T]) Watch(ctx context.Context, id string) iter.Seq2[Job[T], error] {
	return func(yield func(Job[T], error) bool) {
		var last *wire.JobsJob
		for {
			var stream *core.Stream
			err := q.store.Link.Run(ctx, core.Reading, func(c *core.Connection) error {
				handle, err := core.HandleOn(ctx, c, q.opener)
				if err != nil {
					return err
				}
				st, err := c.Session.Open(ctx, wire.MethodJobsWatch, wire.Encode(&wire.JobsId{Handle: handle, Id: id}), true)
				if err != nil {
					return err
				}
				if _, err := st.Next(ctx); err != nil {
					return err
				}
				stream = st
				return nil
			})
			if err != nil {
				yield(Job[T]{}, err)
				return
			}
			again, done := q.watching(ctx, stream, &last, yield)
			stream.Cancel()
			if done || !again {
				return
			}
		}
	}
}

// watching reads a watch's stream: whether to watch again on another
// connection, and whether the caller is done.
func (q *Queue[T]) watching(ctx context.Context, stream *core.Stream, last **wire.JobsJob, yield func(Job[T], error) bool) (again, done bool) {
	for {
		event, err := stream.Next(ctx)
		if err != nil {
			if lost, _ := core.IsConnectionLost(err); lost || errors.Is(err, tinystore.ErrUnavailable) {
				return true, false
			}
			yield(Job[T]{}, err)
			return false, true
		}
		stream.Consumed(len(event.Body))
		if event.End {
			return false, false
		}
		var found wire.JobsJob
		if err := decode(event.Body, &found); err != nil {
			yield(Job[T]{}, err)
			return false, true
		}
		if *last == nil || !showsTheSame(**last, found) {
			*last = &found
			job, err := jobOf[T](&q.source, found)
			if !yield(job, err) {
				return false, true
			}
		}
	}
}

// showsTheSame says whether two reports show a watcher the same: the job's
// state, place, attempt, time, progress and error.
func showsTheSame(a, b wire.JobsJob) bool {
	return a.State == b.State && a.Ahead == b.Ahead && a.Attempt == b.Attempt && same(a.At, b.At) &&
		text(a.Progress) == text(b.Progress) && text(a.Error) == text(b.Error)
}

func same(a, b *int64) bool {
	return (a == nil) == (b == nil) && (a == nil || *a == *b)
}

func decode(answer []byte, m wire.Message) error {
	if err := wire.Decode(answer, m); err != nil {
		return core.Wrap(core.Protocol, err, "an answer that does not read")
	}
	return nil
}
