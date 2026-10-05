package jobs

import (
	"fmt"
	"regexp"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/internal/sqlite"
)

// Options says where the store keeps its queues.
type Options struct {
	// In keeps the queues in a database's own file, an sqldb DB's, instead of
	// jobs.db, so that a batch of the database commits a job with the rows it
	// is about: see Queue.Enqueued. Its tables are named _tinystore_jobs, and
	// the database's schema leaves them out. Nil keeps jobs.db.
	In Database
}

// Database is a file whose owner lets the queues live in it, as sqldb's DB
// does. A program does not implement it: its method names the store's own
// type.
type Database interface {
	SQLiteFile() *sqlite.File
}

const (
	pageSize       = 4 << 10          // 4 KiB pages measured for short rows
	readers        = 4                // reader connections: lookups, scans and the alarm's reads
	writeSlots     = 2048             // bounds writes queued for grouping
	maxKey         = 1 << 10          // a key's bytes
	maxRepeat      = 1 << 10          // a persisted cron expression and zone
	maxValue       = 1 << 20          // a value's bytes; larger ones are the blobs engine's
	inlineValue    = 512              // a value past it lives in a row of spilled
	idBlock        = 1000             // job ids reserved in one write of meta
	scanLimit      = 100              // jobs a page returns when a query does not say
	maxScanLimit   = 1000             // jobs a page may return
	scanBytes      = 4 << 20          // value bytes a page may hold
	claimBatch     = 1000             // jobs one Work write claims at most
	claimAhead     = 2                // one running and one queued per worker
	maintainEvery  = time.Minute      // periodic cleanup
	maintainBatch  = 10_000           // rows one maintenance transaction removes
	quietFailures  = 10 * time.Minute // a repeated failure is logged once in this long
	longestAlarm   = time.Minute      // max idle wait before a clock recheck
	defaultLease   = 30 * time.Second
	defaultTimeout = time.Minute
	defaultRetries = 10
	firstBackoff   = time.Second
	longestBackoff = time.Hour
	defaultFailed  = 7 * 24 * time.Hour
	defaultWaiting = 10_000_000
)

// what one claimed row holds before its worker takes it: an inline value, its
// key and repeat, and what scanning them costs
const claimRowMemory = maxKey + maxRepeat + inlineValue + 512

// a queue's name, as a file's is: short and plain
var validName = regexp.MustCompile(`^[a-z0-9][a-z0-9_-]{0,63}$`)

// QueueOption changes how a queue treats its jobs; none changes what its bytes
// mean, so a program may change them between runs.
type QueueOption interface{ queueOption(*policy) }

type ClaimOption interface{ claimOption(*claimSettings) }

type LeaseOption interface {
	QueueOption
	ClaimOption
}

type EnqueueOption interface{ enqueueOption(*enqueueSettings) }

// SettleOption names when a retried or snoozed job runs again.
type SettleOption interface{ settleOption(*settleSettings) }

type TimeOption interface {
	EnqueueOption
	SettleOption
}

type WorkOption interface{ workOption(*workSettings) }

// policy is what a queue's options say
type policy struct {
	lease       time.Duration
	maxAttempts int
	first       time.Duration
	longest     time.Duration
	maxWaiting  int64
	maxRunning  int
	maxInGroup  int
	rate        int // the jobs that may start in any span of ratePer, none when zero
	ratePer     time.Duration
	keepFailed  time.Duration
	keepDone    time.Duration
	err         error
}

// bounded says the queue bounds the jobs it runs or starts, so that a Work
// loop claims no job ahead for a busy worker: the jobs it holds are the ones
// running
func (p policy) bounded() bool {
	return p.maxRunning > 0 || p.maxInGroup > 0 || p.rate > 0
}

func defaultPolicy() policy {
	return policy{
		lease: defaultLease, maxAttempts: defaultRetries, first: firstBackoff, longest: longestBackoff,
		maxWaiting: defaultWaiting, keepFailed: defaultFailed,
	}
}

type claimSettings struct {
	lease time.Duration
	err   error
}

type enqueueSettings struct {
	at      time.Time
	after   time.Duration
	timed   bool
	key     string
	keyed   bool
	repeat  *Repeat
	move    bool
	group   string
	grouped bool
	err     error
}

type settleSettings struct {
	at    time.Time
	after time.Duration
	timed bool
	err   error
}

type workSettings struct {
	workers   int
	timeout   time.Duration
	untilIdle bool
	err       error
}

// an option of one kind only, so that the compiler refuses it for another,
// and the options that fit two kinds
type (
	forQueues   func(*policy)
	forEnqueues func(*enqueueSettings)
	forWork     func(*workSettings)
	leaseOption time.Duration
	timeOption  struct {
		at    time.Time
		after time.Duration
	}
)

func (f forQueues) queueOption(p *policy)              { f(p) }
func (f forEnqueues) enqueueOption(s *enqueueSettings) { f(s) }
func (f forWork) workOption(s *workSettings)           { f(s) }

func (l leaseOption) queueOption(p *policy) {
	if l <= 0 {
		p.err = fmt.Errorf("%w: jobs: a lease of %v", tinystore.ErrInvalid, time.Duration(l))
	}
	p.lease = time.Duration(l)
}

func (l leaseOption) claimOption(s *claimSettings) {
	if l <= 0 {
		s.err = fmt.Errorf("%w: jobs: a lease of %v", tinystore.ErrInvalid, time.Duration(l))
	}
	s.lease = time.Duration(l)
}

func (o timeOption) enqueueOption(s *enqueueSettings) {
	s.at, s.after, s.timed = o.at, o.after, true
	if err := o.check(); err != nil {
		s.err = err
	}
}

func (o timeOption) settleOption(s *settleSettings) {
	s.at, s.after, s.timed = o.at, o.after, true
	if err := o.check(); err != nil {
		s.err = err
	}
}

// check refuses a time past the years a job's time is kept for, which lie
// well before where parked jobs begin
func (o timeOption) check() error {
	if o.at.IsZero() || o.at.Year() >= 1 && o.at.Year() <= 9999 {
		return nil
	}
	return fmt.Errorf("%w: jobs: a time in the year %d, not 1 to 9999", tinystore.ErrInvalid, o.at.Year())
}

// At runs the job at t by the store's clock; a time in the past runs it now.
// The year of t is 1 to 9999.
func At(t time.Time) TimeOption {
	return timeOption{at: t}
}

// After runs the job d from now.
func After(d time.Duration) TimeOption {
	return timeOption{after: max(d, 0)}
}

// Key names the job, so that Get, Update and Cancel find it and an Enqueue
// under a key whose job waits adds nothing; see the queue's rules for keys.
func Key(key string) EnqueueOption {
	return forEnqueues(func(s *enqueueSettings) {
		if key == "" || len(key) > maxKey {
			s.err = fmt.Errorf("%w: jobs: a key of %d bytes, not 1 to 1024", tinystore.ErrInvalid, len(key))
		}
		s.key, s.keyed = key, true
	})
}

// Move sets the time of the job under the Enqueue's key whatever it was: a
// waiting job moves later as well as earlier, a running one runs again at that
// time after this run, and a key without a job gets one. A deadline that each
// ping pushes back is then one call a ping:
//
//	checks.Enqueue(ctx, check, jobs.Key(id), jobs.At(now.Add(grace)), jobs.Move())
//
// In a queue with KeepDone a key still runs once.
func Move() EnqueueOption {
	return forEnqueues(func(s *enqueueSettings) { s.move = true })
}

// Group puts the job in a group of its queue, such as the customer database it
// works on, so that the queue's MaxRunningInGroup bounds the group's jobs that
// run at once. A group is 1 to 1024 bytes of text. A job keeps its group until
// an Update, or an Enqueue that starts it again after it failed, names another.
func Group(name string) EnqueueOption {
	return forEnqueues(func(s *enqueueSettings) {
		if name == "" || len(name) > maxKey {
			s.err = fmt.Errorf("%w: jobs: a group of %d bytes, not 1 to 1024", tinystore.ErrInvalid, len(name))
		}
		s.group, s.grouped = name, true
	})
}

// Lease is how long a claimed job stays its worker's before another claim may
// take it: 30 seconds unless it says, for a queue or for one Claim. Work
// extends it while its handler runs.
func Lease(d time.Duration) LeaseOption {
	return leaseOption(d)
}

// MaxAttempts is how many attempts a job has before it fails for good, 10
// unless it says.
func MaxAttempts(n int) QueueOption {
	return forQueues(func(p *policy) {
		if n < 1 {
			p.err = fmt.Errorf("%w: jobs: MaxAttempts(%d)", tinystore.ErrInvalid, n)
		}
		p.maxAttempts = n
	})
}

// Backoff is a retry's wait: first after the first failure, doubling after
// each, never past longest, each a tenth longer or shorter at random. It is one
// second to an hour unless it says.
func Backoff(first, longest time.Duration) QueueOption {
	return forQueues(func(p *policy) {
		if first <= 0 || longest < first {
			p.err = fmt.Errorf("%w: jobs: Backoff(%v, %v)", tinystore.ErrInvalid, first, longest)
		}
		p.first, p.longest = first, longest
	})
}

// MaxWaiting refuses a job past n in the queue with tinystore.ErrLimit, ten
// million unless it says. A loop that enqueues without end then stops at a
// limit and not at a full disk.
func MaxWaiting(n int64) QueueOption {
	return forQueues(func(p *policy) {
		if n < 1 {
			p.err = fmt.Errorf("%w: jobs: MaxWaiting(%d)", tinystore.ErrInvalid, n)
		}
		p.maxWaiting = n
	})
}

// MaxRunning is how many of the queue's jobs may run at once, across every
// Work loop and Claim of the store; past it a claim takes nothing until a
// job is settled. A Work loop of such a queue claims no job ahead for a busy
// worker, so that its held jobs are the ones running.
func MaxRunning(n int) QueueOption {
	return forQueues(func(p *policy) {
		if n < 1 {
			p.err = fmt.Errorf("%w: jobs: MaxRunning(%d)", tinystore.ErrInvalid, n)
		}
		p.maxRunning = n
	})
}

// MaxRunningInGroup is how many jobs of one Group may run at once, across every
// Work loop and Claim of the store, while MaxRunning bounds the whole queue: no
// more than two refreshes of one customer's database at once. A claim passes
// over the jobs of a group whose places are taken, and they wait, in the order
// of their times, until the group has room. A job without a group is not bound.
func MaxRunningInGroup(n int) QueueOption {
	return forQueues(func(p *policy) {
		if n < 1 {
			p.err = fmt.Errorf("%w: jobs: MaxRunningInGroup(%d)", tinystore.ErrInvalid, n)
		}
		p.maxInGroup = n
	})
}

// Rate lets at most n of the queue's jobs start in any span of per, across
// every Work loop and Claim of the store: Rate(30, time.Second) for an API that
// takes 30 messages a second. Past it a claim takes nothing until the oldest
// start of the span is per old. The starts are counted in memory, so a
// restart forgets those before it; per is at least a millisecond.
func Rate(n int, per time.Duration) QueueOption {
	return forQueues(func(p *policy) {
		if n < 1 || per < time.Millisecond {
			p.err = fmt.Errorf("%w: jobs: Rate(%d, %v)", tinystore.ErrInvalid, n, per)
		}
		p.rate, p.ratePer = n, per
	})
}

// KeepFailed keeps a job that failed for good for d, seven days unless it says.
func KeepFailed(d time.Duration) QueueOption {
	return forQueues(func(p *policy) {
		if d <= 0 {
			p.err = fmt.Errorf("%w: jobs: KeepFailed(%v)", tinystore.ErrInvalid, d)
		}
		p.keepFailed = d
	})
}

// KeepDone remembers the key of an acknowledged job for d, and an Enqueue
// under a key that waits, runs or ran within d adds nothing: a key runs once.
func KeepDone(d time.Duration) QueueOption {
	return forQueues(func(p *policy) {
		if d <= 0 {
			p.err = fmt.Errorf("%w: jobs: KeepDone(%v)", tinystore.ErrInvalid, d)
		}
		p.keepDone = d
	})
}

// Workers is how many handlers Work runs at once, one unless it says; with one
// a queue runs its jobs in the order of their times.
func Workers(n int) WorkOption {
	return forWork(func(s *workSettings) {
		if n < 1 {
			s.err = fmt.Errorf("%w: jobs: Workers(%d)", tinystore.ErrInvalid, n)
		}
		s.workers = n
	})
}

// Timeout is the deadline of a handler's context, a minute unless it says;
// past it the attempt failed.
func Timeout(d time.Duration) WorkOption {
	return forWork(func(s *workSettings) {
		if d <= 0 {
			s.err = fmt.Errorf("%w: jobs: Timeout(%v)", tinystore.ErrInvalid, d)
		}
		s.timeout = d
	})
}

// UntilIdle makes Work return once no job is due and none runs, for tests and
// Manual stores.
func UntilIdle() WorkOption {
	return forWork(func(s *workSettings) { s.untilIdle = true })
}

func collectPolicy(options []QueueOption) (policy, error) {
	p := defaultPolicy()
	for _, option := range options {
		option.queueOption(&p)
	}
	return p, p.err
}

func collectEnqueue(options []EnqueueOption) (enqueueSettings, error) {
	var s enqueueSettings
	for _, option := range options {
		option.enqueueOption(&s)
	}
	return s, s.err
}

func collectSettle(options []SettleOption) (settleSettings, error) {
	var s settleSettings
	for _, option := range options {
		option.settleOption(&s)
	}
	return s, s.err
}

func collectClaim(p policy, options []ClaimOption) (claimSettings, error) {
	s := claimSettings{lease: p.lease}
	for _, option := range options {
		option.claimOption(&s)
	}
	return s, s.err
}

func collectWork(options []WorkOption) (workSettings, error) {
	s := workSettings{workers: 1, timeout: defaultTimeout}
	for _, option := range options {
		option.workOption(&s)
	}
	return s, s.err
}

// when is a time an Enqueue or a settlement names in unix milliseconds, or
// fallback when it names none
func when(at time.Time, after time.Duration, timed bool, now, fallback int64) int64 {
	switch {
	case !timed:
		return fallback
	case !at.IsZero():
		return at.UnixMilli()
	}
	return now + after.Milliseconds()
}
