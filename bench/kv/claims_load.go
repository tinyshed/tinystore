package main

import (
	"context"
	"errors"
	"math/rand/v2"
	"sync/atomic"
	"time"

	"github.com/tinyshed/tinystore"
)

// claimsLoad is the webhook claims case under load. Of a hundred deliveries 90
// bring a new event, 8 one of the last 1024 again, perhaps while its first
// delivery is being handled, and 2 one handled before the phase; one handler in
// a hundred fails and releases its claim, so that its event is delivered again
type claimsLoad struct {
	claims    webhookClaims
	keys      int
	now       func() time.Time
	delivered atomic.Int64 // the events delivered, before the phase and in it
	handled   *once
}

func loadClaims(ctx context.Context, b *backend, keys int) (workload, error) {
	claims, err := openClaims(ctx, b)
	if err != nil {
		return nil, err
	}
	l := &claimsLoad{claims: claims, keys: keys, now: b.now, handled: newOnce(keys)}
	l.delivered.Store(int64(keys))
	return l, nil
}

// prepare remembers keys events handled over the last week, the oldest an
// hour from being forgotten
func (l *claimsLoad) prepare(ctx context.Context) error {
	now := l.now()
	return inBatches(l.keys, func(from, to int) error {
		batch := make([]storedEvent, 0, to-from)
		for i := int64(from); i < int64(to); i++ {
			expires := now.Add(time.Hour + spread(seedEventAges, i, handledTerm-time.Hour))
			batch = append(batch, storedEvent{id: eventID(i), expires: expires})
		}
		return l.claims.preload(ctx, batch)
	})
}

// eventID is event i's id, spelled as a payment provider spells one
func eventID(i int64) string {
	return "evt_" + token(seedEvents, i)
}

func (l *claimsLoad) requests(worker, _ int) (request, error) {
	random := seeded(seedClaimRequests, int64(worker))
	return func(ctx context.Context) (step, error) {
		event := l.delivery(random)
		claim, err := l.claims.claim(ctx, eventID(event))
		switch {
		case err != nil:
			return step{"deliver", ""}, err
		case claim == "":
			return step{"deliver", "duplicate"}, nil
		case random.IntN(100) == 0:
			return l.release(ctx, event, claim)
		}
		return l.finish(ctx, event, claim)
	}, nil
}

// delivery is the event a delivery brings
func (l *claimsLoad) delivery(random *rand.Rand) int64 {
	switch pick := random.IntN(100); {
	case pick < 90:
		return l.delivered.Add(1) - 1
	case pick < 98:
		delivered := l.delivered.Load()
		return delivered - 1 - random.Int64N(min(1024, delivered))
	}
	return random.Int64N(int64(l.keys))
}

// finish remembers a handled event; one that was handled before, before the
// phase or in it, has been handled twice
func (l *claimsLoad) finish(ctx context.Context, event int64, claim string) (step, error) {
	err := l.claims.finish(ctx, eventID(event), claim)
	switch {
	case errors.Is(err, tinystore.ErrConflict):
		return step{"deliver", "claim_expired"}, nil
	case err != nil:
		return step{"deliver", ""}, err
	case event < int64(l.keys) || !l.handled.mark(event):
		return step{"deliver", ""}, violation("handled_twice")
	}
	return step{"deliver", "handled"}, nil
}

func (l *claimsLoad) release(ctx context.Context, event int64, claim string) (step, error) {
	err := l.claims.release(ctx, eventID(event), claim)
	switch {
	case errors.Is(err, tinystore.ErrConflict):
		return step{"deliver", "claim_expired"}, nil
	case err != nil:
		return step{"deliver", ""}, err
	}
	return step{"deliver", "released"}, nil
}

func (l *claimsLoad) verify(context.Context) error {
	return nil
}

// checkClaims holds an implementation to the case: an event is claimed once
// while it is handled and remembered a week once it is, a claim that expired
// can neither finish nor release the next one's, and a released event is
// claimed again
func checkClaims(ctx context.Context, b *backend, clock *fakeClock) error {
	claims, err := openClaims(ctx, b)
	if err != nil {
		return err
	}
	var t transcript
	claim := func(event string) string {
		version, claimErr := claims.claim(ctx, event)
		t.note(version != "", claimErr)
		return version
	}

	first := claim("evt_1")
	claim("evt_1")
	t.note("ok", claims.finish(ctx, "evt_1", first))
	claim("evt_1")
	expired := claim("evt_2")
	clock.advance(claimTerm + time.Minute)
	next := claim("evt_2")
	t.note("ok", claims.finish(ctx, "evt_2", expired))
	t.note("ok", claims.release(ctx, "evt_2", expired))
	t.note("ok", claims.finish(ctx, "evt_2", next))
	failed := claim("evt_3")
	t.note("ok", claims.release(ctx, "evt_3", failed))
	claim("evt_3")
	clock.advance(handledTerm)
	claim("evt_1")

	return t.differs("true", "false", "ok", "false", "true", "true", "error: conflict", "error: conflict", "ok",
		"true", "ok", "true", "true")
}
