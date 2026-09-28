package server

import (
	"context"
	"errors"
	"fmt"
	"sync"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/jobs"
	"github.com/tinyshed/tinystore/server/wire"
)

var (
	// errWorkerGone fails a job in a remote worker's hands when the worker
	// left without settling it, as its process dying would
	errWorkerGone = errors.New("jobs: the remote worker went away with the job in its hands")
	errWorkerDone = errors.New("jobs: the remote worker ended its side of the stream")
	errWorkEnded  = errors.New("jobs: the Work loop ended")
)

// jobsWork is the queue's own Work loop for a client: a job it hands a
// handler goes to the client as DATA, and the outcome the client sends back
// settles it. Claiming ahead, settling in the write of the next claim and
// extending leases stay the engine's, and nothing polls.
func jobsWork(c *call) error {
	var ask wire.JobsWorkers
	if err := ask.Decode(c.request); err != nil {
		return err
	}
	handle, err := c.session.jobsHandles.get(ask.Handle)
	if err != nil {
		return err
	}
	options, err := workOptions(ask, c.session.agreed.inFlight)
	if err != nil {
		return err
	}
	if err = begin(c, wire.Empty{}); err != nil {
		return err
	}
	remote := newRemoteWork(c)
	if handle.schedule != nil {
		err = workRemotely(remote, handle.schedule, options)
	} else {
		err = workRemotely(remote, handle.queue, options)
	}
	if err != nil {
		return err
	}
	return trailer(c, wire.Empty{})
}

// workOptions is a remote Work's, its workers no more than a connection's
// streams in flight
func workOptions(ask wire.JobsWorkers, most uint32) ([]jobs.WorkOption, error) {
	var options []jobs.WorkOption
	if ask.Workers > uint64(most) {
		return nil, fmt.Errorf("%w: jobs: %d workers on one stream; at most %d", tinystore.ErrInvalid,
			ask.Workers, most)
	}
	if ask.Workers > 0 {
		options = append(options, jobs.Workers(int(ask.Workers))) //nolint:gosec // at most the streams in flight
	}
	if ask.Timeout > 0 {
		options = append(options, jobs.Timeout(durationOf(ask.Timeout)))
	}
	return options, nil
}

func workRemotely[V any](remote *remoteWork, queue *jobs.Queue[V], options []jobs.WorkOption) error {
	stop := remote.listen()
	defer stop()
	err := queue.Work(remote.ctx, func(ctx context.Context, job jobs.Job[V]) error {
		number, answer, inHand := remote.expect()
		if !inHand {
			<-ctx.Done() // never handed over: the loop's end gives it back uncounted
			return ctx.Err()
		}
		defer remote.done(number)

		held, err := heldOf(job)
		if err != nil {
			return err
		}
		held.Job = number
		outcome, err := remote.offer(ctx, held, answer)
		if err != nil {
			return err
		}
		return settle(ctx, job, outcome)
	}, options...)
	if errors.Is(err, context.Canceled) && remote.ctx.Err() != nil {
		return nil
	}
	return err
}

// remoteWork hands a Work loop's jobs to a client and routes the outcomes it
// sends back to the handlers waiting for them. When the client ends its side,
// cancels or leaves, the jobs in its hands fail as a dead worker's would, and
// only after their handlers return does the loop's context end, so that the
// jobs it claimed ahead and never handed over go back uncounted.
type remoteWork struct {
	call    *call
	ctx     context.Context // the Work loop's
	stop    context.CancelFunc
	sending sync.Mutex     // a job at a time on the stream, whose credit has one waiter
	inHand  sync.WaitGroup // jobs handed over whose handlers have not returned

	mu      sync.Mutex
	last    uint64
	waiting map[uint64]chan wire.JobsOutcome
	gone    error
	goneNow chan struct{}

	listening sync.WaitGroup
}

func newRemoteWork(c *call) *remoteWork {
	remote := &remoteWork{call: c, waiting: map[uint64]chan wire.JobsOutcome{}, goneNow: make(chan struct{})}
	remote.ctx, remote.stop = context.WithCancel(context.WithoutCancel(c.ctx))
	return remote
}

// listen reads the client's outcomes until its side ends, and returns what
// stops it once the loop has
func (w *remoteWork) listen() (stop func()) {
	w.listening.Go(func() { w.leave(w.readOutcomes()) })
	unwatch := context.AfterFunc(w.call.ctx, func() { w.leave(context.Cause(w.call.ctx)) })
	return func() {
		unwatch()
		w.call.upload.fail(errWorkEnded)
		w.leave(errWorkEnded)
		w.listening.Wait()
		w.stop()
	}
}

func (w *remoteWork) readOutcomes() error {
	for {
		body, last, err := w.call.receive()
		if err != nil {
			return err
		}
		var outcome wire.JobsOutcome
		if len(body) > 0 {
			err = outcome.Decode(body)
		}
		w.call.consumed(body)
		switch {
		case err != nil:
			return err
		case len(body) > 0:
			w.deliver(outcome)
		}
		if last {
			return errWorkerDone
		}
	}
}

// expect counts a job into the client's hands and names it, unless the
// client is gone
func (w *remoteWork) expect() (number uint64, answer chan wire.JobsOutcome, inHand bool) {
	w.mu.Lock()
	defer w.mu.Unlock()
	if w.gone != nil {
		return 0, nil, false
	}
	w.inHand.Add(1)
	w.last++
	answer = make(chan wire.JobsOutcome, 1)
	w.waiting[w.last] = answer
	return w.last, answer, true
}

func (w *remoteWork) done(number uint64) {
	w.mu.Lock()
	delete(w.waiting, number)
	w.mu.Unlock()
	w.inHand.Done()
}

// offer sends a job to the client and waits for its outcome
func (w *remoteWork) offer(ctx context.Context, held wire.JobsHeld, answer chan wire.JobsOutcome) (
	wire.JobsOutcome, error,
) {
	w.sending.Lock()
	err := item(w.call, held)
	w.sending.Unlock()
	if err != nil {
		return wire.JobsOutcome{}, err
	}
	select {
	case outcome := <-answer:
		return outcome, nil
	case <-w.goneNow:
		return wire.JobsOutcome{}, fmt.Errorf("%w: %w", errWorkerGone, w.why())
	case <-ctx.Done():
		return wire.JobsOutcome{}, ctx.Err()
	}
}

func (w *remoteWork) deliver(outcome wire.JobsOutcome) {
	w.mu.Lock()
	answer := w.waiting[outcome.Job]
	w.mu.Unlock()
	if answer != nil {
		select {
		case answer <- outcome:
		default: // a second outcome for one job: the first settles it
		}
	}
}

// leave says no outcome will come: the jobs in the client's hands fail, and
// once their handlers returned the loop's context ends
func (w *remoteWork) leave(cause error) {
	w.mu.Lock()
	defer w.mu.Unlock()
	if w.gone != nil {
		return
	}
	w.gone = cause
	close(w.goneNow)
	w.listening.Go(func() {
		w.inHand.Wait()
		w.stop()
	})
}

func (w *remoteWork) why() error {
	w.mu.Lock()
	defer w.mu.Unlock()
	return w.gone
}
