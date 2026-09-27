package jobs

import (
	"math"
	"sync"
	"time"
)

// alarm is a queue's next time in memory. A Work loop sets it from the file
// when a claim found fewer jobs than it wanted; a write that makes a job due
// sooner lowers it and wakes every loop waiting on it; so nothing polls, and a
// million jobs due next week cost nothing until next week:
//
//	claim finds 3 of 8, the next due at 18:00   → at 18:00, loops sleep until then
//	Enqueue at 17:30 commits                    → at 17:30, the loops wake and sleep again
//	18:00 − nothing lowered it                  → the loops claim
type alarm struct {
	mu      sync.Mutex
	at      int64 // unix milliseconds; math.MaxInt64 when nothing waits
	lowered bool  // a write lowered it since a loop last read the file
	changed chan struct{}
}

// newAlarm rings at once, so that the first loop reads the file
func newAlarm() *alarm {
	return &alarm{changed: make(chan struct{})}
}

// lower is a write's, once it has committed a job due at
func (a *alarm) lower(at int64) {
	a.mu.Lock()
	defer a.mu.Unlock()
	if at >= a.at {
		return
	}
	a.at, a.lowered = at, true
	close(a.changed)
	a.changed = make(chan struct{})
}

// rung says whether a job may be due at now, and starts a read of the file
// whose answer set gives
func (a *alarm) rung(now int64) bool {
	a.mu.Lock()
	defer a.mu.Unlock()
	a.lowered = false
	return now >= a.at
}

// set is a loop's after the file said when the queue next needs a claim, or
// zero for never; a write that lowered the alarm since keeps its time
func (a *alarm) set(next int64) {
	a.mu.Lock()
	defer a.mu.Unlock()
	if next == 0 {
		next = math.MaxInt64
	}
	if a.lowered {
		next = min(next, a.at)
	}
	a.at = next
}

// wait is what a loop sleeps on: the channel a lowering closes, and how long
// until the alarm rings, a minute at most, so that a clock that jumps is met
func (a *alarm) wait(now int64) (<-chan struct{}, time.Duration) {
	a.mu.Lock()
	defer a.mu.Unlock()
	sleep := longestAlarm
	if a.at != math.MaxInt64 {
		sleep = min(sleep, time.Duration(max(a.at-now, 0))*time.Millisecond)
	}
	return a.changed, sleep
}
