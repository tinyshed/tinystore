package jobs

import (
	"math"
	"sync"
	"time"
)

// rateLog is the starts a queue's Rate counts: when claims leased jobs and how
// many, the oldest first, within the last span.
//
// A claim within a 1024th of the span after the last is counted at the later
// time, so that the log holds about 1024 entries whatever the rate. A start
// then stays in the span a little longer than it was, which never lets more
// than n through:
//
//	Rate(3, time.Second), starts at 0, 0 and 400 ms   two more from 1000 ms, a third from 1400 ms
type rateLog struct {
	mu     sync.Mutex
	n      int
	per    int64 // milliseconds
	starts []rateStarts
	used   int   // the starts the log holds
	last   int64 // the latest time it was asked about, so that a clock stepping back moves nothing
}

type rateStarts struct {
	at    int64 // unix milliseconds
	count int
}

// newRateLog is the log of a queue's Rate, nil for a queue without one
func newRateLog(p policy) *rateLog {
	if p.rate == 0 {
		return nil
	}
	return &rateLog{n: p.rate, per: p.ratePer.Milliseconds()}
}

// room is how many jobs may start at now, and when one may next when none can
func (r *rateLog) room(now int64) (int, int64) {
	if r == nil {
		return math.MaxInt, 0
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	r.forget(now)
	if left := r.n - r.used; left > 0 {
		return left, 0
	}
	return 0, r.starts[0].at + r.per
}

// take counts up to want starts at now, and says how many it counted
func (r *rateLog) take(now int64, want int) int {
	if r == nil {
		return want
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	now = r.forget(now)
	taken := min(want, r.n-r.used)
	if taken <= 0 {
		return 0
	}
	if last := len(r.starts) - 1; last >= 0 && r.starts[last].at >= now-r.per/1024 {
		r.starts[last].at, r.starts[last].count = now, r.starts[last].count+taken
	} else {
		r.starts = append(r.starts, rateStarts{at: now, count: taken})
	}
	r.used += taken
	return taken
}

// giveBack uncounts n of the starts the last take counted, which its claim
// did not lease. Claims take and give back inside the file's one writer, so
// that no take comes between.
func (r *rateLog) giveBack(n int) {
	if r == nil || n <= 0 {
		return
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	last := len(r.starts) - 1
	if last < 0 {
		return // the span has passed since, and took the starts along
	}
	n = min(n, r.starts[last].count)
	r.starts[last].count -= n
	r.used -= n
	if r.starts[last].count == 0 {
		r.starts = r.starts[:last]
	}
}

// forget drops the starts a span old at now, and answers now, or the latest
// time asked about when the clock stepped back
func (r *rateLog) forget(now int64) int64 {
	now = max(now, r.last)
	r.last = now
	gone := 0
	for gone < len(r.starts) && r.starts[gone].at <= now-r.per {
		r.used -= r.starts[gone].count
		gone++
	}
	r.starts = r.starts[gone:]
	return now
}

// rateWait is how long a Work loop the rate holds back sleeps: until next, a
// minute at most, so that a clock that jumps is met
func rateWait(now, next int64) time.Duration {
	return min(time.Duration(max(next-now, 0))*time.Millisecond, longestAlarm)
}
