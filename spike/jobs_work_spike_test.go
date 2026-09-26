package spike

import (
	"cmp"
	"context"
	"database/sql"
	"encoding/binary"
	"errors"
	"math"
	"math/rand/v2"
	"slices"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/tinyshed/tinystore/internal/sqlite"
)

// an insert under a key that a job already holds adds nothing
const jobsIfAbsent = ` on conflict (queue, key) where key is not null do nothing`

// TestJobsEnqueue measures a durable Enqueue from 1 to 512 goroutines, each in
// a grouped write, into a queue of a million keyed jobs spread over a week, on
// both layouts: a job due now, one at a random time of the week, and one there
// under a key the insert checks first
func TestJobsEnqueue(t *testing.T) {
	kvMeasuring(t)
	ctx := t.Context()
	for _, name := range []string{"arrival", "time"} {
		layout := jobsLayouts[name]
		for _, kind := range []string{"now", "later", "keyed"} {
			file, _ := jobsOpen(t, layout)
			count := jobsCount()
			jobsFill(t, file, layout, jobsFixture{count: count, from: jobsFrom, span: 7 * 24 * time.Hour, keyed: true})
			statement := layout.insert
			if kind == "keyed" {
				statement += jobsIfAbsent
			}
			var next atomic.Int64
			next.Store(int64(count))
			for _, workers := range []int{1, 8, 64, 512} {
				before := jobsCommits(t, file)
				result := kvLoad(workers, jobsSeconds(), func(int) error {
					i := int(next.Add(1))
					at, key := jobsEnqueued(kind, i)
					return file.UpdateGrouped(ctx, 256, func(w sqlite.Writer) error {
						_, err := w.ExecContext(ctx, statement, jobsQueue, at, i, key, at, jobsValue(i, 0))
						return err
					})
				})
				commits := jobsCommits(t, file) - before
				t.Logf("%-7s %-5s %3d goroutines  Enqueues %s  %5.1f a commit", name, kind, workers, result,
					float64(result.ops)/float64(max(commits, 1)))
			}
		}
	}
}

// jobsEnqueued is job i's time and key: due now, before every job of the week,
// or at a random time of it, keyed when kind says so
func jobsEnqueued(kind string, i int) (int64, any) {
	if kind == "now" {
		return jobsFrom - 1_000_000_000 + int64(i), nil
	}
	at := jobsFrom + rand.New(rand.NewPCG(uint64(i), 31)).Int64N(7*24*3600*1000)
	if kind == "keyed" {
		return at, jobsKey(i)
	}
	return at, nil
}

// jobsAlarm is a queue's next due time in memory: the loop sets it from the
// file when a claim found fewer jobs than it wanted, and an Enqueue due sooner
// lowers it and wakes the loop, so that nothing polls
type jobsAlarm struct {
	mu      sync.Mutex
	at      int64 // unix milliseconds; math.MaxInt64 when nothing waits
	lowered bool  // an Enqueue lowered it since the loop last read the file
	wake    chan struct{}
}

// newJobsAlarm rings at once, so that a loop that starts reads the file
func newJobsAlarm() *jobsAlarm {
	return &jobsAlarm{wake: make(chan struct{}, 1)}
}

// lower is an Enqueue's, once it has committed a job due at
func (a *jobsAlarm) lower(at int64) {
	a.mu.Lock()
	lowered := at < a.at
	if lowered {
		a.at, a.lowered = at, true
	}
	a.mu.Unlock()
	if lowered {
		select {
		case a.wake <- struct{}{}:
		default:
		}
	}
}

// rung says whether a job may be due at now, and starts a read of the file
func (a *jobsAlarm) rung(now int64) bool {
	a.mu.Lock()
	defer a.mu.Unlock()
	a.lowered = false
	return now >= a.at
}

// set is the loop's after the file said the next job is due at next, or that
// none waits when next is negative; an Enqueue that lowered it since wins
func (a *jobsAlarm) set(next int64) {
	a.mu.Lock()
	defer a.mu.Unlock()
	if next < 0 {
		next = math.MaxInt64
	}
	if a.lowered {
		next = min(next, a.at)
	}
	a.at = next
}

// until is how long the loop may sleep, or -1 when nothing waits
func (a *jobsAlarm) until(now int64) time.Duration {
	a.mu.Lock()
	defer a.mu.Unlock()
	if a.at == math.MaxInt64 {
		return -1
	}
	return time.Duration(max(a.at-now, 0)) * time.Millisecond
}

// jobsPipe is the Work loop docs/jobs.md describes: it claims as many due
// jobs as it has free workers in one grouped write with the acknowledgements
// of the jobs its workers finished since, and when nothing is due waits for a
// finished job, its alarm or an Enqueue that lowers it
type jobsPipe struct {
	file    *sqlite.File
	layout  jobsLayout
	workers int
	handle  func(jobsClaimed)
	alarm   *jobsAlarm
	clock   func() int64 // unix milliseconds
}

// jobsWritten is what a run of the loop committed: the jobs acknowledged and
// the writes that carried them
type jobsWritten struct {
	acked, writes int
}

func (p *jobsPipe) run(ctx context.Context) (jobsWritten, error) {
	hand := make(chan jobsClaimed, p.workers)
	finished := make(chan jobsClaimed, p.workers)
	var working sync.WaitGroup
	for range p.workers {
		working.Go(func() {
			for job := range hand {
				p.handle(job)
				finished <- job
			}
		})
	}
	defer func() {
		close(hand)
		working.Wait()
	}()

	var written jobsWritten
	var done []jobsClaimed
	free := p.workers
	for ctx.Err() == nil {
		done, free = jobsGather(finished, done, free)
		now := p.clock()
		want := 0
		if free > 0 && p.alarm.rung(now) {
			want = free
		}
		claimed, err := p.settleAndClaim(ctx, done, now, want)
		if err != nil {
			return written, err
		}
		if len(done) > 0 || want > 0 {
			written.acked += len(done)
			written.writes++
		}
		done = done[:0]
		for _, job := range claimed {
			hand <- job
		}
		free -= len(claimed)
		if want > 0 && len(claimed) == want {
			continue
		}
		if job, got := p.wait(ctx, finished, free, now); got {
			done, free = append(done, job), free+1
		}
	}
	return written, ctx.Err()
}

// jobsGather takes the jobs the workers finished without waiting for more
func jobsGather(finished chan jobsClaimed, done []jobsClaimed, free int) ([]jobsClaimed, int) {
	for {
		select {
		case job := <-finished:
			done, free = append(done, job), free+1
		default:
			return done, free
		}
	}
}

// settleAndClaim acknowledges done and claims up to want jobs in one grouped
// write, and when fewer than want were due, sets the alarm from the file
func (p *jobsPipe) settleAndClaim(ctx context.Context, done []jobsClaimed, now int64, want int) ([]jobsClaimed, error) {
	if len(done) == 0 && want == 0 {
		return nil, nil
	}
	var claimed []jobsClaimed
	next := int64(-1)
	err := p.file.UpdateGrouped(ctx, 0, func(w sqlite.Writer) error {
		claimed, next = nil, -1
		for _, job := range done {
			if err := p.layout.ack(ctx, w, job); err != nil {
				return err
			}
		}
		if want == 0 {
			return nil
		}
		var err error
		claimed, err = p.layout.claim(ctx, w, now, now+jobsLease, want)
		if err == nil && len(claimed) < want {
			err = sqlite.QueryRow(ctx, w, p.layout.next, jobsQueue, now).Scan(&next)
			if errors.Is(err, sql.ErrNoRows) {
				next, err = -1, nil
			}
		}
		return err
	})
	if err == nil && want > 0 && len(claimed) < want {
		p.alarm.set(next)
	}
	return claimed, err
}

// wait blocks until a worker finishes, the alarm rings, an Enqueue lowers it,
// or ctx ends; a loop with no free worker waits for a worker only
func (p *jobsPipe) wait(ctx context.Context, finished chan jobsClaimed, free int, now int64) (jobsClaimed, bool) {
	var rings <-chan time.Time
	var wake chan struct{}
	if free > 0 {
		wake = p.alarm.wake
		if until := p.alarm.until(now); until >= 0 {
			timer := time.NewTimer(until)
			defer timer.Stop()
			rings = timer.C
		}
	}
	select {
	case job := <-finished:
		return job, true
	case <-wake:
	case <-rings:
	case <-ctx.Done():
	}
	return jobsClaimed{}, false
}

// TestJobsWork runs the Work loop over two million jobs due at once with a
// handler that does nothing, from 1 to 512 workers, on each layout: what a
// queue's own machinery allows a second, and how many jobs each write carried
func TestJobsWork(t *testing.T) {
	kvMeasuring(t)
	for _, name := range []string{"arrival", "time", "time-mark", "time-table"} {
		layout := jobsLayouts[name]
		file, _ := jobsOpen(t, layout)
		jobsFill(t, file, layout, jobsFixture{count: 2 * jobsCount(), from: jobsFrom, span: time.Minute, inOrder: true})
		for _, workers := range []int{1, 8, 64, 512} {
			pipe := &jobsPipe{
				file: file, layout: layout, workers: workers, handle: func(jobsClaimed) {},
				alarm: newJobsAlarm(), clock: func() int64 { return jobsFrom + 61_000 },
			}
			ctx, cancel := context.WithTimeout(t.Context(), jobsSeconds())
			began := time.Now()
			written, err := pipe.run(ctx)
			elapsed := time.Since(began)
			cancel()
			if err != nil && !errors.Is(err, context.DeadlineExceeded) {
				t.Fatal(err)
			}
			t.Logf("%-10s %3d workers  %8d jobs in %s  %9.0f a second  %6d writes, %6.1f jobs a write", name,
				workers, written.acked, jobsMillis(elapsed), float64(written.acked)/elapsed.Seconds(),
				written.writes, float64(written.acked)/float64(max(written.writes, 1)))
		}
	}
}

// TestJobsWake enqueues one job every 5 ms into an empty queue that a Work
// loop of eight workers waits on, and times each from its Enqueue's call to its
// handler's start; then jobs due 100 ms after their Enqueue, from their time
func TestJobsWake(t *testing.T) {
	kvMeasuring(t)
	for _, delay := range []time.Duration{0, 100 * time.Millisecond} {
		layout := jobsLayouts["time-mark"]
		file, _ := jobsOpen(t, layout)
		var mu sync.Mutex
		var late []time.Duration
		pipe := &jobsPipe{
			file: file, layout: layout, workers: 8, alarm: newJobsAlarm(),
			clock: func() int64 { return time.Now().UnixMilli() },
			handle: func(job jobsClaimed) {
				due := int64(binary.LittleEndian.Uint64(job.value))
				mu.Lock()
				late = append(late, time.Duration(time.Now().UnixNano()-due))
				mu.Unlock()
			},
		}
		ctx, cancel := context.WithCancel(t.Context())
		var running sync.WaitGroup
		running.Go(func() { _, _ = pipe.run(ctx) })

		calls := jobsProduce(t, file, pipe.alarm, delay)
		time.Sleep(delay + 200*time.Millisecond)
		cancel()
		running.Wait()

		mu.Lock()
		slices.Sort(late)
		slices.Sort(calls)
		t.Logf("due %4d ms after the call: %4d jobs, the handler began p50 %s p99 %s worst %s after it; "+
			"an Enqueue p50 %s", delay.Milliseconds(), len(late), jobsMillis(late[len(late)/2]),
			jobsMillis(late[len(late)*99/100]), jobsMillis(late[len(late)-1]), jobsMillis(calls[len(calls)/2]))
		mu.Unlock()
	}
}

// jobsProduce enqueues a job every 5 ms for the load's length, each due delay
// after its call and carrying that time in nanoseconds, and lowers the alarm
// after each; it returns how long each Enqueue took
func jobsProduce(t *testing.T, file *sqlite.File, alarm *jobsAlarm, delay time.Duration) []time.Duration {
	t.Helper()
	ctx := t.Context()
	insert := jobsLayouts["time-mark"].insert
	var calls []time.Duration
	deadline := time.Now().Add(jobsSeconds())
	for i := 1; time.Now().Before(deadline); i++ {
		began := time.Now()
		due := began.Add(delay)
		value := binary.LittleEndian.AppendUint64(nil, uint64(due.UnixNano()))
		err := file.UpdateGrouped(ctx, 64, func(w sqlite.Writer) error {
			_, err := w.ExecContext(ctx, insert, jobsQueue, due.UnixMilli(), i, nil, due.UnixMilli(), value)
			return err
		})
		if err != nil {
			t.Fatal(err)
		}
		calls = append(calls, time.Since(began))
		alarm.lower(due.UnixMilli())
		time.Sleep(5 * time.Millisecond)
	}
	return calls
}

// the application's messages, as a handler of scheduled messages writes them
const (
	jobsMessages = `create table messages (
		id      integer primary key,
		chat    integer not null,
		author  integer not null,
		text    text    not null,
		sent_at integer not null
	) strict`
	jobsMessage = `insert into messages (id, chat, author, text, sent_at) values (?1, ?2, ?3, ?4, ?5)`
)

// TestJobsHandlerWrites is a handler's own write: one message inserted into
// the application's database from 1 to 512 workers, a transaction each, as
// sqldb's Exec commits today, against the inserts waiting behind a leader
// committed together
func TestJobsHandlerWrites(t *testing.T) {
	kvMeasuring(t)
	ctx := t.Context()
	text := string(jobsValue(0, 0))
	for _, way := range []string{"one each", "grouped"} {
		for _, workers := range []int{1, 8, 64, 512} {
			file, _ := jobsOpen(t, jobsLayout{schema: []string{jobsMessages}})
			var next atomic.Int64
			before := jobsCommits(t, file)
			result := kvLoad(workers, jobsSeconds(), func(int) error {
				i := next.Add(1)
				args := []any{i, i % 100_000, i % 1_000_000, text, time.Now().UnixMilli()}
				if way == "one each" {
					return file.Update(ctx, func(tx *sql.Tx) error {
						_, err := tx.ExecContext(ctx, jobsMessage, args...)
						return err
					})
				}
				return file.UpdateGrouped(ctx, 256, func(w sqlite.Writer) error {
					_, err := w.ExecContext(ctx, jobsMessage, args...)
					return err
				})
			})
			commits := jobsCommits(t, file) - before
			t.Logf("%-8s %3d workers  inserts %s  %5.1f a commit", way, workers, result,
				float64(result.ops)/float64(max(commits, 1)))
		}
	}
}

var errJobsNoneDue = errors.New("no job was due")

// TestJobsClaimAck is the raw calls: from 1 to 512 goroutines, each claims one
// job in a grouped write and acknowledges it in another, over a million due
func TestJobsClaimAck(t *testing.T) {
	kvMeasuring(t)
	ctx := t.Context()
	for _, name := range []string{"time", "time-mark", "time-table"} {
		layout := jobsLayouts[name]
		file, _ := jobsOpen(t, layout)
		jobsFill(t, file, layout, jobsFixture{count: jobsCount(), from: jobsFrom, span: time.Minute, inOrder: true})
		now := jobsFrom + 61_000
		for _, workers := range []int{1, 8, 64, 512} {
			before := jobsCommits(t, file)
			result := kvLoad(workers, jobsSeconds(), func(int) error {
				var claimed []jobsClaimed
				err := file.UpdateGrouped(ctx, 0, func(w sqlite.Writer) error {
					var claimErr error
					claimed, claimErr = layout.claim(ctx, w, now, now+jobsLease, 1)
					return claimErr
				})
				if err != nil || len(claimed) == 0 {
					return cmp.Or(err, errJobsNoneDue)
				}
				return file.UpdateGrouped(ctx, 0, func(w sqlite.Writer) error { return layout.ack(ctx, w, claimed[0]) })
			})
			commits := jobsCommits(t, file) - before
			t.Logf("%-10s %3d goroutines  a Claim and its Ack %s  %5.1f calls a commit", name, workers, result,
				2*float64(result.ops)/float64(max(commits, 1)))
		}
	}
}
