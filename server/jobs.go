package server

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"sync"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/jobs"
	"github.com/tinyshed/tinystore/server/wire"
)

// jobsHandle is a queue a session opened: its values JSON as the engine keeps
// them, or a schedule's one repeating job, which holds nothing
type jobsHandle struct {
	name     string
	store    *jobs.Store
	queue    *jobs.Queue[json.RawMessage]
	schedule *jobs.Queue[struct{}]
}

// heldJob is a job a claim leased, of either value type: what its settlement
// and its steps need of it
type heldJob interface {
	Ack(ctx context.Context) error
	Retry(ctx context.Context, cause error, options ...jobs.SettleOption) error
	Fail(ctx context.Context, cause error) error
	Snooze(ctx context.Context, options ...jobs.SettleOption) error
	Extend(ctx context.Context, d time.Duration) error
	Progress(v any)
	Kept(ctx context.Context, name string) (json.RawMessage, bool, error)
	Keep(ctx context.Context, name string, answer json.RawMessage) error
}

// errSettledOnItsStream refuses jobs.settle a job a work stream handed over,
// whose outcome its stream carries
var errSettledOnItsStream = fmt.Errorf("%w: jobs: a job a work stream handed over settles on its stream",
	tinystore.ErrInvalid)

// streamed is a job a work stream handed over, in the session's claims for
// jobs.step and jobs.keep alone, under the number its stream gave it
type streamed struct{ heldJob }

func (streamed) Ack(context.Context) error { return errSettledOnItsStream }
func (streamed) Retry(context.Context, error, ...jobs.SettleOption) error {
	return errSettledOnItsStream
}
func (streamed) Fail(context.Context, error) error                  { return errSettledOnItsStream }
func (streamed) Snooze(context.Context, ...jobs.SettleOption) error { return errSettledOnItsStream }
func (streamed) Extend(context.Context, time.Duration) error        { return errSettledOnItsStream }

func (s *Server) jobsMethods(methods map[wire.Method]handler) {
	methods[wire.JobsOpen] = jobsOpen
	methods[wire.JobsEnqueue] = jobsEnqueue
	methods[wire.JobsUpdate] = jobsUpdate
	methods[wire.JobsCancel] = jobsCancel
	methods[wire.JobsGet] = jobsGet
	methods[wire.JobsClaim] = jobsClaim
	methods[wire.JobsSettle] = jobsSettle
	methods[wire.JobsScan] = jobsScan
	methods[wire.JobsWork] = jobsWork
	methods[wire.JobsStep] = jobsStep
	methods[wire.JobsKeep] = jobsKeep
	methods[wire.JobsWatch] = jobsWatch
}

func jobsOpen(c *call) error {
	var ask wire.JobsQueue
	if err := ask.Decode(c.request); err != nil {
		return err
	}
	store, err := c.session.server.jobsStore(c.ctx)
	if err != nil {
		return err
	}
	options := queueOptions(ask)
	opened := &jobsHandle{name: ask.Name, store: store}
	if ask.Schedule != nil {
		repeat, repeatErr := repeatOf(*ask.Schedule)
		if repeatErr != nil {
			return repeatErr
		}
		opened.schedule, err = jobs.OpenSchedule(c.ctx, store, ask.Name, repeat, options...)
	} else {
		opened.queue, err = jobs.OpenQueue[json.RawMessage](c.ctx, store, ask.Name, options...)
	}
	if err != nil {
		return err
	}
	return respond(c, wire.Handle{Handle: c.session.jobsHandles.add(opened)})
}

func queueOptions(ask wire.JobsQueue) []jobs.QueueOption {
	var options []jobs.QueueOption
	if ask.Lease > 0 {
		options = append(options, jobs.Lease(durationOf(ask.Lease)))
	}
	if ask.MaxAttempts > 0 {
		options = append(options, jobs.MaxAttempts(int(min(ask.MaxAttempts, 1<<31-1))))
	}
	if ask.BackoffFirst > 0 || ask.BackoffMost > 0 {
		options = append(options, jobs.Backoff(durationOf(ask.BackoffFirst), durationOf(ask.BackoffMost)))
	}
	if ask.MaxWaiting > 0 {
		options = append(options, jobs.MaxWaiting(int64(min(ask.MaxWaiting, 1<<63-1))))
	}
	if ask.KeepFailed > 0 {
		options = append(options, jobs.KeepFailed(durationOf(ask.KeepFailed)))
	}
	if ask.KeepDone > 0 {
		options = append(options, jobs.KeepDone(durationOf(ask.KeepDone)))
	}
	if ask.MaxRunning > 0 {
		options = append(options, jobs.MaxRunning(int(min(ask.MaxRunning, 1<<31-1))))
	}
	return options
}

func durationOf(ms int64) time.Duration {
	return time.Duration(ms) * time.Millisecond
}

// repeatOf is a repeat as the engine takes it: a cron expression in a zone the
// system knows by its name, or every so many milliseconds
func repeatOf(r wire.Repeat) (jobs.Repeat, error) {
	switch {
	case r.Every > 0 && r.Cron == "":
		return jobs.Every(durationOf(r.Every)), nil
	case r.Cron != "" && r.Every == 0:
		if r.Zone == "" {
			return jobs.Repeat{}, fmt.Errorf("%w: jobs: a cron repeat names its zone", tinystore.ErrInvalid)
		}
		zone, err := time.LoadLocation(r.Zone)
		if err != nil {
			return jobs.Repeat{}, fmt.Errorf("%w: jobs: a repeat's zone: %w", tinystore.ErrInvalid, err)
		}
		return jobs.Cron(r.Cron, zone), nil
	}
	return jobs.Repeat{}, fmt.Errorf("%w: jobs: a repeat is a cron expression or every so many milliseconds",
		tinystore.ErrInvalid)
}

func enqueueOptions(job wire.JobsJob) ([]jobs.EnqueueOption, error) {
	var options []jobs.EnqueueOption
	if job.Key != "" {
		options = append(options, jobs.Key(job.Key))
	}
	if job.At != 0 {
		options = append(options, jobs.At(time.UnixMilli(job.At)))
	}
	if job.After > 0 {
		options = append(options, jobs.After(durationOf(job.After)))
	}
	if job.Repeat != nil {
		repeat, err := repeatOf(*job.Repeat)
		if err != nil {
			return nil, err
		}
		options = append(options, repeat)
	}
	return options, nil
}

// jobValue is a job's value as its queue keeps it, JSON checked before it is kept
func jobValue(text string) (json.RawMessage, error) {
	if !json.Valid([]byte(text)) {
		return nil, fmt.Errorf("%w: jobs: a job's value is JSON", tinystore.ErrInvalid)
	}
	return json.RawMessage(text), nil
}

// queueOf is the handle's queue of values; a schedule's one job is not enqueued
func (h *jobsHandle) queueOf(method string) (*jobs.Queue[json.RawMessage], error) {
	if h.queue == nil {
		return nil, fmt.Errorf("%w: jobs: %q is a schedule, whose one job %s does not take", tinystore.ErrInvalid,
			h.name, method)
	}
	return h.queue, nil
}

// jobsEnqueue adds its jobs in one transaction, all or none
func jobsEnqueue(c *call) error {
	var ask wire.JobsBatch
	if err := ask.Decode(c.request); err != nil {
		return err
	}
	handle, err := c.session.jobsHandles.get(ask.Handle)
	if err != nil {
		return err
	}
	queue, err := handle.queueOf("enqueue")
	if err != nil {
		return err
	}
	if len(ask.Jobs) == 1 {
		// One job shares a commit with the writes beside it, as Enqueue does in
		// Go; a transaction of its own would pay a sync and hold the writer.
		if err = enqueueOne(c.ctx, queue, ask.Jobs[0]); err != nil {
			return opFailed(0, err)
		}
		return respond(c, wire.Empty{})
	}
	err = handle.store.Tx(c.ctx, func(tx *jobs.Tx) error {
		inside := queue.WithTx(tx)
		for i, job := range ask.Jobs {
			if enqueueErr := enqueueOne(c.ctx, inside, job); enqueueErr != nil {
				return opFailed(i, enqueueErr)
			}
		}
		return nil
	})
	if err != nil {
		return err
	}
	return respond(c, wire.Empty{})
}

func enqueueOne(ctx context.Context, queue *jobs.Queue[json.RawMessage], job wire.JobsJob) error {
	value, err := jobValue(job.Value)
	if err != nil {
		return err
	}
	options, err := enqueueOptions(job)
	if err != nil {
		return err
	}
	return queue.Enqueue(ctx, value, options...)
}

func jobsUpdate(c *call) error {
	var ask wire.JobsChange
	if err := ask.Decode(c.request); err != nil {
		return err
	}
	handle, err := c.session.jobsHandles.get(ask.Handle)
	if err != nil {
		return err
	}
	queue, err := handle.queueOf("update")
	if err != nil {
		return err
	}
	value, err := jobValue(ask.Value)
	if err != nil {
		return err
	}
	job := ask.JobsJob
	key := job.Key
	job.Key = ""
	options, err := enqueueOptions(job)
	if err != nil {
		return err
	}
	if err = queue.Update(c.ctx, key, value, options...); err != nil {
		return err
	}
	return respond(c, wire.Empty{})
}

func jobsCancel(c *call) error {
	var ask wire.JobsKey
	if err := ask.Decode(c.request); err != nil {
		return err
	}
	handle, err := c.session.jobsHandles.get(ask.Handle)
	if err != nil {
		return err
	}
	var cancelled bool
	if handle.schedule != nil {
		cancelled, err = handle.schedule.Cancel(c.ctx, ask.Key)
	} else {
		cancelled, err = handle.queue.Cancel(c.ctx, ask.Key)
	}
	if err != nil {
		return err
	}
	return respond(c, wire.JobsEntry{Found: cancelled})
}

func jobsGet(c *call) error {
	var ask wire.JobsKey
	if err := ask.Decode(c.request); err != nil {
		return err
	}
	handle, err := c.session.jobsHandles.get(ask.Handle)
	if err != nil {
		return err
	}
	var entry wire.JobsEntry
	if handle.schedule != nil {
		entry, err = getJob(c.ctx, handle.schedule, ask.Key)
	} else {
		entry, err = getJob(c.ctx, handle.queue, ask.Key)
	}
	if err != nil {
		return err
	}
	return respond(c, entry)
}

func getJob[V any](ctx context.Context, queue *jobs.Queue[V], key string) (wire.JobsEntry, error) {
	entry, found, err := queue.Get(ctx, key)
	if err != nil || !found {
		return wire.JobsEntry{}, err
	}
	return entryOfJob(entry, true)
}

func entryOfJob[V any](entry jobs.Entry[V], found bool) (wire.JobsEntry, error) {
	value, err := jsonOf(entry.Value)
	return wire.JobsEntry{
		Found: found, Key: entry.Key, Value: value, At: entry.At.UnixMilli(), Attempt: uint64(max(entry.Attempt, 0)),
		State: uint64(max(entry.State, 0)), Err: entry.Err, Repeat: entry.Repeat, Ahead: uint64(max(entry.Ahead, 0)),
		Progress: string(entry.Progress), Ran: unixMillis(entry.Ran), Took: uint64(max(entry.Took.Milliseconds(), 0)),
	}, err
}

// jobsWatch follows the job under a key: a download of its entry now and at
// each change, ended by the last, done, failed or cancelled; a key that names
// no job ends it at once. It also ends when its client cancels or leaves.
func jobsWatch(c *call) error {
	var ask wire.JobsKey
	if err := ask.Decode(c.request); err != nil {
		return err
	}
	handle, err := c.session.jobsHandles.get(ask.Handle)
	if err != nil {
		return err
	}
	c.follow()
	if err = begin(c, wire.Empty{}); err != nil {
		return err
	}
	if handle.schedule != nil {
		err = watchJob(c, handle.schedule, ask.Key)
	} else {
		err = watchJob(c, handle.queue, ask.Key)
	}
	if err != nil {
		return err
	}
	return trailer(c, wire.Empty{})
}

func watchJob[V any](c *call, queue *jobs.Queue[V], key string) error {
	for entry, err := range queue.Watch(c.ctx, key) {
		if err != nil {
			return err
		}
		answer, err := entryOfJob(entry, true)
		if err == nil {
			err = item(c, answer)
		}
		if err != nil {
			return err
		}
	}
	return context.Cause(c.ctx)
}

func jsonOf[V any](value V) (string, error) {
	if raw, ok := any(value).(json.RawMessage); ok {
		return string(raw), nil
	}
	encoded, err := json.Marshal(value)
	return string(encoded), err
}

// jobsClaim leases the next due job to the client, which settles it by the
// number the answer gives it
func jobsClaim(c *call) error {
	var ask wire.JobsLease
	if err := ask.Decode(c.request); err != nil {
		return err
	}
	handle, err := c.session.jobsHandles.get(ask.Handle)
	if err != nil {
		return err
	}
	var options []jobs.ClaimOption
	if ask.Lease > 0 {
		options = append(options, jobs.Lease(durationOf(ask.Lease)))
	}
	var held wire.JobsHeld
	if handle.schedule != nil {
		held, err = claimJob(c, handle.schedule, options)
	} else {
		held, err = claimJob(c, handle.queue, options)
	}
	if err != nil {
		return err
	}
	return respond(c, held)
}

func claimJob[V any](c *call, queue *jobs.Queue[V], options []jobs.ClaimOption) (wire.JobsHeld, error) {
	job, found, err := queue.Claim(c.ctx, options...)
	if err != nil || !found {
		return wire.JobsHeld{}, err
	}
	held, err := heldOf(job)
	if err != nil {
		return wire.JobsHeld{}, err
	}
	held.Job = c.session.claims.add(job)
	return held, nil
}

func heldOf[V any](job jobs.Job[V]) (wire.JobsHeld, error) {
	value, err := jsonOf(job.Value)
	return wire.JobsHeld{
		Found: true, Key: job.Key, Value: value, At: job.At.UnixMilli(), Attempt: uint64(max(job.Attempt, 0)),
	}, err
}

// jobsSettle writes the outcomes of claimed jobs, each settled at once so
// that they share the writer's group
func jobsSettle(c *call) error {
	var ask wire.JobsOutcomes
	if err := ask.Decode(c.request); err != nil {
		return err
	}
	answer := wire.JobsSettled{Errors: make([]*wire.Error, len(ask.Outcomes))}
	var settling sync.WaitGroup
	for i, outcome := range ask.Outcomes {
		held, err := c.session.claims.get(outcome.Job)
		if err != nil {
			answer.Errors[i] = failure(c.ctx, err)
			continue
		}
		settling.Go(func() {
			if err := settle(c.ctx, held, outcome); err != nil {
				answer.Errors[i] = failure(c.ctx, err)
				return
			}
			if outcome.How != wire.JobExtend && outcome.How != wire.JobProgress {
				c.session.claims.remove(outcome.Job)
			}
		})
	}
	settling.Wait()
	return respond(c, answer)
}

// jobsStep reads the answer a held job's run kept under a step's name
func jobsStep(c *call) error {
	var ask wire.JobsAnswer
	if err := ask.Decode(c.request); err != nil {
		return err
	}
	if ask.Answer != "" {
		return fmt.Errorf("%w: jobs: jobs.step looks a step up and carries no answer; jobs.keep keeps one",
			tinystore.ErrInvalid)
	}
	held, err := c.session.claims.get(ask.Job)
	if err != nil {
		return err
	}
	answer, found, err := held.Kept(c.ctx, ask.Name)
	if err != nil {
		return err
	}
	return respond(c, wire.JobsKept{Found: found, Answer: string(answer)})
}

// jobsKeep keeps what a step of a held job's run answered, once it is written
func jobsKeep(c *call) error {
	var ask wire.JobsAnswer
	if err := ask.Decode(c.request); err != nil {
		return err
	}
	held, err := c.session.claims.get(ask.Job)
	if err != nil {
		return err
	}
	if err = held.Keep(c.ctx, ask.Name, json.RawMessage(ask.Answer)); err != nil {
		return err
	}
	return respond(c, wire.Empty{})
}

func settle(ctx context.Context, held heldJob, outcome wire.JobsOutcome) error {
	switch outcome.How {
	case wire.JobAck:
		return held.Ack(ctx)
	case wire.JobRetry:
		return held.Retry(ctx, causeOf(outcome), timing(outcome)...)
	case wire.JobFail:
		return held.Fail(ctx, causeOf(outcome))
	case wire.JobSnooze:
		return held.Snooze(ctx, timing(outcome)...)
	case wire.JobExtend:
		return held.Extend(ctx, durationOf(outcome.After))
	case wire.JobProgress:
		progress, err := progressOf(outcome)
		if err == nil {
			held.Progress(progress)
		}
		return err
	}
	return fmt.Errorf("%w: jobs: an outcome %d, not 1 to 6", tinystore.ErrInvalid, outcome.How)
}

// progressOf is a progress outcome's report, which must be JSON
func progressOf(outcome wire.JobsOutcome) (json.RawMessage, error) {
	if !json.Valid([]byte(outcome.Progress)) {
		return nil, fmt.Errorf("%w: jobs: a progress of %d bytes that are not JSON", tinystore.ErrInvalid,
			len(outcome.Progress))
	}
	return json.RawMessage(outcome.Progress), nil
}

func causeOf(outcome wire.JobsOutcome) error {
	if outcome.Err == "" {
		return errors.New("the worker gave no reason")
	}
	return errors.New(outcome.Err)
}

func timing(outcome wire.JobsOutcome) []jobs.SettleOption {
	switch {
	case outcome.At != 0:
		return []jobs.SettleOption{jobs.At(time.UnixMilli(outcome.At))}
	case outcome.HasAfter || outcome.After > 0:
		return []jobs.SettleOption{jobs.After(durationOf(outcome.After))}
	}
	return nil
}

// jobsScan is a download: a page of the queue's jobs, a job a DATA, and a
// last DATA saying where the next page begins
func jobsScan(c *call) error {
	var ask wire.JobsQuery
	if err := ask.Decode(c.request); err != nil {
		return err
	}
	handle, err := c.session.jobsHandles.get(ask.Handle)
	if err != nil {
		return err
	}
	query := jobs.Query{
		Prefix: ask.Prefix, State: jobs.State(min(ask.State, 3)), After: ask.After, Limit: int(min(ask.Limit, 1<<20)),
	}
	if handle.schedule != nil {
		return scanJobs(c, handle.schedule, query)
	}
	return scanJobs(c, handle.queue, query)
}

func scanJobs[V any](c *call, queue *jobs.Queue[V], query jobs.Query) error {
	page, err := queue.Scan(c.ctx, query)
	if err != nil {
		return err
	}
	release, err := c.holdAnswer(func() int64 { return weighJobs(page.Entries) })
	if err != nil {
		return err
	}
	defer release()
	if err = begin(c, wire.Empty{}); err != nil {
		return err
	}
	for _, found := range page.Entries {
		entry, entryErr := entryOfJob(found, false)
		if entryErr != nil {
			return entryErr
		}
		if err = item(c, entry); err != nil {
			return err
		}
	}
	return trailer(c, wire.JobsPage{More: page.More, After: page.Next.After})
}
