package jobs

import (
	"context"
	"math/rand/v2"
	"sync/atomic"
	"testing"
	"time"
)

// the log's worked example: starts at 0, 0 and 400 ms of Rate(3, time.Second)
// let two more from 1000 ms and a third from 1400 ms
func TestARateLetsItsStartsAgainASpanLater(t *testing.T) {
	log := newRateLog(policy{rate: 3, ratePer: time.Second})
	for _, at := range []int64{0, 0, 400} {
		if taken := log.take(at, 1); taken != 1 {
			t.Fatalf("a start at %d ms was refused", at)
		}
	}
	for _, check := range []struct {
		at         int64
		room, next int64
	}{{999, 0, 1000}, {1000, 2, 0}, {1399, 2, 0}, {1400, 3, 0}} {
		if room, next := log.room(check.at); int64(room) != check.room || next != check.next {
			t.Fatalf("at %d ms the rate lets %d start, the next at %d", check.at, room, next)
		}
	}
	if room, _ := log.room(0); room != 3 {
		t.Fatalf("a clock that stepped back finds room for %d", room)
	}
}

// however claims come, and whatever they give back, no span of per holds
// more than n starts, and the log holds about 1024 entries a span
func TestARateNeverLetsMoreThanItsCountStartInASpan(t *testing.T) {
	random := rand.New(rand.NewPCG(1, 2))
	for range 200 {
		n, per := 1+random.IntN(50), int64(1+random.IntN(5000))
		log := newRateLog(policy{rate: n, ratePer: time.Duration(per) * time.Millisecond})
		var starts []int64
		now := int64(0)
		for range 2000 {
			now += int64(random.IntN(int(per)/10 + 2))
			taken := log.take(now, 1+random.IntN(n+2))
			given := random.IntN(taken + 1)
			log.giveBack(given)
			for range taken - given {
				starts = append(starts, now)
			}
		}
		for i, start := range starts {
			inSpan := 0
			for _, other := range starts[:i+1] {
				if other > start-per {
					inSpan++
				}
			}
			if inSpan > n {
				t.Fatalf("Rate(%d, %d ms) let %d start in the span to %d ms", n, per, inSpan, start)
			}
		}
		if len(log.starts) > 1026 {
			t.Fatalf("the log of Rate(%d, %d ms) holds %d entries", n, per, len(log.starts))
		}
	}
}

// a queue's rate bounds its claims across Claim and Work, and Work until idle
// returns when the rate lets none start, until the clock moves on
func TestAQueueRateBoundsItsClaims(t *testing.T) {
	queues := openTestQueues(t, t.TempDir())
	messages := openTestQueue[string](t, queues, "messages", Rate(3, time.Second))
	for range 10 {
		mustEnqueue(t, messages, "hello")
	}
	for range 3 {
		if err := mustClaim(t, messages).Ack(t.Context()); err != nil {
			t.Fatal(err)
		}
	}
	nothingDue(t, messages)
	queues.clock.advance(999 * time.Millisecond)
	nothingDue(t, messages)
	queues.clock.advance(time.Millisecond)

	var ran atomic.Int64
	work := func(context.Context, Job[string]) error {
		ran.Add(1)
		return nil
	}
	if err := messages.Work(t.Context(), work, Workers(4), UntilIdle()); err != nil || ran.Load() != 3 {
		t.Fatalf("a second later Work ran %d: %v", ran.Load(), err)
	}
	queues.clock.advance(time.Second)
	if err := messages.Work(t.Context(), work, Workers(4), UntilIdle()); err != nil || ran.Load() != 6 {
		t.Fatalf("two seconds later Work ran %d: %v", ran.Load(), err)
	}
	if room, next := messages.state.rate.room(queues.clock.Now().UnixMilli()); room != 0 ||
		next != testStart.Add(3*time.Second).UnixMilli() {
		t.Fatalf("the rate lets %d start, the next at %d", room, next)
	}
}

// a Work loop the rate holds back sleeps until the rate lets the next job
// start, and then runs it without a claim in between
func TestWorkWaitsForItsRate(t *testing.T) {
	queues := openTestQueues(t, t.TempDir())
	messages := openTestQueue[string](t, queues, "messages", Rate(1, 50*time.Millisecond))
	mustEnqueue(t, messages, "first")
	mustEnqueue(t, messages, "second")
	ran := make(chan string, 2)
	stop := working(t, messages, func(_ context.Context, job Job[string]) error {
		ran <- job.Value
		return nil
	})
	defer stop()
	if value := <-ran; value != "first" {
		t.Fatalf("the first job run is %s", value)
	}
	before := commits(t, queues)
	time.Sleep(100 * time.Millisecond)
	if written := commits(t, queues) - before; written > 2 {
		t.Fatalf("a loop the rate held back wrote %d times", written)
	}
	queues.clock.advance(50 * time.Millisecond)
	select {
	case value := <-ran:
		if value != "second" {
			t.Fatalf("the second job run is %s", value)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("the rate let the second job start, and no loop ran it")
	}
}
