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
	reads   map[*alarmRead]struct{}
	changed chan struct{}
}

// alarmRead is one loop's read of the file, and the earliest time a write
// committed while it read, which the read may not have seen
type alarmRead struct {
	lowered int64
}

// newAlarm rings at once, so that the first loop reads the file
func newAlarm() *alarm {
	return &alarm{reads: map[*alarmRead]struct{}{}, changed: make(chan struct{})}
}

// lower is a write's, once it has committed a job due at
func (a *alarm) lower(at int64) {
	a.mu.Lock()
	defer a.mu.Unlock()
	for read := range a.reads {
		read.lowered = min(read.lowered, at)
	}
	if at >= a.at {
		return
	}
	a.at = at
	close(a.changed)
	a.changed = make(chan struct{})
}

// rung starts a read of the file when a job may be due at now, and is nil
// when none may be
func (a *alarm) rung(now int64) *alarmRead {
	a.mu.Lock()
	defer a.mu.Unlock()
	if now < a.at {
		return nil
	}
	read := &alarmRead{lowered: math.MaxInt64}
	a.reads[read] = struct{}{}
	return read
}

// due says whether a job may be due at now, without starting a read
func (a *alarm) due(now int64) bool {
	a.mu.Lock()
	defer a.mu.Unlock()
	return now >= a.at
}

// set ends a read that found the queue's next time, zero for never; a write
// committed while it read keeps its time when it is sooner
//
//	read starts, Enqueue at 17:30 commits, read finds 18:00   → at 17:30
//	read starts, Enqueue at 19:00 commits, read finds 18:00   → at 18:00
func (a *alarm) set(read *alarmRead, next int64) {
	a.mu.Lock()
	defer a.mu.Unlock()
	delete(a.reads, read)
	if next == 0 {
		next = math.MaxInt64
	}
	a.at = min(next, read.lowered)
}

// forget ends a read that found no next time: the alarm keeps ringing
func (a *alarm) forget(read *alarmRead) {
	a.mu.Lock()
	defer a.mu.Unlock()
	delete(a.reads, read)
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
