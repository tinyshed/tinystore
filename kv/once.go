package kv

import (
	"cmp"
	"context"
	"fmt"
	"sync"
	"time"

	"github.com/tinyshed/tinystore"
)

// the kind of bucket that keeps the answers of a Once
const kindOnce = "once"

// how long a Once keeps an answer when DefaultTTL does not say
const keptAnswer = 24 * time.Hour

// the longest keeping an answer may take once its function has returned,
// whatever the context of the Run that ran it
const keepAnswer = 10 * time.Second

// Once runs a function at most once a key and keeps what it returns, so that
// a request sent again is answered as the first was rather than done twice: a
// charge, an import, a webhook's effect.
//
//	charges.Run(ctx, "req-7", charge) → runs charge, keeps its receipt a day
//	charges.Run(ctx, "req-7", charge) → that receipt; charge does not run
//
// One Run of a key runs its function at a time in the store's process, the
// clients of its server included: a Run of a key another Run is running waits
// for it and returns its answer. A function's error keeps nothing, so the next
// Run runs it again; an answer a program wants kept, a card declined, is a
// value rather than an error.
//
// A claim on a key lives in memory as long as its Run. A process that dies
// after a function's effect outside the store and before its answer was kept
// runs it again on the next Run, so such an effect carries the key too: a
// payment provider's own idempotency key.
type Once[V any] struct {
	answers *Bucket[V]
}

// OpenOnce opens the answers name of kv.db, creating them the first time. An
// answer lives a day unless DefaultTTL says. A name that holds anything else
// is ErrInvalid, and so is Sliding: an answer lives from when it was kept.
func OpenOnce[V any](ctx context.Context, state *Store, name string, options ...BucketOption) (*Once[V], error) {
	settings, err := settle[V](options)
	if err == nil && settings.sliding > 0 {
		err = fmt.Errorf("%w: an answer lives from when it is kept, so Sliding is a bucket's", tinystore.ErrInvalid)
	}
	if err != nil {
		return nil, fmt.Errorf("kv: once %q: %w", name, err)
	}

	id, err := state.claimBucket(ctx, name, kindOnce)
	if err != nil {
		return nil, err
	}

	opened := branch{state: state, id: id, name: name, ttl: cmp.Or(settings.ttl, keptAnswer)}
	return &Once[V]{answers: &Bucket[V]{branch: opened, codec: settings.codec}}, nil
}

// Of is the branch of these answers that owners name, as Bucket.Of names one.
func (o *Once[V]) Of(owners ...any) *Once[V] {
	return &Once[V]{answers: o.answers.Of(owners...)}
}

// Run returns the answer kept under key, or runs fn and keeps what it returns.
//
// While another Run of the key runs its function this one waits, until ctx
// ends, and then returns the answer it kept, or, when it failed, runs fn
// itself. fn's error is returned and keeps nothing. An answer is kept even
// when ctx ends after fn returned, since its work is done; an error keeping it
// is returned beside it.
func (o *Once[V]) Run(ctx context.Context, key any, fn func(context.Context) (V, error)) (V, error) {
	c, err := o.answers.begin(key, nil)
	if err != nil {
		var zero V
		return zero, o.answers.fail(c, err)
	}
	running := onceKey{bucket: o.answers.id, path: string(c.path)}

	for {
		answer, found, err := o.answers.Get(ctx, key)
		if err != nil || found {
			return answer, err
		}
		done, mine := o.answers.state.runs.claim(running)
		if mine {
			return o.runClaimed(ctx, key, running, fn)
		}
		select {
		case <-done:
		case <-ctx.Done():
			var zero V
			return zero, ctx.Err()
		}
	}
}

// runClaimed runs fn for a key this Run has claimed, unless the Run before it
// kept an answer between the read and the claim, and keeps what fn returns.
// The claim is let go however fn ends, a panic included.
func (o *Once[V]) runClaimed(ctx context.Context, key any, running onceKey, fn func(context.Context) (V, error)) (
	V, error,
) {
	defer o.answers.state.runs.release(running)

	answer, found, err := o.answers.Get(ctx, key)
	if err != nil || found {
		return answer, err
	}
	if answer, err = fn(ctx); err != nil {
		return answer, err
	}

	keeping, cancel := context.WithTimeout(context.WithoutCancel(ctx), keepAnswer)
	defer cancel()
	return answer, o.answers.Set(keeping, key, answer)
}

// Get is the answer kept under key, and whether one is.
func (o *Once[V]) Get(ctx context.Context, key any) (V, bool, error) {
	return o.answers.Get(ctx, key)
}

// Delete forgets the answer kept under key, so that the next Run runs again.
func (o *Once[V]) Delete(ctx context.Context, key any) error {
	return o.answers.Delete(ctx, key)
}

// onceRuns are the keys whose Run is running its function in this process,
// each with the channel that Run closes when it is done
type onceRuns struct {
	mu      sync.Mutex
	running map[onceKey]chan struct{}
}

type onceKey struct {
	bucket int64
	path   string
}

// claim makes the caller the one Run of key unless another Run is, and
// returns the channel that Run closes when it is done
func (r *onceRuns) claim(key onceKey) (done <-chan struct{}, mine bool) {
	r.mu.Lock()
	defer r.mu.Unlock()
	if running, ok := r.running[key]; ok {
		return running, false
	}
	if r.running == nil {
		r.running = map[onceKey]chan struct{}{}
	}
	running := make(chan struct{})
	r.running[key] = running
	return running, true
}

// release lets go of a claim and wakes the Runs waiting for it
func (r *onceRuns) release(key onceKey) {
	r.mu.Lock()
	defer r.mu.Unlock()
	close(r.running[key])
	delete(r.running, key)
}
