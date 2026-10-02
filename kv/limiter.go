package kv

import (
	"context"
	"fmt"
	"math"
	"time"

	"github.com/tinyshed/tinystore"
)

// the kind of bucket a limiter keeps its times in
const kindLimiter = "limiter"

// how often a limiter's times reach the file, and so what a crash forgets
const limiterFlush = time.Second

// Limiter lets each key's requests through at a steady rate with room for a
// burst. It is the generic cell rate algorithm: a key holds one time, when its
// next request is due, so there is no window whose edge lets twice the rate
// through, and a key costs one int64.
//
//	Rate(3, time.Second), Burst(3): three pass at once, the fourth waits 333ms
//
// Its times live in memory and reach the file every second and on Close, as
// LoseAtMost counters do: a crash forgets at most the last second, and so lets
// at most one burst more through. A key whose time has come is as one never
// seen, and maintenance deletes it.
type Limiter struct {
	branch
	memory   *memory
	interval int64 // the nanoseconds one request takes of the rate
	burst    int64
}

// Allowance is what a limiter answers a request.
type Allowance struct {
	OK bool
	// Left is how many more requests would pass now.
	Left int64
	// RetryAfter is how long until the request would pass; zero when OK.
	RetryAfter time.Duration
}

// LimiterOption says how fast a limiter lets requests through.
type LimiterOption func(*limiterSettings)

type limiterSettings struct {
	count int64
	per   time.Duration
	burst int64
}

// Rate lets count requests through every per, each key alone: Rate(100,
// time.Second). A limiter needs one.
func Rate(count int64, per time.Duration) LimiterOption {
	return func(s *limiterSettings) { s.count, s.per = count, per }
}

// Burst is how many requests may pass at once before the rate holds the next;
// count of the Rate when it is not given.
func Burst(n int64) LimiterOption {
	return func(s *limiterSettings) { s.burst = n }
}

// OpenLimiter opens the limiter name of kv.db, creating it the first time. A
// name that holds values or counters is ErrInvalid; the rate is the caller's,
// and may change between runs.
func OpenLimiter(ctx context.Context, state *Store, name string, options ...LimiterOption) (*Limiter, error) {
	interval, burst, err := limiterRate(options)
	if err != nil {
		return nil, fmt.Errorf("kv: limiter %q: %w", name, err)
	}

	id, err := state.claimBucket(ctx, name, kindLimiter)
	if err != nil {
		return nil, err
	}

	held, err := state.memoryFor(name, id, limiterFlush)
	if err != nil {
		return nil, err
	}
	opened := branch{state: state, id: id, name: name}
	return &Limiter{branch: opened, memory: held, interval: interval, burst: burst}, nil
}

// limiterRate is the nanoseconds a request takes and the burst, refused when
// they are not a rate or a burst would pass what a time can hold
func limiterRate(options []LimiterOption) (interval, burst int64, err error) {
	var said limiterSettings
	for _, option := range options {
		option(&said)
	}
	if said.burst == 0 {
		said.burst = said.count
	}
	switch {
	case said.count < 1 || said.per <= 0:
		return 0, 0, fmt.Errorf("%w: a limiter needs a Rate of at least one request a positive span",
			tinystore.ErrInvalid)
	case int64(said.per) < said.count:
		return 0, 0, fmt.Errorf("%w: a Rate of %d every %v is past one a nanosecond", tinystore.ErrInvalid,
			said.count, said.per)
	case said.burst < 1:
		return 0, 0, fmt.Errorf("%w: a Burst of %d", tinystore.ErrInvalid, said.burst)
	}
	interval = int64(said.per) / said.count
	if said.burst > math.MaxInt64/4/interval {
		return 0, 0, fmt.Errorf("%w: a Burst of %d at %v a request is past what a time holds",
			tinystore.ErrInvalid, said.burst, time.Duration(interval))
	}
	return interval, said.burst, nil
}

// Of is the branch of this limiter that owners name, as Bucket.Of names one.
func (l *Limiter) Of(owners ...any) *Limiter {
	return &Limiter{branch: l.under(owners), memory: l.memory, interval: l.interval, burst: l.burst}
}

// Allow asks for one request of key.
func (l *Limiter) Allow(ctx context.Context, key any) (Allowance, error) {
	return l.AllowN(ctx, key, 1)
}

// AllowN asks for n requests of key at once: all of them pass or none does.
// More than the burst never passes, and is ErrInvalid.
func (l *Limiter) AllowN(ctx context.Context, key any, n int64) (Allowance, error) {
	c, err := l.begin(key, nil)
	if err == nil && (n < 1 || n > l.burst) {
		err = fmt.Errorf("%w: %d requests at once, past a burst of %d", tinystore.ErrInvalid, n, l.burst)
	}
	if err != nil {
		return Allowance{}, l.fail(c, err)
	}
	release, err := l.state.admit(ctx)
	if err != nil {
		return Allowance{}, err
	}
	defer release()

	now := l.state.now().UnixNano()
	var answer Allowance
	_, err = l.memory.step(ctx, c, func(due, _ int64, _ bool) (int64, int64, bool, error) {
		var next int64
		answer, next = l.decide(due, now, n)
		return next, millisecondsAfter(next), answer.OK, nil
	})
	return answer, l.fail(c, err)
}

// decide is the algorithm: due is when the key's next request was due, zero
// for a key never seen or long quiet. Requests pass when, taken, they leave
// the key due no later than a burst of requests from now.
//
//	interval 10ms, burst 3, now 0, due 0:   next 10ms ≤ 30ms → OK, left 2
//	                        now 0, due 30ms: next 40ms > 30ms → wait 10ms
func (l *Limiter) decide(due, now, n int64) (Allowance, int64) {
	next := max(due, now) + n*l.interval
	limit := now + l.burst*l.interval
	if next > limit {
		return Allowance{RetryAfter: time.Duration(next - limit)}, due
	}
	return Allowance{OK: true, Left: (limit - next) / l.interval}, next
}

// millisecondsAfter is a time in nanoseconds as an expiry: the first
// millisecond at or after it
func millisecondsAfter(nanoseconds int64) int64 {
	return (nanoseconds + int64(time.Millisecond) - 1) / int64(time.Millisecond)
}
