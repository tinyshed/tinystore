# KV API for Go

Every public type, function and constant of the KV engine in `tinyshed.org/tinystore/kv`, generated from its source. The [KV guide](../../kv/README.md) explains how to use them, and the [Bun and Node](../bun/kv.md) and [Python](../python/kv.md) pages list the same API.

## After

```go
func After(key string) after
```

After starts a page after a key: the page before's Next.

## Burst

```go
func Burst(n int) burst
```

Burst is how many requests a rate limit lets through at once.

## Encrypted

```go
func Encrypted() encrypted
```

Encrypted keeps every value of the bucket sealed with the store's encryption key.

## ExpiresAt

```go
func ExpiresAt(t time.Time) expiresAt
```

ExpiresAt is when a key a write gives expires, as a time.

## FlushEvery

```go
func FlushEvery(d time.Duration) flushEvery
```

FlushEvery keeps counts in memory and writes them that often, so that an add costs no commit and a crash loses at most that span.

## Idle

```go
func Idle(d time.Duration) idle
```

Idle is how long a key lives from its last read or write: a session gone unused. A bucket has a TTL or an Idle, not both.

## IfVersion

```go
func IfVersion(v Version) ifVersion
```

IfVersion writes only while the key is at this version, an entry's Version: tinystore.ErrConflict otherwise.

## Keep

```go
func Keep(d time.Duration) keep
```

Keep is how long an answer is kept: a day unless given.

## Limit

```go
func Limit(n int) limit
```

Limit is how many keys a page holds: 1000, and 10,000 at most.

## Rate

```go
func Rate(n int, per time.Duration) rate
```

Rate is a rate limit's smooth rate: n requests a per, kv.Rate(100, time.Second).

## TTL

```go
func TTL(d time.Duration) ttl
```

TTL is how long a key lives from its last write: a bucket's or counters' for every key, or one write's own.

## Window

```go
func Window(name string, n int, per time.Duration) windowOption
```

Window is a quota's window: n uses a per, counted from a key's first use, kv.Window("daily", 100, 24\*time.Hour).

## Allowance

```go
type Allowance struct {
	OK bool
	// Left is how many more would pass now.
	Left uint64
	// RetryAt is when a refused request may try again; the zero time when it passed.
	RetryAt time.Time
	// Windows are a quota's windows by name; none for a rate limit.
	Windows map[string]WindowUse
}
```

Allowance is what a rate limit or a quota answers a request.

## Bucket

```go
type Bucket[T any] struct {
	// contains filtered or unexported fields
}
```

Bucket is a bucket's values under one branch of owners, the bucket's own when there are none. Inside a transaction, from In, its calls go through it.

### OpenBucket

```go
func OpenBucket[T any](home tinystore.Home, name string, opts ...BucketOption) (*Bucket[T], error)
```

OpenBucket opens a bucket of values of type T by key, on a store or on a database whose file then keeps it. It opens at its first call; its error is a name or an option it refuses.

	sessions, err := kv.OpenBucket[Session](store, "sessions", kv.Idle(30*24*time.Hour))
	codes, err := kv.OpenBucket[int64](store, "login-codes", kv.TTL(15*time.Minute))
	seen, err := kv.OpenBucket[struct{}](store, "stripe-events", kv.TTL(7*24*time.Hour))

A string, \[]byte, integer, float or bool is kept as itself, struct{} as keys alone, and any other type as JSON.

### Bucket.Add

```go
func (b *Bucket[T]) Add(ctx context.Context, key string, opts ...CreateOption) (bool, error)
```

Add puts the key in a bucket used as a set, a bucket of struct{}; false when it was there.

### Bucket.All

```go
func (b *Bucket[T]) All(ctx context.Context, opts ...ListOption) iter.Seq2[Entry[T], error]
```

All walks this branch's own keys a page at a time, holding nothing between pages. A key written during the walk may or may not be met; it renews no idle key.

### Bucket.Clear

```go
func (b *Bucket[T]) Clear(ctx context.Context) error
```

Clear removes this branch's keys and every branch under it, at once however many.

### Bucket.Create

```go
func (b *Bucket[T]) Create(ctx context.Context, key string, value T, opts ...CreateOption) (bool, error)
```

Create writes the value only where no live key is, and says whether it did.

### Bucket.Delete

```go
func (b *Bucket[T]) Delete(ctx context.Context, key string, opts ...CheckOption) (bool, error)
```

Delete removes the key, and says whether it was there.

### Bucket.Entry

```go
func (b *Bucket[T]) Entry(ctx context.Context, key string) (Entry[T], bool, error)
```

Entry is the key's value with its version and expiry, and whether it holds one.

### Bucket.Expire

```go
func (b *Bucket[T]) Expire(ctx context.Context, key string, d time.Duration, opts ...CheckOption) error
```

Expire gives a live key a new expiry, d from now, keeping its value.

### Bucket.ExpireAt

```go
func (b *Bucket[T]) ExpireAt(ctx context.Context, key string, at time.Time, opts ...CheckOption) error
```

ExpireAt gives a live key a new expiry at a time, keeping its value.

### Bucket.Get

```go
func (b *Bucket[T]) Get(ctx context.Context, key string) (T, bool, error)
```

Get is the key's value, and whether it holds one.

### Bucket.Has

```go
func (b *Bucket[T]) Has(ctx context.Context, key string) (bool, error)
```

Has says whether the key holds a value.

### Bucket.In

```go
func (b *Bucket[T]) In(tx tinystore.InTx) *Bucket[T]
```

In is the bucket inside a transaction: the store's, whose writes commit together when its function returns, or a database's, whose writes commit with its rows.

### Bucket.List

```go
func (b *Bucket[T]) List(ctx context.Context, opts ...ListOption) (Page[T], error)
```

List is one page of this branch's own keys, in the byte order of their text: "10" before "9".

### Bucket.Name

```go
func (b *Bucket[T]) Name() string
```

Name is the bucket's name.

### Bucket.Set

```go
func (b *Bucket[T]) Set(ctx context.Context, key string, value T, opts ...SetOption) error
```

Set writes the value; a key that exists keeps its expiry unless the call gives one.

### Bucket.Take

```go
func (b *Bucket[T]) Take(ctx context.Context, key string, opts ...CheckOption) (T, bool, error)
```

Take reads the value and removes the key in one commit: of two callers taking a code at once, one gets it.

### Bucket.Under

```go
func (b *Bucket[T]) Under(owners ...any) *Bucket[T]
```

Under is the branch the owners name below this one: whose keys these are.

## BucketOption

```go
type BucketOption interface {
	// contains filtered or unexported methods
}
```

BucketOption is how a bucket opens.

## CheckOption

```go
type CheckOption interface {
	// contains filtered or unexported methods
}
```

CheckOption is the version a Take, a Delete or an Expire needs the key at.

## Counters

```go
type Counters struct {
	// contains filtered or unexported fields
}
```

Counters are numbers by key that only add up: 0 for one never added to, or expired.

### OpenCounters

```go
func OpenCounters(home tinystore.Home, name string, opts ...CountersOption) (*Counters, error)
```

OpenCounters opens counters by key on the store.

	attempts, err := kv.OpenCounters(store, "login-attempts", kv.TTL(15*time.Minute))

### Counters.Add

```go
func (c *Counters) Add(ctx context.Context, key string, n int64) (int64, error)
```

Add adds n and gives the new count; inside a transaction the count is known once it committed, and Add gives 0.

### Counters.Clear

```go
func (c *Counters) Clear(ctx context.Context) error
```

Clear removes this branch's counters and every branch's under it.

### Counters.Delete

```go
func (c *Counters) Delete(ctx context.Context, key string) (bool, error)
```

Delete removes the counter, and says whether it was there.

### Counters.Get

```go
func (c *Counters) Get(ctx context.Context, key string) (int64, error)
```

Get is the key's count.

### Counters.In

```go
func (c *Counters) In(tx tinystore.InTx) *Counters
```

In is the counters inside the store's transaction: an add commits with its writes.

### Counters.Under

```go
func (c *Counters) Under(owners ...any) *Counters
```

Under is the counters of a branch, each key apart from the same key elsewhere.

## CountersOption

```go
type CountersOption interface {
	// contains filtered or unexported methods
}
```

CountersOption is how counters open.

## CreateOption

```go
type CreateOption interface {
	// contains filtered or unexported methods
}
```

CreateOption is what a Create or an Add gives besides its value: an expiry.

## Entry

```go
type Entry[T any] struct {
	Key     string
	Value   T
	Version Version
	// ExpiresAt is the zero time for a key that never expires.
	ExpiresAt time.Time
}
```

Entry is a key's value with its version and expiry.

## ListOption

```go
type ListOption interface {
	// contains filtered or unexported methods
}
```

ListOption is how a page of keys reads.

## Once

```go
type Once[T any] struct {
	// contains filtered or unexported fields
}
```

Once runs a function once a key and keeps its answer: a request sent twice is answered as the first was rather than done twice. The core lets one run of a key go at a time, every client of the store included: it answers the kept answer, or hands the run to this client and keeps what it sends back.

### OpenOnce

```go
func OpenOnce[T any](home tinystore.Home, name string, opts ...OnceOption) (*Once[T], error)
```

OpenOnce opens answers of type T kept once a key, a day unless Keep says.

	charges, err := kv.OpenOnce[Receipt](store, "charges", kv.Keep(24*time.Hour))

### Once.Delete

```go
func (o *Once[T]) Delete(ctx context.Context, key string) (bool, error)
```

Delete forgets the answer kept for key, so that its next run runs fn again.

### Once.Get

```go
func (o *Once[T]) Get(ctx context.Context, key string) (T, bool, error)
```

Get is the answer kept for key, and whether one is.

### Once.Run

```go
func (o *Once[T]) Run(ctx context.Context, key string, fn func(ctx context.Context) (T, error)) (T, error)
```

Run is the answer kept for key, or fn's, which is kept. A run of a key that another runs, here or in another process, waits for it and gets its answer; an error of fn keeps nothing and is Run's, so the next run runs fn again. A connection lost after fn returned and before its answer was kept is OutcomeUnknown.

### Once.Under

```go
func (o *Once[T]) Under(owners ...any) *Once[T]
```

Under is the answers of a branch, each key apart from the same key elsewhere.

## OnceOption

```go
type OnceOption interface {
	// contains filtered or unexported methods
}
```

OnceOption is how answers kept once a key open.

## Page

```go
type Page[T any] struct {
	Entries []Entry[T]
	Next    string
}
```

Page is a page of a branch's keys, and where the next starts: "" on the last.

## Quota

```go
type Quota struct {
	// contains filtered or unexported fields
}
```

Quota counts uses by key in named windows that reset, all of them or none in one durable write.

### OpenQuota

```go
func OpenQuota(home tinystore.Home, name string, opts ...QuotaOption) (*Quota, error)
```

OpenQuota opens a quota of windows, each counted from a key's first use: kv.OpenQuota(store, "ai", kv.Window("daily", 100, 24\*time.Hour)).

### Quota.Allow

```go
func (q *Quota) Allow(ctx context.Context, key string, n ...int64) (Allowance, error)
```

Allow uses one of key's every window, or n, or none when one has no room; more than a window's limit is Invalid.

### Quota.Peek

```go
func (q *Quota) Peek(ctx context.Context, key string) (Allowance, error)
```

Peek is how key stands in every window, using nothing.

### Quota.Refund

```go
func (q *Quota) Refund(ctx context.Context, key string, n ...int64) error
```

Refund gives one use back to every window of key, or n.

### Quota.Reset

```go
func (q *Quota) Reset(ctx context.Context, key string) error
```

Reset forgets key's uses in every window.

### Quota.Under

```go
func (q *Quota) Under(owners ...any) *Quota
```

Under is the quota of a branch, each key apart from the same key elsewhere.

## QuotaOption

```go
type QuotaOption interface {
	// contains filtered or unexported methods
}
```

QuotaOption is how a quota opens.

## RateLimit

```go
type RateLimit struct {
	// contains filtered or unexported fields
}
```

RateLimit is a smooth rate of requests a key, the generic cell rate algorithm with its times in the core's memory.

### OpenRateLimit

```go
func OpenRateLimit(home tinystore.Home, name string, opts ...RateLimitOption) (*RateLimit, error)
```

OpenRateLimit opens a rate limit: kv.OpenRateLimit(store, "api", kv.Rate(100, time.Second), kv.Burst(20)).

### RateLimit.Allow

```go
func (r *RateLimit) Allow(ctx context.Context, key string, n ...int64) (Allowance, error)
```

Allow asks for one request of key, or n: all pass or none; more than the burst is Invalid.

### RateLimit.Peek

```go
func (r *RateLimit) Peek(ctx context.Context, key string) (Allowance, error)
```

Peek is the answer one request would get, using nothing.

### RateLimit.Reset

```go
func (r *RateLimit) Reset(ctx context.Context, key string) error
```

Reset forgets key, so that its next request starts afresh.

### RateLimit.Under

```go
func (r *RateLimit) Under(owners ...any) *RateLimit
```

Under is the rate limit of a branch, each key apart from the same key elsewhere.

## RateLimitOption

```go
type RateLimitOption interface {
	// contains filtered or unexported methods
}
```

RateLimitOption is how a rate limit opens.

## SetOption

```go
type SetOption interface {
	// contains filtered or unexported methods
}
```

SetOption is what a Set gives besides its value: an expiry, or the version the key must still be at.

## Version

```go
type Version []byte
```

Version is what a key was when it was read: compared only for equality, given back to IfVersion.

## WindowUse

```go
type WindowUse struct {
	Used, Limit, Left uint64
	// ResetsAt is when the window resets; the zero time for one not started.
	ResetsAt time.Time
}
```

WindowUse is one window of a quota as a key stands in it.

<!-- Generated by task reference from sdk/go/kv/. Edit the doc comments there, not this file. -->
