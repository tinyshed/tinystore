package main

import (
	"context"
	"strings"
	"time"
)

// the operations the mechanics round timed on kv's prototype, one to a stage
const (
	opWrite = "write"
	opHit   = "hit"
	opMiss  = "miss"
)

// opsDevice makes a session of about the mechanics round's size, whose
// prototype wrote 64-byte values
var opsDevice = strings.Repeat("d", 64)

// opsLoad is one operation of the sessions case alone, as the mechanics round
// measured its prototype: a new session written, or a stored one read and
// found, or a token read and missed. Every session was renewed just before the
// phase, so that no read in it renews one.
type opsLoad struct {
	store sessions
	keys  int
	now   func() time.Time
	op    string
}

func loadWrites(ctx context.Context, b *backend, keys int) (workload, error) {
	return openOps(ctx, b, keys, opWrite)
}

func loadHits(ctx context.Context, b *backend, keys int) (workload, error) {
	return openOps(ctx, b, keys, opHit)
}

func loadMisses(ctx context.Context, b *backend, keys int) (workload, error) {
	return openOps(ctx, b, keys, opMiss)
}

func openOps(ctx context.Context, b *backend, keys int, op string) (workload, error) {
	store, err := openSessions(ctx, b)
	if err != nil {
		return nil, err
	}
	return &opsLoad{store: store, keys: keys, now: b.now, op: op}, nil
}

// prepare signs in keys sessions in a scattered order, every one just now
func (l *opsLoad) prepare(ctx context.Context) error {
	now, order := l.now(), newScatter(l.keys)
	return inBatches(l.keys, func(from, to int) error {
		batch := make([]storedSession, 0, to-from)
		for n := from; n < to; n++ {
			i := int64(order.at(n))
			batch = append(batch, storedSession{
				user: i / sessionsPerUser, token: token(seedTokens, i),
				session: session{Device: opsDevice, Since: now.UnixMilli()}, expires: now.Add(sessionTerm),
			})
		}
		return l.store.preload(ctx, batch)
	})
}

func (l *opsLoad) requests(worker, _ int) (request, error) {
	random := seeded(seedOpRequests, int64(worker))
	return func(ctx context.Context) (step, error) {
		switch l.op {
		case opWrite:
			s := session{Device: opsDevice, Since: l.now().UnixMilli()}
			return step{opWrite, "written"}, l.store.signIn(ctx, random.Int64N(int64(l.keys)), randomToken(random), s)
		case opHit:
			i := random.Int64N(int64(l.keys))
			found, _, err := l.store.read(ctx, i/sessionsPerUser, token(seedTokens, i))
			return step{opHit, foundOrMissing(found)}, err
		}
		found, _, err := l.store.read(ctx, random.Int64N(int64(l.keys)), randomToken(random))
		return step{opMiss, foundOrMissing(found)}, err
	}, nil
}

func foundOrMissing(found bool) string {
	if found {
		return "found"
	}
	return "missing"
}

func (l *opsLoad) verify(context.Context) error {
	return nil
}
