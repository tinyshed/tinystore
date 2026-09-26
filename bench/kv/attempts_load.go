package main

import (
	"context"
	"fmt"
	"math"
	"strconv"
	"sync/atomic"
	"time"
)

// the addresses that try the population's emails
const attackers = 16

// attemptsLoad is the attempt limits case under load. Nine sign-ins in ten come
// from an address of a population of keys and try one of keys emails, one in
// ten from one of sixteen addresses trying those emails; in the flood every
// sign-in comes from an address never seen before. A sign-in past 20 attempts
// by its address or 5 by its email is refused
type attemptsLoad struct {
	store     attempts
	keys      int
	flood     bool
	now       func() time.Time
	strangers atomic.Int64 // the addresses the flood has used
	byAddress []tracked    // the attackers', then the population's
	byEmail   []tracked
}

func loadAttempts(ctx context.Context, b *backend, keys int) (workload, error) {
	return newAttemptsLoad(ctx, b, keys, false)
}

func loadFlood(ctx context.Context, b *backend, keys int) (workload, error) {
	return newAttemptsLoad(ctx, b, keys, true)
}

func newAttemptsLoad(ctx context.Context, b *backend, keys int, flood bool) (workload, error) {
	store, err := openAttempts(ctx, b)
	if err != nil {
		return nil, err
	}
	return &attemptsLoad{
		store: store, keys: keys, flood: flood, now: b.now,
		byAddress: make([]tracked, attackers+keys), byEmail: make([]tracked, keys),
	}, nil
}

// prepare leaves keys counters of earlier sign-ins, half by address and half
// by email, in windows begun over the last ten minutes; the phase counts
// other addresses and emails, so that its counts start from zero
func (l *attemptsLoad) prepare(ctx context.Context) error {
	now := l.now()
	return inBatches(l.keys, func(from, to int) error {
		batch := make([]storedCount, 0, to-from)
		for i := int64(from); i < int64(to); i++ {
			batch = append(batch, earlierCount(i, now))
		}
		return l.store.preload(ctx, batch)
	})
}

func earlierCount(i int64, now time.Time) storedCount {
	expires := now.Add(attemptWindow - spread(seedCountAges, i, 10*time.Minute))
	count := storedCount{scope: "ip", subject: address(172, i), attempts: 1 + i%5, expires: expires}
	if i%2 == 1 {
		count.scope, count.subject = "email", "earlier"+strconv.FormatInt(i, 10)+"@example.com"
	}
	return count
}

// address is the n-th IPv4 address from first.0.0.0 on
//
//	address(10, 258) → 10.0.1.2
func address(first, n int64) string {
	return fmt.Sprintf("%d.%d.%d.%d", first+n>>24, n>>16&255, n>>8&255, n&255)
}

func emailOf(i int) string {
	return "user" + strconv.Itoa(i) + "@example.com"
}

func (l *attemptsLoad) requests(worker, _ int) (request, error) {
	random := seeded(seedAttemptRequests, int64(worker))
	return func(ctx context.Context) (step, error) {
		email := random.IntN(l.keys)
		if l.flood {
			return l.signInFromStranger(ctx, email)
		}
		from := attackers + random.IntN(l.keys)
		if random.IntN(10) == 0 {
			from = random.IntN(attackers)
		}
		return l.signInFrom(ctx, from, email)
	}, nil
}

// signInFrom counts a sign-in from an address of byAddress
func (l *attemptsLoad) signInFrom(ctx context.Context, from, email int) (step, error) {
	byIP, byEmail, err := l.store.attempt(ctx, knownAddress(from), emailOf(email))
	if err != nil {
		return step{"sign_in", ""}, err
	}
	l.byAddress[from].returned(byIP)
	l.byEmail[email].returned(byEmail)
	return step{"sign_in", verdict(byIP, byEmail)}, nil
}

// signInFromStranger counts a sign-in from an address never seen, whose count
// is one
func (l *attemptsLoad) signInFromStranger(ctx context.Context, email int) (step, error) {
	byIP, byEmail, err := l.store.attempt(ctx, address(100, l.strangers.Add(1)), emailOf(email))
	switch {
	case err != nil:
		return step{"sign_in", ""}, err
	case byIP != 1:
		return step{"sign_in", ""}, violation("miscounted")
	}
	l.byEmail[email].returned(byEmail)
	return step{"sign_in", verdict(byIP, byEmail)}, nil
}

// knownAddress is an attacker's address, or one of the population's
func knownAddress(from int) string {
	if from < attackers {
		return address(203, int64(from))
	}
	return address(10, int64(from-attackers))
}

func verdict(byIP, byEmail int64) string {
	switch {
	case byIP > ipLimit:
		return "refused_by_address"
	case byEmail > emailLimit:
		return "refused_by_email"
	}
	return "allowed"
}

// verify finds a counter whose attempts returned fewer counts than there were
// attempts: two given one count, and a limit passed unnoticed. It is exact
// while a phase is shorter than a window, since the phase's windows begin in it
func (l *attemptsLoad) verify(context.Context) error {
	miscounted := 0
	for _, counters := range [][]tracked{l.byAddress, l.byEmail} {
		for i := range counters {
			if counters[i].added.Load() != counters[i].highest.Load() {
				miscounted++
			}
		}
	}
	if miscounted > 0 {
		return fmt.Errorf("%d counters returned fewer counts than they had attempts", miscounted)
	}
	return nil
}

// tracked is what the harness saw of one counter: the attempts it added, and
// the highest count they returned, which equals them when no attempt was lost
type tracked struct {
	added, highest atomic.Int64
}

func (c *tracked) returned(count int64) {
	c.added.Add(1)
	for {
		seen := c.highest.Load()
		if count <= seen || c.highest.CompareAndSwap(seen, count) {
			return
		}
	}
}

// checkAttempts holds an implementation to the case: a window counts from its
// first attempt and starts again from zero once it ends, and a count past the
// int64 range is refused
func checkAttempts(ctx context.Context, b *backend, clock *fakeClock) error {
	store, err := openAttempts(ctx, b)
	if err != nil {
		return err
	}
	var t transcript
	attempt := func() {
		byIP, byEmail, attemptErr := store.attempt(ctx, "192.0.2.1", "someone@example.com")
		t.note(fmt.Sprint(byIP, byEmail), attemptErr)
	}

	for range 6 {
		attempt()
	}
	clock.advance(attemptWindow - time.Minute)
	attempt()
	clock.advance(2 * time.Minute)
	attempt()
	t.note(store.add(ctx, "ip", "192.0.2.2", math.MaxInt64))
	t.note(store.add(ctx, "ip", "192.0.2.2", 1))

	return t.differs("1 1", "2 2", "3 3", "4 4", "5 5", "6 6", "7 7", "1 1", "9223372036854775807", "error: limit")
}
