package jobs

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"sync"
	"time"

	"github.com/tinyshed/tinystore/internal/core"
	"github.com/tinyshed/tinystore/internal/wire"
)

// mostHandlers is how many handlers a client's worker runs at once at most,
// as the store bounds them.
const mostHandlers = 1024

// The pause before a worker dials again after a failure, doubling up to the last.
const (
	firstPause = 100 * time.Millisecond
	lastPause  = 30 * time.Second
)

// Run is this call on a job: what it is, its steps and progress, and the
// answers its handler returns. Its handler's ctx ends past its timeout, and
// when a cancel takes the job.
type Run struct {
	// ID is the job's id, "" for one added without.
	ID string
	// At is when the run was due, which "thirty days ago" counts from even when it starts late.
	At time.Time
	// Attempt is this run's number among the job's attempts, the first being 1.
	Attempt int
	Group   string
	steps   *core.Connection
	number  uint64
	report  *reporter
}

// outcome is how a run ends otherwise than done, made by its Retry, Snooze
// or Fail: its handler returns it.
type outcome struct {
	run    *Run
	answer wire.JobsAnswer
}

func (o *outcome) Error() string {
	if o.answer.Error != nil {
		return "run." + o.answer.How + ": " + *o.answer.Error
	}
	return "run." + o.answer.How
}

// Retry runs the job again after its backoff, the run counted as an attempt: return it.
func (r *Run) Retry() error { return &outcome{r, wire.JobsAnswer{How: "retry"}} }

// RetryAfter runs the job again d from now, the run counted as an attempt: return it.
func (r *Run) RetryAfter(d time.Duration) error { return r.timed("retry", d) }

// RetryAt runs the job again at a time, the run counted as an attempt: return it.
func (r *Run) RetryAt(t time.Time) error { return r.at("retry", t) }

// Snooze runs the job again d from now, the run not counted: return it.
func (r *Run) Snooze(d time.Duration) error { return r.timed("snooze", d) }

// SnoozeUntil runs the job again at a time, the run not counted: return it.
func (r *Run) SnoozeUntil(t time.Time) error { return r.at("snooze", t) }

// Fail fails the job for good now, its reason kept as its error: return it.
func (r *Run) Fail(reason any) error {
	text := fmt.Sprint(reason)
	if err, ok := reason.(error); ok {
		text = err.Error()
	}
	return &outcome{r, wire.JobsAnswer{How: "fail", Error: &text}}
}

func (r *Run) timed(how string, d time.Duration) error {
	ms, err := core.Millis(d, "a run's delay")
	if err != nil {
		return err
	}
	return &outcome{r, wire.JobsAnswer{How: how, Delay: &ms}}
}

func (r *Run) at(how string, t time.Time) error {
	at := t.UnixMilli()
	return &outcome{r, wire.JobsAnswer{How: how, At: &at}}
}

// SetProgress shows how far the job got, anything JSON writes within 4 KiB:
// the latest goes, ten a second at most.
func (r *Run) SetProgress(progress any) {
	text, err := json.Marshal(progress)
	if err != nil || len(text) > 4096 {
		return
	}
	r.report.set(string(text))
}

// Step runs fn once in the job's run and keeps its answer, as JSON, across
// the job's attempts: the attempt after a retry, a lost worker or a restart
// gets the answer back without running fn again. A step whose attempt ends
// before its answer is kept runs again, so what fn does outside the store
// should bear doing twice. A name is the step's within its job, numbered in
// a loop: "model:1", "tool:1".
func Step[R any](ctx context.Context, run *Run, name string, fn func(ctx context.Context) (R, error)) (R, error) {
	var zero R
	asked := wire.Encode(&wire.JobsStep{Run: run.number, Name: name})
	answer, err := run.steps.Session.Call(ctx, wire.MethodJobsStep, asked)
	if err != nil {
		return zero, err
	}
	var kept wire.JobsKept
	if err := decode(answer, &kept); err != nil {
		return zero, err
	}
	if kept.Found {
		var value R
		if err := json.Unmarshal([]byte(text(kept.Answer)), &value); err != nil {
			return zero, core.Wrap(core.Corrupt, err, "step %q: a kept answer that does not read", name)
		}
		return value, nil
	}
	value, err := fn(ctx)
	if err != nil {
		return zero, err
	}
	encoded, err := json.Marshal(value)
	if err != nil {
		return zero, core.Wrap(core.Invalid, err, "step %q: an answer JSON cannot write", name)
	}
	textAnswer := string(encoded)
	keep := wire.Encode(&wire.JobsStep{Run: run.number, Name: name, Answer: &textAnswer})
	if _, err := run.steps.Session.Call(ctx, wire.MethodJobsKeep, keep); err != nil {
		return zero, err
	}
	return value, nil
}

// reporter sends what a handler reports of its job: the latest, one send at
// a time and one each 100 ms at most.
type reporter struct {
	send    func(text string)
	mu      sync.Mutex
	latest  *string
	sending bool
	sent    time.Time
	timer   *time.Timer
	stopped bool
	idle    *sync.Cond
}

func newReporter(send func(text string)) *reporter {
	r := &reporter{send: send}
	r.idle = sync.NewCond(&r.mu)
	return r
}

func (r *reporter) set(text string) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.latest = &text
	r.flush()
}

func (r *reporter) flush() {
	if r.stopped || r.sending || r.latest == nil {
		return
	}
	if wait := time.Until(r.sent.Add(100 * time.Millisecond)); wait > 0 {
		if r.timer == nil {
			r.timer = time.AfterFunc(wait, func() {
				r.mu.Lock()
				r.timer = nil
				r.flush()
				r.mu.Unlock()
			})
		}
		return
	}
	text := *r.latest
	r.latest, r.sending, r.sent = nil, true, time.Now()
	go func() {
		r.send(text)
		r.mu.Lock()
		r.sending = false
		r.idle.Broadcast()
		r.flush()
		r.mu.Unlock()
	}()
}

// stop drops what waits and lets what is being sent finish, before the run's answer follows it.
func (r *reporter) stop() {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.stopped = true
	if r.timer != nil {
		r.timer.Stop()
	}
	for r.sending {
		r.idle.Wait()
	}
}

// Work runs handler on each job as it falls due, until ctx ends or the store
// closes; then it takes no job more and returns once the handlers under way,
// each bounded by its timeout, have answered. As many run at once as the
// queue's concurrency lets, one when it sets none, or Concurrency's Total
// under it. A worker whose connection is lost reaches the store again, the
// jobs it held running again as a dead worker's would.
func (q *Queue[T]) Work(ctx context.Context, handler func(ctx context.Context, value T, run *Run) error, opts ...WorkOption) error {
	handlers, err := handlersOf(opts, q.describe)
	if err != nil {
		return err
	}
	return keepWorking(ctx, &q.source, handlers, func(ctx context.Context, held wire.JobsHeld, run *Run) error {
		var value T
		if held.Value != nil && *held.Value != "" {
			if err := json.Unmarshal([]byte(*held.Value), &value); err != nil {
				return run.Fail(fmt.Sprintf("its value no longer reads: %v", err))
			}
		}
		return handler(ctx, value, run)
	})
}

// RunDue runs what is due when it starts and what falls due meanwhile, then
// returns; it never waits for a later job. Tests, scripts and commands use it.
func (q *Queue[T]) RunDue(ctx context.Context, handler func(ctx context.Context, value T, run *Run) error, opts ...WorkOption) error {
	handlers, err := handlersOf(opts, q.describe)
	if err != nil {
		return err
	}
	_, err = runDue(ctx, &q.source, handlers, func(ctx context.Context, held wire.JobsHeld, run *Run) error {
		var value T
		if held.Value != nil && *held.Value != "" {
			if err := json.Unmarshal([]byte(*held.Value), &value); err != nil {
				return run.Fail(fmt.Sprintf("its value no longer reads: %v", err))
			}
		}
		return handler(ctx, value, run)
	})
	return err
}

func handlersOf(opts []WorkOption, describe string) (*uint64, error) {
	var o workOptions
	for _, opt := range opts {
		opt.work(&o)
	}
	if o.handlers != nil && (*o.handlers < 1 || *o.handlers > mostHandlers) {
		return nil, core.Errorf(core.Invalid, "%s: a worker of %d handlers, not 1 to %d", describe, *o.handlers, mostHandlers)
	}
	return o.handlers, nil
}

type handle func(ctx context.Context, held wire.JobsHeld, run *Run) error

// keepWorking runs a worker until ctx ends or the store closes: a stream on
// the link's connection, and another once one is lost.
func keepWorking(ctx context.Context, s *source, handlers *uint64, run handle) error {
	if s.err != nil {
		return s.err
	}
	stopping := make(chan struct{})
	var stopOnce sync.Once
	stop := func() { stopOnce.Do(func() { close(stopping) }) }
	finished := make(chan struct{})
	forget := s.store.OnClose(core.StopWorkers, func() {
		stop()
		<-finished
	})
	defer forget()
	defer close(finished)
	go func() {
		select {
		case <-ctx.Done():
			stop()
		case <-stopping:
		case <-finished:
		}
	}()
	failures := core.FailureLog{Work: s.describe + " worker"}
	pause := firstPause
	for {
		select {
		case <-stopping:
			return nil
		default:
		}
		err := s.store.Link.Run(context.WithoutCancel(ctx), core.Reading, func(c *core.Connection) error {
			_, err := serve(ctx, c, s, handlers, false, stopping, run)
			return err
		})
		select {
		case <-stopping:
			return nil
		default:
		}
		if err == nil {
			failures.Succeeded()
			pause = firstPause
		} else {
			if k := core.KindOf(err); k == core.Invalid || k == core.PermissionDenied {
				return err
			}
			failures.Failed(err)
			pause = min(pause*2, lastPause)
		}
		select {
		case <-stopping:
			return nil
		case <-time.After(pause):
		}
	}
}

// runDue runs a stream that ends once nothing is due, and says how many jobs
// its handlers finished.
func runDue(ctx context.Context, s *source, handlers *uint64, run handle) (int, error) {
	if s.err != nil {
		return 0, s.err
	}
	finished := 0
	err := s.store.Link.Run(ctx, core.Reading, func(c *core.Connection) (err error) {
		finished, err = serve(ctx, c, s, handlers, true, nil, run)
		return err
	})
	return finished, err
}

// serve opens a worker's stream on a connection and runs it to its end,
// which the store sends after a stop or a GOAWAY once the jobs in hand are
// answered, or once nothing is due when it ends when idle. It says how many
// jobs its handlers finished.
func serve(ctx context.Context, c *core.Connection, s *source, handlers *uint64, untilIdle bool, stopping <-chan struct{}, run handle) (int, error) {
	// the stream outlives ctx: a stop ends it once the handlers under way answered
	streaming := context.WithoutCancel(ctx)
	id, err := core.HandleOn(streaming, c, s.opener)
	if err != nil {
		return 0, err
	}
	asked := wire.Encode(&wire.JobsWork{Handle: id, Concurrency: handlers, UntilIdle: untilIdle})
	stream, err := c.Session.Open(streaming, wire.MethodJobsWork, asked, false)
	if err != nil {
		return 0, err
	}
	w := &workStream{source: s, conn: c, stream: stream, run: run, runs: map[uint64]*running{}}
	served := make(chan struct{})
	defer close(served)
	if stopping != nil {
		go func() {
			select {
			case <-stopping:
				_ = stream.Send(streaming, wire.Encode(&wire.JobsAnswer{How: "stop"}), false)
			case <-served:
			}
		}()
	}
	return w.serve(streaming)
}

// workStream is a worker's stream on one connection: the jobs the store
// hands over, and the handlers they run here.
type workStream struct {
	source   *source
	conn     *core.Connection
	stream   *core.Stream
	run      handle
	mu       sync.Mutex
	runs     map[uint64]*running
	finished int
	running  sync.WaitGroup
}

type running struct {
	cancel    context.CancelCauseFunc
	cancelled bool
}

var errCancelled = errors.New("a cancel took the job while it ran")

func (w *workStream) serve(ctx context.Context) (int, error) {
	defer w.running.Wait()
	started, err := w.stream.Next(ctx)
	if err != nil {
		w.cancelAll()
		return 0, err
	}
	for !started.End {
		event, err := w.stream.Next(ctx)
		if err != nil {
			w.cancelAll()
			return w.count(), err
		}
		w.stream.Consumed(len(event.Body))
		if event.End {
			break
		}
		var held wire.JobsHeld
		if err := decode(event.Body, &held); err != nil {
			w.cancelAll()
			return w.count(), err
		}
		w.take(ctx, held)
	}
	w.running.Wait()
	return w.count(), nil
}

func (w *workStream) count() int {
	w.mu.Lock()
	defer w.mu.Unlock()
	return w.finished
}

func (w *workStream) cancelAll() {
	w.mu.Lock()
	defer w.mu.Unlock()
	for _, r := range w.runs {
		r.cancel(errCancelled)
	}
}

// take runs a job the store handed over at once, since it sends no more than
// the handlers; or cancels one.
func (w *workStream) take(ctx context.Context, held wire.JobsHeld) {
	w.mu.Lock()
	if held.Cancelled {
		if r, ok := w.runs[held.Run]; ok {
			r.cancelled = true
			r.cancel(errCancelled)
		}
		w.mu.Unlock()
		return
	}
	runCtx, cancel := context.WithCancelCause(ctx)
	r := &running{cancel: cancel}
	w.runs[held.Run] = r
	w.mu.Unlock()
	w.running.Add(1)
	go func() {
		defer w.running.Done()
		defer func() {
			w.mu.Lock()
			delete(w.runs, held.Run)
			w.mu.Unlock()
			cancel(nil)
		}()
		w.handle(runCtx, held, r)
	}()
}

// handle runs one job's handler and sends the answer it ended with; a job a
// cancel took gets none.
func (w *workStream) handle(ctx context.Context, held wire.JobsHeld, r *running) {
	number := held.Run
	report := newReporter(func(progress string) {
		_ = w.stream.Send(context.Background(), wire.Encode(&wire.JobsAnswer{Run: number, How: "progress", Progress: &progress}), false)
	})
	run := &Run{ID: text(held.Id), At: core.Moment(held.At), Attempt: int(held.Attempt), Group: text(held.Group),
		steps: w.conn, number: number, report: report}
	timed, stop := context.WithTimeout(ctx, w.source.timeout)
	err := w.handled(timed, held, run)
	timedOut := errors.Is(timed.Err(), context.DeadlineExceeded)
	stop()
	report.stop()
	w.mu.Lock()
	cancelled := r.cancelled
	if !cancelled {
		w.finished++
	}
	w.mu.Unlock()
	if cancelled {
		return
	}
	answer := answerOf(err, run, timedOut, w.source.timeout)
	answer.Run = number
	_ = w.stream.Send(context.Background(), wire.Encode(&answer), false)
}

func (w *workStream) handled(ctx context.Context, held wire.JobsHeld, run *Run) (err error) {
	defer func() {
		if r := recover(); r != nil {
			err = fmt.Errorf("the handler panicked: %v", r)
		}
	}()
	return w.run(ctx, held, run)
}

// answerOf is how a handler's error ends its run: done for none, an
// answer the run made, or a retry naming the error.
func answerOf(err error, run *Run, timedOut bool, timeout time.Duration) wire.JobsAnswer {
	if err == nil {
		return wire.JobsAnswer{How: "done"}
	}
	var made *outcome
	if errors.As(err, &made) {
		if made.run != run {
			reason := "the handler returned an answer another run made"
			return wire.JobsAnswer{How: "retry", Error: &reason}
		}
		return made.answer
	}
	reason := err.Error()
	if timedOut && errors.Is(err, context.DeadlineExceeded) {
		reason = fmt.Sprintf("the run passed its timeout of %s", timeout)
	}
	return wire.JobsAnswer{How: "retry", Error: &reason}
}
