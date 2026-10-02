package server

import (
	"context"
	"encoding/json"
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
	remote := newRemoteWork(c, ask.Cancels)
	if handle.schedule != nil {
		err = workRemotely(remote, handle.schedule, options)
	} else {
		err = workRemotely(remote, handle.queue, options)
	}
	if err != nil {
		return err
	}
	if err = remote.refusal(); err != nil {
		return err
	}
	return trailer(c, wire.Empty{})
}

// workOptions are a remote Work's options; its workers are at most a
// connection's streams in flight.
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
	if ask.UntilIdle {
		options = append(options, jobs.UntilIdle())
	}
	return options, nil
}

func workRemotely[V any](remote *remoteWork, queue *jobs.Queue[V], options []jobs.WorkOption) error {
	stop := remote.listen()
	defer stop()
	err := queue.Work(remote.ctx, func(ctx context.Context, job jobs.Job[V]) error {
		number, answer, inHand := remote.expect(job.Progress)
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
// sends back to the handlers waiting for them.
//
// When the client ends its side, cancels or leaves, the jobs in its hands fail
// as a dead worker's would. The loop's context ends only after their handlers
// return, so that the jobs it claimed ahead and never handed over go back
// uncounted.
type remoteWork struct {
	call    *call
	ctx     context.Context // the Work loop's
	stop    context.CancelFunc
	cancels bool           // the client stops a job Cancel takes, when told
	sending sync.Mutex     // a job at a time on the stream, whose credit has one waiter
	inHand  sync.WaitGroup // jobs handed over whose handlers have not returned

	mu      sync.Mutex
	last    uint64
	waiting map[uint64]handedOver
	gone    error
	goneNow chan struct{}
	refused error // an outcome the client sent that no job can take, which ends the stream

	listening sync.WaitGroup
}

// handedOver is a job in the client's hands: where its outcome goes, and what
// keeps what the client reports of it meanwhile
type handedOver struct {
	answer   chan wire.JobsOutcome
	progress func(any)
}

func newRemoteWork(c *call, cancels bool) *remoteWork {
	remote := &remoteWork{
		call: c, cancels: cancels, waiting: map[uint64]handedOver{}, goneNow: make(chan struct{}),
	}
	remote.ctx, remote.stop = context.WithCancel(context.WithoutCancel(c.ctx))
	return remote
}

// listen reads the client's outcomes until its side ends, and returns the
// function that stops it, to be called once the loop has returned.
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
		if err == nil && outcome.How == wire.JobExtend {
			err = fmt.Errorf("%w: jobs: a work stream extends its jobs' leases itself; "+
				"only a claimed job is extended", tinystore.ErrInvalid)
		}
		if err == nil && outcome.How == wire.JobProgress {
			_, err = progressOf(outcome)
		}
		switch {
		case err != nil:
			w.refuse(err)
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
// client is gone; progress keeps what the client reports of it
func (w *remoteWork) expect(progress func(any)) (number uint64, answer chan wire.JobsOutcome, inHand bool) {
	w.mu.Lock()
	defer w.mu.Unlock()
	if w.gone != nil {
		return 0, nil, false
	}
	w.inHand.Add(1)
	w.last++
	answer = make(chan wire.JobsOutcome, 1)
	w.waiting[w.last] = handedOver{answer: answer, progress: progress}
	return w.last, answer, true
}

func (w *remoteWork) done(number uint64) {
	w.mu.Lock()
	delete(w.waiting, number)
	w.mu.Unlock()
	w.inHand.Done()
}

// offer sends a job to the client and waits for its outcome. A job Cancel
// takes meanwhile is the client's to stop, when it takes cancels, and what it
// sends back for it settles nothing.
func (w *remoteWork) offer(ctx context.Context, held wire.JobsHeld, answer chan wire.JobsOutcome) (
	wire.JobsOutcome, error,
) {
	if err := w.send(held); err != nil {
		return wire.JobsOutcome{}, err
	}
	select {
	case outcome := <-answer:
		return outcome, nil
	case <-w.goneNow:
		return wire.JobsOutcome{}, fmt.Errorf("%w: %w", errWorkerGone, w.why())
	case <-ctx.Done():
		if w.cancels && errors.Is(context.Cause(ctx), jobs.ErrCancelled) {
			if err := w.send(wire.JobsHeld{Job: held.Job, Cancelled: true}); err != nil {
				return wire.JobsOutcome{}, err
			}
		}
		return wire.JobsOutcome{}, ctx.Err()
	}
}

func (w *remoteWork) send(held wire.JobsHeld) error {
	w.sending.Lock()
	defer w.sending.Unlock()
	return item(w.call, held)
}

// deliver hands an outcome to the job it names, or keeps what the client
// reports of the job, which stays in its hands
func (w *remoteWork) deliver(outcome wire.JobsOutcome) {
	w.mu.Lock()
	handed, ok := w.waiting[outcome.Job]
	w.mu.Unlock()
	switch {
	case !ok:
	case outcome.How == wire.JobProgress:
		handed.progress(json.RawMessage(outcome.Progress))
	default:
		select {
		case handed.answer <- outcome:
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

func (w *remoteWork) refuse(err error) {
	w.mu.Lock()
	defer w.mu.Unlock()
	w.refused = err
}

func (w *remoteWork) refusal() error {
	w.mu.Lock()
	defer w.mu.Unlock()
	return w.refused
}

func (w *remoteWork) why() error {
	w.mu.Lock()
	defer w.mu.Unlock()
	return w.gone
}
