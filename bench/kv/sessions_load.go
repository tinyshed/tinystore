package main

import (
	"context"
	"math/rand/v2"
	"time"
)

// the sessions each user signed in before the phase
const sessionsPerUser = 3

// the user agents sessions are signed in from, as a request carries them
var userAgents = []string{
	"Mozilla/5.0 (iPhone; CPU iPhone OS 18_6 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) " +
		"Version/18.6 Mobile/15E148 Safari/604.1",
	"Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) " +
		"Chrome/140.0.0.0 Safari/537.36",
	"Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) " +
		"Version/18.6 Safari/605.1.15",
	"Mozilla/5.0 (Linux; Android 15; Pixel 9) AppleWebKit/537.36 (KHTML, like Gecko) " +
		"Chrome/140.0.0.0 Mobile Safari/537.36",
}

// sessionsLoad is the sessions case under load. Of a thousand requests 950 read
// a session, 25 sign in, 20 sign out, 4 list a user's devices and 1 signs out
// everywhere; the preparation leaves 29 sessions in 30 renewed over a day ago,
// so that a first read renews one until the phase has read them
type sessionsLoad struct {
	store sessions
	keys  int
	users int
	now   func() time.Time
}

func loadSessions(ctx context.Context, b *backend, keys int) (workload, error) {
	store, err := openSessions(ctx, b)
	if err != nil {
		return nil, err
	}
	return &sessionsLoad{store: store, keys: keys, users: max(keys/sessionsPerUser, 1), now: b.now}, nil
}

// prepare signs in three sessions a user in a scattered order, as users sign
// in, each last renewed at an age spread over the term
func (l *sessionsLoad) prepare(ctx context.Context) error {
	now, order := l.now(), newScatter(l.keys)
	return inBatches(l.keys, func(from, to int) error {
		batch := make([]storedSession, 0, to-from)
		for n := from; n < to; n++ {
			batch = append(batch, preparedSession(int64(order.at(n)), now))
		}
		return l.store.preload(ctx, batch)
	})
}

// preparedSession is session i, user i/3's
func preparedSession(i int64, now time.Time) storedSession {
	renewed := now.Add(-spread(seedSessionAges, i, sessionTerm))
	return storedSession{
		user: i / sessionsPerUser, token: token(seedTokens, i),
		session: session{Device: userAgents[i%int64(len(userAgents))], Since: renewed.UnixMilli()},
		expires: renewed.Add(sessionTerm),
	}
}

func (l *sessionsLoad) requests(worker, _ int) (request, error) {
	random := seeded(seedSessionRequests, int64(worker))
	return func(ctx context.Context) (step, error) {
		switch pick := random.IntN(1000); {
		case pick < 950:
			return l.read(ctx, random)
		case pick < 975:
			return l.signIn(ctx, random)
		case pick < 995:
			return l.signOut(ctx, random)
		case pick < 999:
			return l.list(ctx, random)
		}
		return l.signOutEverywhere(ctx, random)
	}, nil
}

func (l *sessionsLoad) read(ctx context.Context, random *rand.Rand) (step, error) {
	i := random.Int64N(int64(l.keys))
	found, renewed, err := l.store.read(ctx, i/sessionsPerUser, token(seedTokens, i))
	switch {
	case renewed:
		return step{"read", "renewed"}, err
	case found:
		return step{"read", "found"}, err
	}
	return step{"read", "missing"}, err
}

func (l *sessionsLoad) signIn(ctx context.Context, random *rand.Rand) (step, error) {
	user := random.Int64N(int64(l.users))
	s := session{Device: userAgents[random.IntN(len(userAgents))], Since: l.now().UnixMilli()}
	return step{"sign_in", "signed_in"}, l.store.signIn(ctx, user, randomToken(random), s)
}

func (l *sessionsLoad) signOut(ctx context.Context, random *rand.Rand) (step, error) {
	i := random.Int64N(int64(l.keys))
	return step{"sign_out", "signed_out"}, l.store.signOut(ctx, i/sessionsPerUser, token(seedTokens, i))
}

func (l *sessionsLoad) list(ctx context.Context, random *rand.Rand) (step, error) {
	_, err := l.store.devices(ctx, random.Int64N(int64(l.users)))
	return step{"list", "listed"}, err
}

func (l *sessionsLoad) signOutEverywhere(ctx context.Context, random *rand.Rand) (step, error) {
	err := l.store.signOutEverywhere(ctx, random.Int64N(int64(l.users)))
	return step{"sign_out_everywhere", "signed_out"}, err
}

func (l *sessionsLoad) verify(context.Context) error {
	return nil
}

// checkSessions holds an implementation to the case: a read renews a session
// last renewed over a day ago, which then outlives its first term, and one not
// read again expires with it; a sign-out ends one session, everywhere every one
func checkSessions(ctx context.Context, b *backend, clock *fakeClock) error {
	store, err := openSessions(ctx, b)
	if err != nil {
		return err
	}
	var t transcript
	signIn := func(user int64, token string) {
		t.note("ok", store.signIn(ctx, user, token, session{Device: "phone", Since: clock.Now().UnixMilli()}))
	}
	read := func(user int64, token string) {
		found, _, readErr := store.read(ctx, user, token)
		t.note(found, readErr)
	}

	signIn(1, "a")
	signIn(1, "b")
	signIn(2, "c")
	read(1, "a")
	t.note(store.devices(ctx, 1))
	clock.advance(2 * day)
	read(1, "a")
	t.note("ok", b.maintain(ctx))
	clock.advance(29 * day)
	read(1, "a")
	read(1, "b")
	t.note(store.devices(ctx, 1))
	t.note("ok", store.signOut(ctx, 1, "a"))
	read(1, "a")
	signIn(1, "d")
	signIn(2, "e")
	t.note("ok", store.signOutEverywhere(ctx, 1))
	t.note(store.devices(ctx, 1))
	read(2, "e")

	return t.differs("ok", "ok", "ok", "true", "2", "true", "ok", "true", "false", "1", "ok", "false",
		"ok", "ok", "ok", "0", "true")
}
