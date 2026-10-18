package kv

import (
	"context"
	"time"

	"tinyshed.org/tinystore"
	"tinyshed.org/tinystore/internal/core"
	"tinyshed.org/tinystore/internal/wire"
)

// Counters are numbers by key that only add up: 0 for one never added to, or expired.
type Counters struct{ parts }

// OpenCounters opens counters by key on the store.
//
//	attempts, err := kv.OpenCounters(store, "login-attempts", kv.TTL(15*time.Minute))
func OpenCounters(home tinystore.Home, name string, opts ...CountersOption) (*Counters, error) {
	var o countersOptions
	for _, opt := range opts {
		opt.counters(&o)
	}
	if err := core.CheckName(name, "counters"); err != nil {
		return nil, err
	}
	ttl, err := millis(o.ttl, "kv counters "+name+": ttl")
	if err != nil {
		return nil, err
	}
	flush, err := millis(o.flushEvery, "kv counters "+name+": flush every")
	if err != nil {
		return nil, err
	}
	of := homeOf(home)
	open := &core.Opener{Method: wire.MethodKvCountersOpen, Body: wire.Encode(&wire.KvCountersOpen{Name: name, Ttl: ttl, FlushEvery: flush})}
	return &Counters{parts{store: of.Store, name: name, opener: open, via: core.ViaLink{Link: of.Store.Link}}}, nil
}

// Under is the counters of a branch, each key apart from the same key elsewhere.
func (c *Counters) Under(owners ...any) *Counters { return &Counters{c.below(owners)} }

// In is the counters inside the store's transaction: an add commits with its writes.
func (c *Counters) In(tx tinystore.InTx) *Counters { return &Counters{c.in(tx, "kv counters "+c.name)} }

// Add adds n and gives the new count; inside a transaction the count is
// known once it committed, and Add gives 0.
func (c *Counters) Add(ctx context.Context, key string, n int64) (int64, error) {
	call := wire.KvCall{Key: key, N: &n}
	if c.tx != nil || c.err != nil {
		if c.err != nil {
			return 0, c.err
		}
		return 0, c.tx.Write(wire.MethodKvCountersAdd, c.part(), call, nil)
	}
	return c.count(ctx, wire.MethodKvCountersAdd, call, core.Writing)
}

// Get is the key's count.
func (c *Counters) Get(ctx context.Context, key string) (int64, error) {
	return c.count(ctx, wire.MethodKvCountersGet, wire.KvCall{Key: key}, core.Reading)
}

// Delete removes the counter, and says whether it was there.
func (c *Counters) Delete(ctx context.Context, key string) (bool, error) {
	answer, err := c.call(ctx, wire.MethodKvCountersDelete, wire.KvCall{Key: key}, core.Writing)
	return foundOf(answer, err)
}

// Clear removes this branch's counters and every branch's under it.
func (c *Counters) Clear(ctx context.Context) error {
	if c.err != nil {
		return c.err
	}
	_, err := c.via.Call(ctx, c.opener, wire.MethodKvCountersClear, func(handle uint64) []byte {
		return wire.Encode(&wire.KvBranch{Handle: handle, Under: c.under})
	}, core.Writing)
	return err
}

func (c *Counters) count(ctx context.Context, method uint16, call wire.KvCall, idempotence core.Idempotence) (int64, error) {
	answer, err := c.call(ctx, method, call, idempotence)
	if err != nil {
		return 0, err
	}
	var count wire.KvCount
	err = decode(answer, &count)
	return count.Value, err
}

// call calls a method of a handle's branch on the link's connection, the
// handle opened there first.
func (p parts) call(ctx context.Context, method uint16, call wire.KvCall, idempotence core.Idempotence) ([]byte, error) {
	if p.err != nil {
		return nil, p.err
	}
	return p.via.Call(ctx, p.opener, method, func(handle uint64) []byte {
		call.Handle, call.Under = handle, p.under
		return wire.Encode(&call)
	}, idempotence)
}

// Allowance is what a rate limit or a quota answers a request.
type Allowance struct {
	OK bool
	// Left is how many more would pass now.
	Left uint64
	// RetryAt is when a refused request may try again; the zero time when it passed.
	RetryAt time.Time
	// Windows are a quota's windows by name; none for a rate limit.
	Windows map[string]WindowUse
}

// WindowUse is one window of a quota as a key stands in it.
type WindowUse struct {
	Used, Limit, Left uint64
	// ResetsAt is when the window resets; the zero time for one not started.
	ResetsAt time.Time
}

func allowanceOf(answer []byte, err error) (Allowance, error) {
	if err != nil {
		return Allowance{}, err
	}
	var a wire.KvAllowance
	if err := decode(answer, &a); err != nil {
		return Allowance{}, err
	}
	out := Allowance{OK: a.Ok, Left: a.Left, RetryAt: core.MomentOf(a.RetryAt)}
	if len(a.Windows) > 0 {
		out.Windows = map[string]WindowUse{}
		for _, w := range a.Windows {
			out.Windows[w.Name] = WindowUse{w.Used, w.Limit, w.Left, core.MomentOf(w.ResetsAt)}
		}
	}
	return out, nil
}

// RateLimit is a smooth rate of requests a key, the generic cell rate
// algorithm with its times in the core's memory.
type RateLimit struct{ parts }

// OpenRateLimit opens a rate limit: kv.OpenRateLimit(store, "api", kv.Rate(100, time.Second), kv.Burst(20)).
func OpenRateLimit(home tinystore.Home, name string, opts ...RateLimitOption) (*RateLimit, error) {
	var o rateLimitOptions
	for _, opt := range opts {
		opt.rateLimit(&o)
	}
	if err := core.CheckName(name, "rate limit"); err != nil {
		return nil, err
	}
	per, err := core.Millis(o.per, "kv rate limit "+name+": its rate's span")
	if err != nil {
		return nil, err
	}
	if o.rate < 1 || per < 1 {
		return nil, core.Errorf(core.Invalid, "kv rate limit %s: a rate of %d a %s; give kv.Rate", name, o.rate, o.per)
	}
	if o.burst != nil && *o.burst < 1 {
		return nil, core.Errorf(core.Invalid, "kv rate limit %s: a burst of %d", name, *o.burst)
	}
	of := homeOf(home)
	open := &core.Opener{Method: wire.MethodKvRateLimitOpen, Body: wire.Encode(&wire.KvRateLimitOpen{Name: name, Rate: o.rate, Per: per, Burst: o.burst})}
	return &RateLimit{parts{store: of.Store, name: name, opener: open, via: core.ViaLink{Link: of.Store.Link}}}, nil
}

// Under is the rate limit of a branch, each key apart from the same key elsewhere.
func (r *RateLimit) Under(owners ...any) *RateLimit { return &RateLimit{r.below(owners)} }

// Allow asks for one request of key, or n: all pass or none; more than the
// burst is Invalid.
func (r *RateLimit) Allow(ctx context.Context, key string, n ...int64) (Allowance, error) {
	return allowanceOf(r.call(ctx, wire.MethodKvAllow, requests(key, n), core.Writing))
}

// Peek is the answer one request would get, using nothing.
func (r *RateLimit) Peek(ctx context.Context, key string) (Allowance, error) {
	return allowanceOf(r.call(ctx, wire.MethodKvPeek, wire.KvCall{Key: key}, core.Reading))
}

// Reset forgets key, so that its next request starts afresh.
func (r *RateLimit) Reset(ctx context.Context, key string) error {
	_, err := r.call(ctx, wire.MethodKvReset, wire.KvCall{Key: key}, core.Writing)
	return err
}

func requests(key string, n []int64) wire.KvCall {
	call := wire.KvCall{Key: key}
	if len(n) > 0 && n[0] != 1 {
		call.N = &n[0]
	}
	return call
}

// Quota counts uses by key in named windows that reset, all of them or none
// in one durable write.
type Quota struct{ parts }

// OpenQuota opens a quota of windows, each counted from a key's first use:
// kv.OpenQuota(store, "ai", kv.Window("daily", 100, 24*time.Hour)).
func OpenQuota(home tinystore.Home, name string, opts ...QuotaOption) (*Quota, error) {
	var o quotaOptions
	for _, opt := range opts {
		opt.quota(&o)
	}
	if err := core.CheckName(name, "quota"); err != nil {
		return nil, err
	}
	message := wire.KvQuotaOpen{Name: name}
	for _, w := range o.windows {
		per, err := core.Millis(w.per, "kv quota "+name+": window "+w.name)
		if err != nil {
			return nil, err
		}
		message.Windows = append(message.Windows, wire.KvWindow{Name: w.name, Limit: w.limit, Per: per})
	}
	of := homeOf(home)
	open := &core.Opener{Method: wire.MethodKvQuotaOpen, Body: wire.Encode(&message)}
	return &Quota{parts{store: of.Store, name: name, opener: open, via: core.ViaLink{Link: of.Store.Link}}}, nil
}

// Under is the quota of a branch, each key apart from the same key elsewhere.
func (q *Quota) Under(owners ...any) *Quota { return &Quota{q.below(owners)} }

// Allow uses one of key's every window, or n, or none when one has no room;
// more than a window's limit is Invalid.
func (q *Quota) Allow(ctx context.Context, key string, n ...int64) (Allowance, error) {
	return allowanceOf(q.call(ctx, wire.MethodKvAllow, requests(key, n), core.Writing))
}

// Peek is how key stands in every window, using nothing.
func (q *Quota) Peek(ctx context.Context, key string) (Allowance, error) {
	return allowanceOf(q.call(ctx, wire.MethodKvPeek, wire.KvCall{Key: key}, core.Reading))
}

// Reset forgets key's uses in every window.
func (q *Quota) Reset(ctx context.Context, key string) error {
	_, err := q.call(ctx, wire.MethodKvReset, wire.KvCall{Key: key}, core.Writing)
	return err
}

// Refund gives one use back to every window of key, or n.
func (q *Quota) Refund(ctx context.Context, key string, n ...int64) error {
	_, err := q.call(ctx, wire.MethodKvRefund, requests(key, n), core.Writing)
	return err
}
