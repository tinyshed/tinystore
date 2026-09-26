package main

import (
	"context"
	"crypto/sha256"
	"math/rand/v2"
	"sync/atomic"
	"time"
)

// the codes a worker asked for and has not clicked; past them it leaves the
// oldest, as a user leaves a code unclicked
const pendingCodes = 64

// linksLoad is the sign-in link case under load. Of a hundred requests 50 ask
// for a code, 40 click the oldest code their worker asked for and 10 click one
// again, as a second click or a mail scanner does: half of them a code asked
// for before the phase, half one of the last 1024 asked for
type linksLoad struct {
	codes  signInCodes
	keys   int
	now    func() time.Time
	issued atomic.Int64 // the codes asked for, before the phase and in it
	taken  *once
}

func loadLinks(ctx context.Context, b *backend, keys int) (workload, error) {
	codes, err := openCodes(ctx, b)
	if err != nil {
		return nil, err
	}
	l := &linksLoad{codes: codes, keys: keys, now: b.now, taken: newOnce(keys)}
	l.issued.Store(int64(keys))
	return l, nil
}

// prepare asks for keys codes over the last ten minutes, each working five to
// fifteen minutes more
func (l *linksLoad) prepare(ctx context.Context) error {
	now := l.now()
	return inBatches(l.keys, func(from, to int) error {
		batch := make([]storedCode, 0, to-from)
		for i := int64(from); i < int64(to); i++ {
			expires := now.Add(codeTerm - spread(seedCodeAges, i, 10*time.Minute))
			batch = append(batch, storedCode{digest: codeDigest(i), user: codeUser(i), expires: expires})
		}
		return l.codes.preload(ctx, batch)
	})
}

func textDigest(text string) []byte {
	digest := sha256.Sum256([]byte(text))
	return digest[:]
}

func codeDigest(code int64) []byte {
	return textDigest(token(seedCodes, code))
}

// codeUser is the user code signs in
func codeUser(code int64) int64 {
	return code / 2
}

func (l *linksLoad) requests(worker, _ int) (request, error) {
	random := seeded(seedLinkRequests, int64(worker))
	var pending []int64 // the worker's codes, oldest first
	return func(ctx context.Context) (step, error) {
		switch pick := random.IntN(100); {
		case pick >= 90:
			return l.click(ctx, "click_again", l.clickedAgain(random))
		case pick >= 50 && len(pending) > 0:
			code := pending[0]
			pending = pending[1:]
			return l.click(ctx, "click", code)
		}
		return l.ask(ctx, &pending)
	}, nil
}

// ask asks for a new code and keeps it for its worker to click
func (l *linksLoad) ask(ctx context.Context, pending *[]int64) (step, error) {
	code := l.issued.Add(1) - 1
	if err := l.codes.issue(ctx, codeDigest(code), codeUser(code)); err != nil {
		return step{"ask", ""}, err
	}
	if len(*pending) == pendingCodes {
		*pending = (*pending)[1:]
	}
	*pending = append(*pending, code)
	return step{"ask", "asked"}, nil
}

// click takes a code; signing in someone else, or signing in twice with one
// code, breaks the case
func (l *linksLoad) click(ctx context.Context, op string, code int64) (step, error) {
	user, found, err := l.codes.take(ctx, codeDigest(code))
	switch {
	case err != nil:
		return step{op, ""}, err
	case !found:
		return step{op, "missing"}, nil
	case user != codeUser(code):
		return step{op, ""}, violation("wrong_user")
	case !l.taken.mark(code):
		return step{op, ""}, violation("taken_twice")
	}
	return step{op, "signed_in"}, nil
}

// clickedAgain is a code clicked once more: half the time one asked for before
// the phase, half one of the last 1024, which its own worker may be clicking
// at the same moment
func (l *linksLoad) clickedAgain(random *rand.Rand) int64 {
	if random.IntN(2) == 0 {
		return random.Int64N(int64(l.keys))
	}
	issued := l.issued.Load()
	return issued - 1 - random.Int64N(min(1024, issued))
}

func (l *linksLoad) verify(context.Context) error {
	return nil
}

// checkLinks holds an implementation to the case: a code signs its user in
// once, and only while its fifteen minutes last
func checkLinks(ctx context.Context, b *backend, clock *fakeClock) error {
	codes, err := openCodes(ctx, b)
	if err != nil {
		return err
	}
	var t transcript
	take := func(code string) {
		user, found, takeErr := codes.take(ctx, textDigest(code))
		if !found {
			t.note("missing", takeErr)
			return
		}
		t.note(user, takeErr)
	}

	t.note("ok", codes.issue(ctx, textDigest("a"), 7))
	take("a")
	take("a")
	t.note("ok", codes.issue(ctx, textDigest("b"), 8))
	clock.advance(codeTerm + time.Minute)
	take("b")
	t.note("ok", codes.issue(ctx, textDigest("c"), 9))
	clock.advance(codeTerm - time.Minute)
	take("c")

	return t.differs("ok", "7", "missing", "ok", "missing", "ok", "9")
}
