package jobs

import (
	"time"

	"tinyshed.org/tinystore/internal/core"
	"tinyshed.org/tinystore/internal/wire"
)

// Each call takes the options it has a use for, and a call given one it has
// none for does not compile: jobs.Attempts opens a queue and never adds a job.

// QueueOption is how a queue opens.
type QueueOption interface{ queue(*queueOptions) }

// ScheduleOption is how a schedule opens: its repeat, attempts, backoff and timeout.
type ScheduleOption interface{ schedule(*queueOptions) }

// AddOption is what an Add gives besides its value: an id, a time, a group, a repeat.
type AddOption interface{ add(*jobOptions) }

// SetOption is what a Set or an Update gives besides its id and value: a time, a group, a repeat.
type SetOption interface{ set(*jobOptions) }

// ListOption is how a page of jobs reads.
type ListOption interface{ list(*listOptions) }

// WorkOption is how a worker runs its handlers.
type WorkOption interface{ work(*workOptions) }

type queueOptions struct {
	attempts    *uint64
	backoff     *wire.JobsBackoff
	timeout     *time.Duration
	concurrency *wire.JobsConcurrency
	rate        *wire.JobsRate
	groupRate   *wire.JobsRate
	dedupe      *time.Duration
	keep        *time.Duration
	maxWaiting  *uint64
	every       *time.Duration
	cron        *cron
	err         error
}

type jobOptions struct {
	id    *string
	at    *time.Time
	delay *time.Duration
	group *string
	every *time.Duration
	cron  *cron
}

type listOptions struct {
	prefix, state, after *string
	limit                *uint64
}

type workOptions struct{ handlers *uint64 }

type attempts int

// Attempts is how many times a job runs before it fails for good, the first
// run counted: 10 unless given.
func Attempts(n int) attempts { return attempts(n) }

func (a attempts) queue(o *queueOptions)    { n := uint64(max(int(a), 0)); o.attempts = &n }
func (a attempts) schedule(o *queueOptions) { a.queue(o) }

// Backoff is the wait before a retry: Initial, doubling each time up to Max,
// a tenth longer or shorter at random; 1 s doubling to 1 h unless given.
type Backoff struct {
	Initial, Max time.Duration
}

func (b Backoff) queue(o *queueOptions) {
	initial, err := core.Millis(b.Initial, "a backoff's initial")
	most, err2 := core.Millis(b.Max, "a backoff's max")
	if err != nil || err2 != nil {
		o.err = core.Errorf(core.Invalid, "a backoff of %s to %s", b.Initial, b.Max)
		return
	}
	o.backoff = &wire.JobsBackoff{Initial: initial, Max: most}
}

func (b Backoff) schedule(o *queueOptions) { b.queue(o) }

type timeout time.Duration

// Timeout is how long one run may take: a minute unless given. Past it the
// run's ctx ends, and what the handler then returns settles the job.
func Timeout(d time.Duration) timeout { return timeout(d) }

func (t timeout) queue(o *queueOptions)    { d := time.Duration(t); o.timeout = &d }
func (t timeout) schedule(o *queueOptions) { t.queue(o) }

// Concurrency is how many jobs of a queue run at once: Total across every
// worker of the store, Group in each group, so that one customer's backlog
// holds back no other. Given to Work, Total is how many handlers that worker runs.
type Concurrency struct {
	Total, Group int
}

func (c Concurrency) queue(o *queueOptions) {
	o.concurrency = &wire.JobsConcurrency{}
	if c.Total > 0 {
		n := uint64(c.Total)
		o.concurrency.Total = &n
	}
	if c.Group > 0 {
		n := uint64(c.Group)
		o.concurrency.Group = &n
	}
}

func (c Concurrency) work(o *workOptions) {
	if c.Total > 0 {
		n := uint64(c.Total)
		o.handlers = &n
	}
}

type rate struct {
	n   int
	per time.Duration
}

// Rate bounds how many runs of a queue start a span: jobs.Rate(30, time.Second).
func Rate(n int, per time.Duration) rate { return rate{n, per} }

func (r rate) queue(o *queueOptions) {
	per, err := core.Millis(r.per, "a rate's span")
	if err != nil || r.n < 1 || per < 1 {
		o.err = core.Errorf(core.Invalid, "a rate of %d a %s", r.n, r.per)
		return
	}
	o.rate = &wire.JobsRate{Count: uint64(r.n), Per: per}
}

type groupRate rate

// GroupRate bounds how many runs of each group of a queue start a span,
// beside its Rate: jobs.GroupRate(2, time.Second), each account's own limit.
// It lives in memory, as Rate does.
func GroupRate(n int, per time.Duration) groupRate { return groupRate{n, per} }

func (r groupRate) queue(o *queueOptions) {
	per, err := core.Millis(r.per, "a group's rate's span")
	if err != nil || r.n < 1 || per < 1 {
		o.err = core.Errorf(core.Invalid, "a group's rate of %d a %s", r.n, r.per)
		return
	}
	o.groupRate = &wire.JobsRate{Count: uint64(r.n), Per: per}
}

type dedupe time.Duration

// Dedupe is how long a done job's id stays taken, so that the same id added
// again meanwhile adds nothing.
func Dedupe(d time.Duration) dedupe { return dedupe(d) }

func (d dedupe) queue(o *queueOptions) { v := time.Duration(d); o.dedupe = &v }

type keep time.Duration

// Keep is how long a failed job stays: a week unless given.
func Keep(d time.Duration) keep { return keep(d) }

func (k keep) queue(o *queueOptions) { v := time.Duration(k); o.keep = &v }

type maxWaiting int

// MaxWaiting is how many jobs a queue holds waiting before an add is a Limit error.
func MaxWaiting(n int) maxWaiting { return maxWaiting(n) }

func (m maxWaiting) queue(o *queueOptions) { n := uint64(max(int(m), 0)); o.maxWaiting = &n }

type id string

// ID is a job's id, which finds it again and which an add of a taken one adds nothing under.
func ID(s string) id { return id(s) }

func (i id) add(o *jobOptions) { s := string(i); o.id = &s }

type at time.Time

// At is when a job runs.
func At(t time.Time) at { return at(t) }

func (a at) add(o *jobOptions) { t := time.Time(a); o.at = &t }
func (a at) set(o *jobOptions) { a.add(o) }

type delay time.Duration

// Delay is how long from now a job runs, on the store's clock.
func Delay(d time.Duration) delay { return delay(d) }

func (d delay) add(o *jobOptions) { v := time.Duration(d); o.delay = &v }
func (d delay) set(o *jobOptions) { d.add(o) }

type group string

// Group is the group a job runs in, which Concurrency's Group bounds.
func Group(g string) group { return group(g) }

func (g group) add(o *jobOptions) { s := string(g); o.group = &s }
func (g group) set(o *jobOptions) { g.add(o) }

type every time.Duration

// Every repeats a job every span, which needs an id; a schedule's repeat too.
func Every(d time.Duration) every { return every(d) }

func (e every) add(o *jobOptions)        { v := time.Duration(e); o.every = &v }
func (e every) set(o *jobOptions)        { e.add(o) }
func (e every) schedule(o *queueOptions) { v := time.Duration(e); o.every = &v }

type cron struct {
	expr, timeZone string
}

// Cron repeats a job by a cron of five fields in a time zone, an IANA name,
// "UTC" among them: jobs.Cron("0 20 * * *", "Europe/Berlin").
func Cron(expr, timeZone string) cron { return cron{expr, timeZone} }

func (c cron) add(o *jobOptions)        { o.cron = &c }
func (c cron) set(o *jobOptions)        { o.cron = &c }
func (c cron) schedule(o *queueOptions) { o.cron = &c }

type prefix string

// Prefix lists the jobs whose ids start with it.
func Prefix(p string) prefix { return prefix(p) }

func (p prefix) list(o *listOptions) { s := string(p); o.prefix = &s }

type state string

// The states a list may keep to.
const (
	Scheduled state = "scheduled"
	Waiting   state = "waiting"
	Running   state = "running"
	Failed    state = "failed"
)

func (s state) list(o *listOptions) { v := string(s); o.state = &v }

type limit int

// Limit is how many jobs a page holds: 100, and 1000 at most.
func Limit(n int) limit { return limit(n) }

func (l limit) list(o *listOptions) { n := uint64(max(int(l), 0)); o.limit = &n }

type after string

// After starts a page after the page before's Next.
func After(next string) after { return after(next) }

func (a after) list(o *listOptions) {
	if a != "" {
		s := string(a)
		o.after = &s
	}
}
