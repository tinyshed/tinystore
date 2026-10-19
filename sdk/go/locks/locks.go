// Package locks is locks and semaphores, as docs/guides/locks/README.md has them: a
// key that one holder at a time, or a semaphore's limit, may hold while a
// function runs, across every program of a store. The store keeps them in
// its process's memory; a holder here renews its lease at a third of its ttl
// from this program, so that a program that froze stops renewing and loses
// the key.
//
//	invoices, err := locks.OpenLock(store, "invoices", locks.TTL(10*time.Second))
//	err = invoices.Hold(ctx, invoice.ID, func(ctx context.Context, lease *locks.Lease) error {
//		return charge(ctx, invoice, lease.Fence) // ctx ends when the lease is lost
//	})
package locks

import (
	"context"
	"crypto/rand"
	"fmt"
	"math/big"
	"strconv"
	"time"

	"tinyshed.org/tinystore"
	"tinyshed.org/tinystore/internal/core"
	"tinyshed.org/tinystore/internal/wire"
)

const defaultWait = 10 * time.Second

// Fence grows with every lease the store gives a lock, 128 bits: hand it to
// whatever must refuse a late holder, which keeps the greatest it saw.
type Fence struct{ hi, lo uint64 }

// Less says f came before other.
func (f Fence) Less(other Fence) bool { return f.hi < other.hi || f.hi == other.hi && f.lo < other.lo }

// String is the fence in decimal, as the store writes it.
func (f Fence) String() string {
	n := new(big.Int).Lsh(new(big.Int).SetUint64(f.hi), 64)
	return n.Or(n, new(big.Int).SetUint64(f.lo)).String()
}

func fenceOf(text string) Fence {
	n, ok := new(big.Int).SetString(text, 10)
	if !ok {
		return Fence{}
	}
	lo := new(big.Int).And(n, new(big.Int).SetUint64(math64))
	return Fence{hi: new(big.Int).Rsh(n, 64).Uint64(), lo: lo.Uint64()}
}

const math64 = 1<<64 - 1

// Lease is a holding of a key: a lock's Fence, none for a semaphore's.
type Lease struct {
	Fence Fence
}

// Option is how a lock or a semaphore opens.
type Option func(*keys)

// TTL is how long a lease lasts from its last renewal, 1 s to 1 hour, 10 s
// unless given; this program renews it at a third of it while the function runs.
func TTL(d time.Duration) Option { return func(k *keys) { k.ttl = d } }

// MaxHold is how long a lease lasts from its grant, however it is renewed:
// past it the next in line holds the key, and the holder's ctx ends.
func MaxHold(d time.Duration) Option { return func(k *keys) { k.maxHold = &d } }

// Limit is how many may hold a semaphore's key at once, 1 to 1000.
func Limit(n int) Option { return func(k *keys) { k.limit = n } }

// HoldOption is how a hold waits.
type HoldOption func(*time.Duration)

// Wait is how long a hold waits in line for its key, 10 s unless given: a
// key still held past it is ErrInUse.
func Wait(d time.Duration) HoldOption { return func(w *time.Duration) { *w = d } }

// keys is what a lock and a semaphore share: asking for a key, and the lease once given.
type keys struct {
	link    *core.Link
	store   *core.Store
	kind    string
	name    string
	limit   int
	ttl     time.Duration
	maxHold *time.Duration
}

func open(store *tinystore.Store, kind, name string, opts []Option) (*keys, error) {
	k := &keys{store: store.CoreHome().Store, kind: kind, name: name, limit: 1, ttl: 10 * time.Second}
	k.link = k.store.Link
	for _, opt := range opts {
		opt(k)
	}
	if err := core.CheckName(name, kind); err != nil {
		return nil, err
	}
	if kind == "semaphore" && (k.limit < 1 || k.limit > 1000) {
		return nil, core.Errorf(core.Invalid, "semaphore %s: a limit of %d, not 1 to 1000", name, k.limit)
	}
	if _, err := core.Millis(k.ttl, kind+" "+name+": its ttl"); err != nil {
		return nil, err
	}
	return k, nil
}

// Lock is keys held one holder at a time.
type Lock struct{ keys *keys }

// OpenLock opens a lock: keys held one holder at a time, across every program of the store.
func OpenLock(store *tinystore.Store, name string, opts ...Option) (*Lock, error) {
	k, err := open(store, "lock", name, opts)
	if err != nil {
		return nil, err
	}
	return &Lock{k}, nil
}

// Hold waits for key in line, holds it while fn runs and lets it go. A key
// still held past the wait is ErrInUse; a lease lost while fn ran ends its
// ctx, and Hold is ErrLost though fn succeeded, since another holder may
// have run beside it.
func (l *Lock) Hold(ctx context.Context, key any, fn func(ctx context.Context, lease *Lease) error, opts ...HoldOption) error {
	wait := defaultWait
	for _, opt := range opts {
		opt(&wait)
	}
	_, err := l.keys.hold(ctx, key, wait, fn)
	return err
}

// TryHold holds key while fn runs if nobody holds it, and says whether it did.
func (l *Lock) TryHold(ctx context.Context, key any, fn func(ctx context.Context, lease *Lease) error) (bool, error) {
	return l.keys.hold(ctx, key, 0, fn)
}

// Semaphore is keys held by up to their limit at a time.
type Semaphore struct{ keys *keys }

// OpenSemaphore opens a semaphore: each key held by up to Limit holders at once.
func OpenSemaphore(store *tinystore.Store, name string, opts ...Option) (*Semaphore, error) {
	k, err := open(store, "semaphore", name, opts)
	if err != nil {
		return nil, err
	}
	return &Semaphore{k}, nil
}

// Hold waits for one of key's permits in line and holds it while fn runs, as a lock's Hold does.
func (s *Semaphore) Hold(ctx context.Context, key any, fn func(ctx context.Context, lease *Lease) error, opts ...HoldOption) error {
	wait := defaultWait
	for _, opt := range opts {
		opt(&wait)
	}
	_, err := s.keys.hold(ctx, key, wait, fn)
	return err
}

// TryHold holds one of key's permits while fn runs if one is free, and says whether it did.
func (s *Semaphore) TryHold(ctx context.Context, key any, fn func(ctx context.Context, lease *Lease) error) (bool, error) {
	return s.keys.hold(ctx, key, 0, fn)
}

// holding is the keys a ctx holds or waits for, so that a hold inside its own hold is refused.
type holdingKey struct{}

func held(ctx context.Context, place string) bool {
	for at, ok := ctx.Value(holdingKey{}).(*holdingPlace); ok && at != nil; at = at.outer {
		if at.place == place {
			return true
		}
	}
	return false
}

type holdingPlace struct {
	place string
	outer *holdingPlace
}

func (k *keys) place(key string) string {
	return fmt.Sprintf("%s %s: key %s", k.kind, k.name, strconv.Quote(key))
}

func (k *keys) hold(ctx context.Context, given any, wait time.Duration, fn func(context.Context, *Lease) error) (bool, error) {
	key, err := core.KeyText(given)
	if err != nil {
		return false, err
	}
	place := k.place(key)
	if held(ctx, place) {
		return false, core.Errorf(core.Invalid, "%s: a hold of a key inside its own hold would wait for itself", place)
	}
	answer, err := k.ask(ctx, key, wait)
	if err != nil {
		return false, core.Wrap(core.KindOf(err), err, "%s", place)
	}
	if !answer.Acquired {
		if wait == 0 {
			return false, nil
		}
		return false, core.Errorf(core.InUse, "%s: held for all of the wait", place)
	}
	outer, _ := ctx.Value(holdingKey{}).(*holdingPlace)
	inner, cancel := context.WithCancelCause(context.WithValue(ctx, holdingKey{}, &holdingPlace{place, outer}))
	defer cancel(nil)
	lease := newLeasing(k, place, answer, cancel)
	defer lease.stop()
	ran := fn(inner, &Lease{Fence: fenceOf(answer.Fence)})
	if lost := lease.stop(); lost != nil {
		return true, lost
	}
	k.release(answer.Lease)
	return true, ran
}

// ask is the key once the store gives it, or that it was held for all of
// wait. A connection lost on the way asks again with the same request, which
// the store answers with the lease it gave, or with the ask's place in line.
func (k *keys) ask(ctx context.Context, key string, wait time.Duration) (wire.LocksHeld, error) {
	request := make([]byte, 16)
	_, _ = rand.Read(request)
	body := wire.LocksHold{
		Kind: k.kind, Name: k.name, Key: key, Limit: uint64(k.limit), Ttl: uint64(k.ttl / time.Millisecond),
		Wait: uint64(max(wait, 0) / time.Millisecond), Request: request,
	}
	if k.maxHold != nil {
		ms := uint64(*k.maxHold / time.Millisecond)
		body.MaxHold = &ms
	}
	var held wire.LocksHeld
	answer, err := k.link.Call(ctx, core.Reading, wire.MethodLocksHold, wire.Encode(&body))
	if err != nil {
		return held, err
	}
	if err := wire.Decode(answer, &held); err != nil {
		return held, core.Wrap(core.Protocol, err, "an answer that does not read")
	}
	return held, nil
}

// release lets the key go: a lease that ran out meanwhile lets go nothing another holds now.
func (k *keys) release(lease []byte) {
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	_, _ = k.link.Call(ctx, core.Reading, wire.MethodLocksRelease, wire.Encode(&wire.LocksLease{Lease: lease}))
}

// leasing renews a lease while its holder runs, and ends the holder's ctx
// when the lease is lost: not renewed in time, revoked at its max hold, or
// given by a store that has since closed.
type leasing struct {
	keys   *keys
	place  string
	lease  []byte
	cancel context.CancelCauseFunc
	done   chan struct{}
	ended  chan struct{}
	lost   error
}

func newLeasing(k *keys, place string, answer wire.LocksHeld, cancel context.CancelCauseFunc) *leasing {
	l := &leasing{keys: k, place: place, lease: answer.Lease, cancel: cancel, done: make(chan struct{}), ended: make(chan struct{})}
	var revoke <-chan time.Time
	if answer.RevokeIn != nil {
		revoke = time.After(time.Duration(*answer.RevokeIn) * time.Millisecond)
	}
	go l.renew(revoke)
	return l
}

func (l *leasing) renew(revoke <-chan time.Time) {
	defer close(l.ended)
	ttl := l.keys.ttl
	every := time.NewTicker(max(time.Millisecond, ttl/3))
	defer every.Stop()
	// the lease runs out at its ttl from the last renewal sent, less a margin
	margin := min(time.Second, ttl/10)
	runsOut := time.NewTimer(ttl - margin)
	defer runsOut.Stop()
	for {
		select {
		case <-l.done:
			return
		case <-revoke:
			l.lose("it was revoked at its max hold", nil)
			return
		case <-runsOut.C:
			l.lose("it was not renewed in time", nil)
			return
		case <-every.C:
			sent := time.Now()
			ctx, cancel := context.WithTimeout(context.Background(), ttl)
			_, err := l.keys.link.Call(ctx, core.Reading, wire.MethodLocksRenew,
				wire.Encode(&wire.LocksRenew{Lease: l.lease, Ttl: uint64(ttl / time.Millisecond)}))
			cancel()
			switch lost, _ := core.IsConnectionLost(err); {
			case err == nil:
				runsOut.Reset(time.Until(sent.Add(ttl - margin)))
			case core.KindOf(err) == core.Lost:
				l.lose("it ran out before it was renewed", nil)
				return
			case core.KindOf(err) == core.Closed && !lost:
				l.lose("its store closed", err)
				return
			}
			// any other failure keeps renewing until the lease's own deadline
		}
	}
}

func (l *leasing) lose(why string, cause error) {
	if cause == nil {
		cause = core.Errorf(core.Lost, "%s: the lease was lost: %s", l.place, why)
	}
	l.lost = cause
	l.cancel(cause)
}

// stop stops renewing, and says why the lease was lost, if it was.
func (l *leasing) stop() error {
	select {
	case <-l.done:
	default:
		close(l.done)
	}
	<-l.ended
	return l.lost
}
