// Package kv is the application's current state, as plan/api/kv.md has it:
// buckets of one value type by key, keys in branches, expiry by the store's
// clock and versions that never repeat, with counters, rate limits, quotas
// and answers kept once a key beside them. A handle opens with its first call.
//
//	sessions, err := kv.OpenBucket[Session](store, "sessions", kv.Idle(30*24*time.Hour))
//	err = sessions.Under(user.ID).Set(ctx, digest(token), session)
//	session, found, err := sessions.Under(user.ID).Get(ctx, digest(token))
package kv

import (
	"context"
	"iter"
	"time"

	"tinyshed.org/tinystore"
	"tinyshed.org/tinystore/internal/core"
	"tinyshed.org/tinystore/internal/wire"
)

// Version is what a key was when it was read: compared only for equality,
// given back to IfVersion.
type Version []byte

// Entry is a key's value with its version and expiry.
type Entry[T any] struct {
	Key     string
	Value   T
	Version Version
	// ExpiresAt is the zero time for a key that never expires.
	ExpiresAt time.Time
}

// Page is a page of a branch's keys, and where the next starts: "" on the last.
type Page[T any] struct {
	Entries []Entry[T]
	Next    string
}

// parts is what every handle of kv is: its store, its open, its branch, and
// how its calls reach the store.
type parts struct {
	store  *core.Store
	name   string
	opener *core.Opener
	under  []string
	home   core.Database
	via    core.Via
	// tx is the store's transaction the handle was taken into
	tx *core.StoreTx
	// err is what In refused, which every call answers
	err error
}

func (p parts) below(owners []any) parts {
	under := append([]string{}, p.under...)
	for _, owner := range owners {
		text, err := core.KeyText(owner)
		if err != nil && p.err == nil {
			p.err = err
		}
		under = append(under, text)
	}
	p.under = under
	return p
}

func (p parts) part() core.KvPart {
	return core.KvPart{Link: p.store.Link, Name: p.name, Opener: p.opener, Under: p.under, Home: p.home}
}

// in is the handle taken into a transaction: the store's, whose writes wait
// for its commit, or a database's, whose calls go on its stream.
func (p parts) in(tx tinystore.InTx, what string) parts {
	of := tx.CoreTx()
	switch {
	case of.Store != nil:
		file := "kv.db"
		if p.err == nil {
			p.err = of.Store.Takes(p.store.Link, p.home, file, what)
		}
		p.tx = of.Store
	case of.Database != nil:
		if p.home == nil || p.home != of.Database.Database() {
			if p.err == nil {
				p.err = core.Errorf(core.Invalid, "%s: kept outside the database, in a transaction of %s: open it from the database",
					what, of.Database.Database().Describe())
			}
		}
		p.via = of.Database.Via()
	}
	return p
}

func homeOf(home tinystore.Home) core.HomeOf { return home.CoreHome() }

// opener is the opener of a handle kept in the store's kv.db, or in a database's file.
func opener(of core.HomeOf, method uint16, body func(database *uint64) []byte) *core.Opener {
	if of.Database == nil {
		return &core.Opener{Method: method, Body: body(nil)}
	}
	db := of.Database
	return db.Keep(method, body(nil), func(ctx context.Context, c *core.Connection) ([]byte, error) {
		handle, err := db.Handle(ctx, c)
		if err != nil {
			return nil, err
		}
		return body(&handle), nil
	})
}

func viaOf(of core.HomeOf, what string) core.Via {
	if of.Database == nil {
		return core.ViaLink{Link: of.Store.Link}
	}
	return of.Database.Via(what)
}

func millis(d *time.Duration, of string) (*uint64, error) {
	if d == nil {
		return nil, nil
	}
	ms, err := core.Millis(*d, of)
	return &ms, err
}

// Bucket is a bucket's values under one branch of owners, the bucket's own
// when there are none. Inside a transaction, from In, its calls go through it.
type Bucket[T any] struct {
	parts
	values core.Values[T]
}

// OpenBucket opens a bucket of values of type T by key, on a store or on a
// database whose file then keeps it. It opens at its first call; its error
// is a name or an option it refuses.
//
//	sessions, err := kv.OpenBucket[Session](store, "sessions", kv.Idle(30*24*time.Hour))
//	codes, err := kv.OpenBucket[int64](store, "login-codes", kv.TTL(15*time.Minute))
//	seen, err := kv.OpenBucket[struct{}](store, "stripe-events", kv.TTL(7*24*time.Hour))
//
// A string, []byte, integer, float or bool is kept as itself, struct{} as
// keys alone, and any other type as JSON.
func OpenBucket[T any](home tinystore.Home, name string, opts ...BucketOption) (*Bucket[T], error) {
	var o bucketOptions
	for _, opt := range opts {
		opt.bucket(&o)
	}
	if err := core.CheckName(name, "bucket"); err != nil {
		return nil, err
	}
	if o.ttl != nil && o.idle != nil {
		return nil, core.Errorf(core.Invalid, "kv bucket %s: ttl and idle are one or the other", name)
	}
	ttl, err := millis(o.ttl, "kv bucket "+name+": ttl")
	if err != nil {
		return nil, err
	}
	idleMs, err := millis(o.idle, "kv bucket "+name+": idle")
	if err != nil {
		return nil, err
	}
	of := homeOf(home)
	open := opener(of, wire.MethodKvBucketOpen, func(database *uint64) []byte {
		return wire.Encode(&wire.KvBucketOpen{Name: name, Ttl: ttl, Idle: idleMs, Database: database, Encrypted: o.encrypted})
	})
	return &Bucket[T]{
		parts:  parts{store: of.Store, name: name, opener: open, home: of.Database, via: viaOf(of, "kv bucket "+name)},
		values: core.ValuesOf[T](),
	}, nil
}

// Name is the bucket's name.
func (b *Bucket[T]) Name() string { return b.name }

// Under is the branch the owners name below this one: whose keys these are.
func (b *Bucket[T]) Under(owners ...any) *Bucket[T] {
	return &Bucket[T]{parts: b.below(owners), values: b.values}
}

// In is the bucket inside a transaction: the store's, whose writes commit
// together when its function returns, or a database's, whose writes commit
// with its rows.
func (b *Bucket[T]) In(tx tinystore.InTx) *Bucket[T] {
	return &Bucket[T]{parts: b.in(tx, "kv bucket "+b.name), values: b.values}
}

// Get is the key's value, and whether it holds one.
func (b *Bucket[T]) Get(ctx context.Context, key string) (T, bool, error) {
	entry, found, err := b.Entry(ctx, key)
	return entry.Value, found, err
}

// Entry is the key's value with its version and expiry, and whether it holds one.
func (b *Bucket[T]) Entry(ctx context.Context, key string) (Entry[T], bool, error) {
	found, err := b.read(ctx, key)
	if err != nil || !found.Found {
		return Entry[T]{Key: key}, false, err
	}
	return entryOf(key, found, b.values)
}

func entryOf[T any](key string, found wire.KvEntry, values core.Values[T]) (Entry[T], bool, error) {
	var raw wire.Raw
	if found.Value != nil {
		raw = *found.Value
	}
	value, err := values.Decode(raw)
	if err != nil {
		return Entry[T]{Key: key}, false, err
	}
	entry := Entry[T]{Key: key, Value: value, ExpiresAt: core.MomentOf(found.ExpiresAt)}
	if found.Version != nil {
		entry.Version = *found.Version
	}
	return entry, true, nil
}

// Has says whether the key holds a value.
func (b *Bucket[T]) Has(ctx context.Context, key string) (bool, error) {
	if b.tx != nil || b.err != nil {
		found, err := b.read(ctx, key)
		return found.Found, err
	}
	answer, err := b.call(ctx, wire.MethodKvHas, wire.KvCall{Key: key}, core.Reading)
	return foundOf(answer, err)
}

func (b *Bucket[T]) read(ctx context.Context, key string) (wire.KvEntry, error) {
	if b.err != nil {
		return wire.KvEntry{}, b.err
	}
	if b.tx != nil {
		return b.tx.Read(ctx, b.part(), key)
	}
	answer, err := b.call(ctx, wire.MethodKvGet, wire.KvCall{Key: key}, core.Reading)
	var found wire.KvEntry
	if err == nil {
		err = decode(answer, &found)
	}
	return found, err
}

// Set writes the value; a key that exists keeps its expiry unless the call gives one.
func (b *Bucket[T]) Set(ctx context.Context, key string, value T, opts ...SetOption) error {
	var o writeOptions
	for _, opt := range opts {
		opt.set(&o)
	}
	raw, err := b.values.Encode(value)
	if err != nil {
		return err
	}
	call, err := writeCall(key, o)
	if err != nil {
		return err
	}
	call.Value = rawOf(raw)
	if b.tx != nil || b.err != nil {
		return b.keep(wire.MethodKvSet, call, &wire.KvEntry{Found: true, Value: call.Value})
	}
	_, err = b.call(ctx, wire.MethodKvSet, call, core.Writing)
	return err
}

// Create writes the value only where no live key is, and says whether it did.
func (b *Bucket[T]) Create(ctx context.Context, key string, value T, opts ...CreateOption) (bool, error) {
	raw, err := b.values.Encode(value)
	if err != nil {
		return false, err
	}
	return b.create(ctx, key, rawOf(raw), opts)
}

// Add puts the key in a bucket used as a set, a bucket of struct{}; false
// when it was there.
func (b *Bucket[T]) Add(ctx context.Context, key string, opts ...CreateOption) (bool, error) {
	return b.create(ctx, key, nil, opts)
}

func (b *Bucket[T]) create(ctx context.Context, key string, value *wire.Raw, opts []CreateOption) (bool, error) {
	var o writeOptions
	for _, opt := range opts {
		opt.create(&o)
	}
	call, err := writeCall(key, o)
	if err != nil {
		return false, err
	}
	call.Value = value
	if b.tx != nil || b.err != nil {
		found, err := b.read(ctx, key)
		if err != nil || found.Found {
			return false, err
		}
		return true, b.keep(wire.MethodKvCreate, call, &wire.KvEntry{Found: true, Value: value})
	}
	answer, err := b.call(ctx, wire.MethodKvCreate, call, core.Writing)
	if err != nil {
		return false, err
	}
	var written wire.KvWritten
	err = decode(answer, &written)
	return written.Written, err
}

// Take reads the value and removes the key in one commit: of two callers
// taking a code at once, one gets it.
func (b *Bucket[T]) Take(ctx context.Context, key string, opts ...CheckOption) (T, bool, error) {
	found, err := b.remove(ctx, wire.MethodKvTake, key, opts)
	if err != nil || !found.Found {
		var zero T
		return zero, false, err
	}
	entry, ok, err := entryOf(key, found, b.values)
	return entry.Value, ok, err
}

// Delete removes the key, and says whether it was there.
func (b *Bucket[T]) Delete(ctx context.Context, key string, opts ...CheckOption) (bool, error) {
	found, err := b.remove(ctx, wire.MethodKvDelete, key, opts)
	return found.Found, err
}

func (b *Bucket[T]) remove(ctx context.Context, method uint16, key string, opts []CheckOption) (wire.KvEntry, error) {
	var o writeOptions
	for _, opt := range opts {
		opt.check(&o)
	}
	call, err := writeCall(key, o)
	if err != nil {
		return wire.KvEntry{}, err
	}
	if b.tx != nil || b.err != nil {
		// a take or a delete: what the key held, and its removal, kept when
		// the key was there or when the caller named the version it must be at
		found, err := b.read(ctx, key)
		if err == nil && (found.Found || o.checked) {
			err = b.keep(method, call, &wire.KvEntry{})
		}
		return found, err
	}
	answer, err := b.call(ctx, method, call, core.Writing)
	var found wire.KvEntry
	if err == nil {
		if method == wire.MethodKvTake {
			err = decode(answer, &found)
		} else {
			var had wire.KvFound
			err = decode(answer, &had)
			found.Found = had.Found
		}
	}
	return found, err
}

// Expire gives a live key a new expiry, d from now, keeping its value.
func (b *Bucket[T]) Expire(ctx context.Context, key string, d time.Duration, opts ...CheckOption) error {
	return b.expire(ctx, key, &d, nil, opts)
}

// ExpireAt gives a live key a new expiry at a time, keeping its value.
func (b *Bucket[T]) ExpireAt(ctx context.Context, key string, at time.Time, opts ...CheckOption) error {
	return b.expire(ctx, key, nil, &at, opts)
}

func (b *Bucket[T]) expire(ctx context.Context, key string, d *time.Duration, at *time.Time, opts []CheckOption) error {
	o := writeOptions{ttl: d, expiresAt: at}
	for _, opt := range opts {
		opt.check(&o)
	}
	call, err := writeCall(key, o)
	if err != nil {
		return err
	}
	if b.tx != nil || b.err != nil {
		found, err := b.read(ctx, key)
		if err != nil || (!found.Found && !o.checked) {
			return err
		}
		var now *wire.KvEntry
		if found.Found {
			expires := time.Now().UnixMilli()
			if d != nil {
				expires += d.Milliseconds()
			} else {
				expires = at.UnixMilli()
			}
			now = &found
			now.ExpiresAt = &expires
		}
		return b.keep(wire.MethodKvExpire, call, now)
	}
	_, err = b.call(ctx, wire.MethodKvExpire, call, core.Writing)
	return err
}

// Clear removes this branch's keys and every branch under it, at once however many.
func (b *Bucket[T]) Clear(ctx context.Context) error {
	if b.err != nil {
		return b.err
	}
	if b.tx != nil {
		return b.tx.Clear(b.part())
	}
	_, err := b.via.Call(ctx, b.opener, wire.MethodKvClear, func(handle uint64) []byte {
		return wire.Encode(&wire.KvBranch{Handle: handle, Under: b.under})
	}, core.Writing)
	return err
}

// List is one page of this branch's own keys, in the byte order of their
// text: "10" before "9".
func (b *Bucket[T]) List(ctx context.Context, opts ...ListOption) (Page[T], error) {
	var o listOptions
	for _, opt := range opts {
		opt.list(&o)
	}
	if b.err != nil {
		return Page[T]{}, b.err
	}
	answer, err := b.via.Call(ctx, b.opener, wire.MethodKvList, func(handle uint64) []byte {
		return wire.Encode(&wire.KvList{Handle: handle, Under: b.under, After: o.after, Limit: o.limit})
	}, core.Reading)
	if err != nil {
		return Page[T]{}, err
	}
	var page wire.KvPage
	if err := decode(answer, &page); err != nil {
		return Page[T]{}, err
	}
	out := Page[T]{}
	for _, found := range page.Entries {
		key := ""
		if found.Key != nil {
			key = *found.Key
		}
		entry, _, err := entryOf(key, found, b.values)
		if err != nil {
			return Page[T]{}, err
		}
		out.Entries = append(out.Entries, entry)
	}
	if page.Next != nil {
		out.Next = *page.Next
	}
	return out, nil
}

// All walks this branch's own keys a page at a time, holding nothing between
// pages. A key written during the walk may or may not be met; it renews no idle key.
func (b *Bucket[T]) All(ctx context.Context, opts ...ListOption) iter.Seq2[Entry[T], error] {
	return func(yield func(Entry[T], error) bool) {
		after := ""
		for {
			page, err := b.List(ctx, append(opts, After(after))...)
			if err != nil {
				yield(Entry[T]{}, err)
				return
			}
			for _, entry := range page.Entries {
				if !yield(entry, nil) {
					return
				}
			}
			if page.Next == "" {
				return
			}
			after = page.Next
		}
	}
}

func (b *Bucket[T]) call(ctx context.Context, method uint16, call wire.KvCall, idempotence core.Idempotence) ([]byte, error) {
	if b.err != nil {
		return nil, b.err
	}
	return b.via.Call(ctx, b.opener, method, func(handle uint64) []byte {
		call.Handle, call.Under = handle, b.under
		return wire.Encode(&call)
	}, idempotence)
}

// keep keeps a write for the store's transaction's commit.
func (b *Bucket[T]) keep(method uint16, call wire.KvCall, found *wire.KvEntry) error {
	if b.err != nil {
		return b.err
	}
	return b.tx.Write(method, b.part(), call, found)
}

// writeCall is a write's key, expiry and condition as a call carries them.
func writeCall(key string, o writeOptions) (wire.KvCall, error) {
	call := wire.KvCall{Key: key}
	if o.ttl != nil && o.expiresAt != nil {
		return call, core.Errorf(core.Invalid, "a write gives TTL or ExpiresAt, not both")
	}
	if o.ttl != nil {
		ms, err := core.Millis(*o.ttl, "a key's ttl")
		if err != nil {
			return call, err
		}
		call.Ttl = &ms
	}
	if o.expiresAt != nil {
		at := o.expiresAt.UnixMilli()
		call.ExpiresAt = &at
	}
	if o.checked {
		version := []byte(o.ifVersion)
		if version == nil {
			version = []byte{}
		}
		call.IfVersion = &version
	}
	return call, nil
}

func rawOf(raw wire.Raw) *wire.Raw {
	if raw == nil {
		return nil
	}
	return &raw
}

func decode(answer []byte, m wire.Message) error {
	if err := wire.Decode(answer, m); err != nil {
		return core.Wrap(core.Protocol, err, "an answer that does not read")
	}
	return nil
}

func foundOf(answer []byte, err error) (bool, error) {
	if err != nil {
		return false, err
	}
	var found wire.KvFound
	err = decode(answer, &found)
	return found.Found, err
}
