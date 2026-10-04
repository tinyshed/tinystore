# KV API for Go

Every public type, function and constant of the KV engine in `github.com/tinyshed/tinystore/kv`, generated from its source. The [KV guide](../../kv/README.md) explains how to use them, and the [Bun and Node](../bun/kv.md) and [Python](../python/kv.md) pages list the same API.

## ErrOutcomeUnknown

```go
var ErrOutcomeUnknown = sqlite.ErrOutcomeUnknown
```

ErrOutcomeUnknown is returned for a write whose group's commit failed. The write may or may not be in the file, so its caller reads it back before writing again.

## Allowance

```go
type Allowance struct {
	OK bool
	// Left is how many more requests would pass now.
	Left int64
	// RetryAfter is how long until the request would pass; zero when OK.
	RetryAfter time.Duration
}
```

Allowance is what a limiter answers a request.

## Bucket

```go
type Bucket[V any] struct {
	// contains filtered or unexported fields
}
```

Bucket holds values of type V by key. Of and WithTx give handles on the same bucket, and a handle may be used from any number of goroutines.

### OpenBucket

```go
func OpenBucket[V any](ctx context.Context, state *Store, name string, options ...BucketOption) (*Bucket[V], error)
```

OpenBucket opens the bucket name of kv.db for values of type V, creating it the first time. A name that holds counters is ErrInvalid.

### Bucket.All

```go
func (b *Bucket[V]) All(ctx context.Context) iter.Seq2[Entry[V], error]
```

All is every one of this branch's own keys, in Scan's order, a page of Scan at a time. Each page is its own snapshot and none is held between pages, so a slow loop keeps no reader open, and a key written or deleted during the walk may or may not be met; inside View or Tx the pages share its snapshot. An error ends the walk as its last element.

### Bucket.Clear

```go
func (b *Bucket[V]) Clear(ctx context.Context) error
```

Clear removes every key of this branch and of the branches under it. A branch of up to 10,000 keys is deleted in one transaction; a larger one is marked cleared at the file's revision, which hides its keys from every call at once, and Maintain deletes them 10,000 a transaction. A key written after the Clear is a new key. Inside Tx a branch over 10,000 keys is ErrLimit, since deleting it would hold the writer for seconds.

### Bucket.Delete

```go
func (b *Bucket[V]) Delete(ctx context.Context, key any, options ...Option) error
```

Delete removes key; an absent key is not an error, except to IfVersion.

### Bucket.Get

```go
func (b *Bucket[V]) Get(ctx context.Context, key any) (V, bool, error)
```

Get is the value under key, and whether a live key holds one.

### Bucket.GetEntry

```go
func (b *Bucket[V]) GetEntry(ctx context.Context, key any) (Entry[V], bool, error)
```

GetEntry is Get with the key's version and expiry, the expiry as the file has it: a Sliding renewal waiting for its flush is not in it yet.

### Bucket.Has

```go
func (b *Bucket[V]) Has(ctx context.Context, key any) (bool, error)
```

Has says whether a live key is there, without reading its value.

### Bucket.Of

```go
func (b *Bucket[V]) Of(owners ...any) *Bucket[V]
```

Of is the branch of this bucket that owners name, one after another: a string, a \[]byte or an integer each, an integer by its decimal text. A branch exists while it holds keys; there is nothing to create or drop.

### Bucket.Scan

```go
func (b *Bucket[V]) Scan(ctx context.Context, query Query) (Page[V], error)
```

Scan is a page of this branch's own keys, not those of the branches under it, from one snapshot, in the byte order of their text: "10" before "9".

### Bucket.Set

```go
func (b *Bucket[V]) Set(ctx context.Context, key any, value V, options ...Option) error
```

Set writes value under key. A key it creates, or one that had expired, takes the bucket's DefaultTTL unless kv.TTL or kv.ExpireAt says otherwise; a live key keeps the expiry it has unless one of them does.

### Bucket.SetEntry

```go
func (b *Bucket[V]) SetEntry(ctx context.Context, key any, value V, options ...Option) (Entry[V], error)
```

SetEntry is Set, returning what it wrote with its new version.

### Bucket.SetEntryIfAbsent

```go
func (b *Bucket[V]) SetEntryIfAbsent(ctx context.Context, key any, value V, options ...Option) (
	Entry[V], bool, error,
)
```

SetEntryIfAbsent is SetIfAbsent, returning the entry it wrote or the one that was there, each with its version.

### Bucket.SetIfAbsent

```go
func (b *Bucket[V]) SetIfAbsent(ctx context.Context, key any, value V, options ...Option) (bool, error)
```

SetIfAbsent writes value under key only if no live key is there, and says whether it did; an expired key is absent.

### Bucket.Take

```go
func (b *Bucket[V]) Take(ctx context.Context, key any, options ...Option) (V, bool, error)
```

Take reads the value under key and deletes it in one step, so that of the callers taking one key only one gets it. A value that no longer decodes is ErrCorrupt and stays where it was, inside Tx as well.

### Bucket.Touch

```go
func (b *Bucket[V]) Touch(ctx context.Context, key any, options ...Option) (bool, error)
```

Touch gives a live key a new expiry, kv.TTL's, kv.ExpireAt's or else the bucket's DefaultTTL, and keeps its value and version, so that a renewal fails no IfVersion. It says whether the key was there.

### Bucket.WithTx

```go
func (b *Bucket[V]) WithTx(tx *Tx) *Bucket[V]
```

WithTx is this bucket inside tx: its calls run in tx's transaction and see its writes, and after the function tx was given returns they are ErrClosed.

## BucketOption

```go
type BucketOption interface {
	// contains filtered or unexported methods
}
```

BucketOption changes how a bucket serves its values; none changes what its bytes mean, so a program may change them between runs.

### Sliding

```go
func Sliding(term time.Duration) BucketOption
```

Sliding gives a key term from the last time it was read: a key created without kv.TTL or kv.ExpireAt gets term, and a Get, GetEntry or Has renews a live key to term from now once a thirtieth of the term has passed since it last did. With DefaultTTL it is ErrInvalid.

A read does not write: the renewal waits a second for the next flush, and a crash forgets the renewals since the last. So a key read at t lives at least until t + term − term/30 unless it is renewed.

A key with less than a minute left is renewed before its read returns.

### WithCodec

```go
func WithCodec[V any](codec Codec[V]) BucketOption
```

WithCodec writes a bucket's values through codec instead of the bytes their type would get; a codec of another value type is ErrInvalid at OpenBucket.

## Codec

```go
type Codec[V any] interface {
	Encode(V) ([]byte, error)
	Decode([]byte) (V, error)
}
```

Codec writes a bucket's values when the bytes their type would get are not the ones wanted; see WithCodec.

## Config

```go
type Config[T any] struct {
	// contains filtered or unexported fields
}
```

Config is an application's settings, a value of T made of layers in the order given, each over the one before it, and what Update kept over them all:

	kv.Defaults(Config{Port: 8080}), kv.Defaults(fromYAML)
	kv.FromEnv("APP", ".env")       APP_PORT=3000
	config.Update(ctx, func(c *Config) { c.Port = 4000 })

Get reads it from memory. Update checks a change and keeps it field by field in kv.db, so that it outlives a restart, and every handle on the config sees it at once, in this process and through the server; Reset gives a field back to the layers under it.

T is a struct. A field's path is its JSON name, a nested struct's below its own: limits.rps. A field tagged \`env:"NAME"\` reads that variable; another reads the prefix and its path in upper snake case, APP\_LIMITS\_RPS. A field tagged \`secret:"true"\` comes from the layers alone: Update refuses it and Sources hides it. A field tagged \`required:"true"\` is ErrInvalid at OpenConfig while the layers leave it its zero value.

### OpenConfig

```go
func OpenConfig[T any](ctx context.Context, state *Store, name string, options ...ConfigOption) (*Config[T], error)
```

OpenConfig opens the config name of kv.db, creating it the first time, and makes it from its layers. Defaults and an environment that do not fit T, or that fail Validate, are ErrInvalid; a kept value that no longer fits its field is left out, logged, and named by Sources.

### Config.Get

```go
func (c *Config[T]) Get() T
```

Get is the config now, built once a change and shared: it is not to be changed, which Update does.

### Config.Reset

```go
func (c *Config[T]) Reset(ctx context.Context, paths ...string) error
```

Reset forgets what Update kept for paths, each a field or a struct of fields, so that they take their values from the layers under it again; without paths it forgets everything kept.

### Config.Sources

```go
func (c *Config[T]) Sources() []Source
```

Sources says where each field's value came from, in the order of T's fields, and why a kept value is left out.

### Config.Update

```go
func (c *Config[T]) Update(ctx context.Context, change func(*T)) error
```

Update changes the config: change gets a copy of it, and each field it changes is checked, kept, and seen by every handle at once. A change that fails Validate, sets a secret or empties a required field is ErrInvalid and keeps nothing.

### Config.Watch

```go
func (c *Config[T]) Watch(ctx context.Context) iter.Seq[T]
```

Watch yields the config now and each time it changes, from this handle or any other, until ctx ends. A watcher that falls behind skips to the latest.

## ConfigOption

```go
type ConfigOption func(*configSettings)
```

ConfigOption is where a config's values come from, and what checks them.

### Defaults

```go
func Defaults(values any) ConfigOption
```

Defaults is a layer of values: a T, which sets every field, or a map, as a JSON or YAML library reads a file into one, which sets the fields it names. A duration may be text, "30s".

### FromEnv

```go
func FromEnv(prefix string, files ...string) ConfigOption
```

FromEnv is a layer of the environment: each field from its variable, files first, as a dotenv library reads them, and the process's own over them. A file that is not there is skipped.

	PORT=3000  ORIGINS=a.com,b.com  TIMEOUT=1h30m  LIMITS={"rps":5}

### Validate

```go
func Validate[T any](check func(T) error) ConfigOption
```

Validate checks the config each time it is made, at open and before Update keeps a change: a change that fails is refused, and a kept value that fails is left out.

## CounterOption

```go
type CounterOption interface {
	// contains filtered or unexported methods
}
```

### LoseAtMost

```go
func LoseAtMost(d time.Duration) CounterOption
```

LoseAtMost keeps the counters in memory and writes what changed every d, on Close and on Maintain, so that an Add takes no write.

A crash loses the changes since the last flush that committed, a Delete's as an Add's. A flush waits for the writer as any write does, so on a busy file the loss can reach d plus the flush's own wait and time.

Handles on counters of one name share the memory and join no transaction. Opening them again with another d, or without LoseAtMost, is ErrInvalid.

## Counters

```go
type Counters struct {
	// contains filtered or unexported fields
}
```

Counters holds an int64 by key. Of and WithTx give handles on the same counters, and a handle may be used from any number of goroutines.

### OpenCounters

```go
func OpenCounters(ctx context.Context, state *Store, name string, options ...CounterOption) (*Counters, error)
```

OpenCounters opens the counters name of kv.db, creating them the first time. A name that holds values is ErrInvalid.

### Counters.Add

```go
func (c *Counters) Add(ctx context.Context, key any, n int64) (int64, error)
```

Add adds n to the counter under key and returns what it holds now. An absent or expired counter starts from zero with the DefaultTTL of the counters, and a live one keeps its expiry, so a window does not slide. A sum past the int64 range is ErrLimit and changes nothing.

### Counters.Clear

```go
func (c *Counters) Clear(ctx context.Context) error
```

Clear removes every counter of this branch and of the branches under it, as Bucket.Clear does. LoseAtMost counters it also removes from memory once it commits, so that no flush writes them again; a Clear that fails leaves them.

### Counters.Delete

```go
func (c *Counters) Delete(ctx context.Context, key any) error
```

Delete removes the counter under key; an absent one is not an error.

### Counters.Get

```go
func (c *Counters) Get(ctx context.Context, key any) (int64, error)
```

Get is what the counter under key holds; an absent or expired one holds 0.

### Counters.Max

```go
func (c *Counters) Max(ctx context.Context, key any, n int64) (int64, error)
```

Max keeps the larger of the counter under key and n, an absent counter counting as zero, and returns what it holds now.

### Counters.Of

```go
func (c *Counters) Of(owners ...any) *Counters
```

Of is the branch of these counters that owners name, as Bucket.Of names one.

### Counters.WithTx

```go
func (c *Counters) WithTx(tx *Tx) *Counters
```

WithTx binds these counters to tx; LoseAtMost counters refuse calls through it.

## Entry

```go
type Entry[V any] struct {
	// Key is the key's text, without the branch's owners.
	Key     string
	Value   V
	Version Version
	// ExpiresAt is zero for a key that never expires.
	ExpiresAt time.Time
}
```

Entry is a value with what the file knows of it.

## KeyError

```go
type KeyError struct {
	Bucket, Path string
	Err          error
}
```

KeyError is a call refused because of one key: its bucket and the path of owners and key it named. errors.Is finds the store's sentinel in it.

### KeyError.Error

```go
func (e *KeyError) Error() string
```

### KeyError.Unwrap

```go
func (e *KeyError) Unwrap() error
```

## Limiter

```go
type Limiter struct {
	// contains filtered or unexported fields
}
```

Limiter lets each key's requests through at a steady rate with room for a burst. It is the generic cell rate algorithm: a key holds one time, when its next request is due, so there is no window whose edge lets twice the rate through, and a key costs one int64.

	Rate(3, time.Second), Burst(3): three pass at once, the fourth waits 333ms

Its times live in memory and reach the file every second and on Close, as LoseAtMost counters do: a crash forgets at most the last second, and so lets at most one burst more through. A key whose time has come is as one never seen, and maintenance deletes it.

### OpenLimiter

```go
func OpenLimiter(ctx context.Context, state *Store, name string, options ...LimiterOption) (*Limiter, error)
```

OpenLimiter opens the limiter name of kv.db, creating it the first time. A name that holds values or counters is ErrInvalid; the rate is the caller's, and may change between runs.

### Limiter.Allow

```go
func (l *Limiter) Allow(ctx context.Context, key any) (Allowance, error)
```

Allow asks for one request of key.

### Limiter.AllowN

```go
func (l *Limiter) AllowN(ctx context.Context, key any, n int64) (Allowance, error)
```

AllowN asks for n requests of key at once: all of them pass or none does. More than the burst never passes, and is ErrInvalid.

### Limiter.Of

```go
func (l *Limiter) Of(owners ...any) *Limiter
```

Of is the branch of this limiter that owners name, as Bucket.Of names one.

## LimiterOption

```go
type LimiterOption func(*limiterSettings)
```

LimiterOption says how fast a limiter lets requests through.

### Burst

```go
func Burst(n int64) LimiterOption
```

Burst is how many requests may pass at once before the rate holds the next; count of the Rate when it is not given.

### Rate

```go
func Rate(count int64, per time.Duration) LimiterOption
```

Rate lets count requests through every per, each key alone: Rate(100, time.Second). A limiter needs one.

## Maintenance

```go
type Maintenance struct {
	Flushed int // LoseAtMost counters written
	Renewed int // Sliding keys renewed
	Cleared int // rows a marked Clear hid, deleted
	Expired int
}
```

Maintenance is what one Maintain call did.

## Once

```go
type Once[V any] struct {
	// contains filtered or unexported fields
}
```

Once runs a function at most once a key and keeps what it returns, so that a request sent again is answered as the first was rather than done twice: a charge, an import, a webhook's effect.

	charges.Run(ctx, "req-7", charge) → runs charge, keeps its receipt a day
	charges.Run(ctx, "req-7", charge) → that receipt; charge does not run

One Run of a key runs its function at a time in the store's process, the clients of its server included: a Run of a key another Run is running waits for it and returns its answer. A function's error keeps nothing, so the next Run runs it again; an answer a program wants kept, a card declined, is a value rather than an error.

A claim on a key lives in memory as long as its Run. A process that dies after a function's effect outside the store and before its answer was kept runs it again on the next Run, so such an effect carries the key too: a payment provider's own idempotency key.

### OpenOnce

```go
func OpenOnce[V any](ctx context.Context, state *Store, name string, options ...BucketOption) (*Once[V], error)
```

OpenOnce opens the answers name of kv.db, creating them the first time. An answer lives a day unless DefaultTTL says. A name that holds anything else is ErrInvalid, and so is Sliding: an answer lives from when it was kept.

### Once.Delete

```go
func (o *Once[V]) Delete(ctx context.Context, key any) error
```

Delete forgets the answer kept under key, so that the next Run runs again.

### Once.Get

```go
func (o *Once[V]) Get(ctx context.Context, key any) (V, bool, error)
```

Get is the answer kept under key, and whether one is.

### Once.Of

```go
func (o *Once[V]) Of(owners ...any) *Once[V]
```

Of is the branch of these answers that owners name, as Bucket.Of names one.

### Once.Run

```go
func (o *Once[V]) Run(ctx context.Context, key any, fn func(context.Context) (V, error)) (V, error)
```

Run returns the answer kept under key, or runs fn and keeps what it returns.

While another Run of the key runs its function this one waits, until ctx ends, and then returns the answer it kept, or, when it failed, runs fn itself. fn's error is returned and keeps nothing. An answer is kept even when ctx ends after fn returned, since its work is done; an error keeping it is returned beside it.

## OpenOption

```go
type OpenOption interface {
	BucketOption
	CounterOption
}
```

### DefaultTTL

```go
func DefaultTTL(d time.Duration) OpenOption
```

DefaultTTL is the expiry a key gets when it is created without kv.TTL or kv.ExpireAt; a later Set or Add keeps the expiry a key has.

## Option

```go
type Option func(*callOptions)
```

### ExpireAt

```go
func ExpireAt(t time.Time) Option
```

ExpireAt gives the key until t by the store's clock.

### IfVersion

```go
func IfVersion(v Version) Option
```

IfVersion lets a call write only to a live key whose version is v; any other is tinystore.ErrConflict, an absent or expired key included.

### TTL

```go
func TTL(d time.Duration) Option
```

TTL gives the key d from now; see the rules of DefaultTTL.

## Options

```go
type Options struct{}
```

Options holds nothing a program sets yet; it is here so that an option can arrive without breaking a caller.

## Page

```go
type Page[V any] struct {
	Entries []Entry[V]
	// More says the limit, or the page's 4 MiB of values, ended the page before
	// the branch did.
	More bool
	// Next is the query that reads on.
	Next Query
}
```

Page is one page of a Scan, from one snapshot.

## Query

```go
type Query struct {
	// After, when it is not empty, starts the page after that key.
	After string
	Limit int // keys a page returns: 100 when zero, at most 1000
}
```

Query asks Scan for a page of a branch's own keys, in the byte order of their text.

## Quota

```go
type Quota struct {
	// contains filtered or unexported fields
}
```

Quota lets each key use up to a limit in every one of its windows, such as 100 messages every five hours and 300 a week, counted by one Allow.

A window starts at a key's first use after the last one ended and lasts its span, as a counter with DefaultTTL does, so that each key resets on its own:

	Window("session", 100, 5*time.Hour): first use 13:42 → resets at 18:42;
	a use at 19:07 starts the next window, which resets at 00:07

An Allow checks every window and counts in each, or in none, in one durable write: two quotas asked one after the other would leave the first counted when the second refused.

### OpenQuota

```go
func OpenQuota(ctx context.Context, state *Store, name string, windows ...QuotaOption) (*Quota, error)
```

OpenQuota opens the quota name of kv.db, creating it the first time. A name that holds anything else is ErrInvalid. Its windows are the caller's and may change between runs: a window kept under a name no longer given is forgotten, and a limit changed counts what its window used before.

### Quota.Allow

```go
func (q *Quota) Allow(ctx context.Context, key any) (QuotaUsage, error)
```

Allow uses one of key's every window, or none when one has no room left.

### Quota.AllowN

```go
func (q *Quota) AllowN(ctx context.Context, key any, n int64) (QuotaUsage, error)
```

AllowN uses n of key's every window at once, or none when one has no room for n. More than a window's limit never passes, and is ErrInvalid.

### Quota.Delete

```go
func (q *Quota) Delete(ctx context.Context, key any) error
```

Delete forgets key's windows, so that its next use starts each anew.

### Quota.Get

```go
func (q *Quota) Get(ctx context.Context, key any) (QuotaUsage, error)
```

Get is key's windows without using them: OK says whether one more use would pass now.

### Quota.Of

```go
func (q *Quota) Of(owners ...any) *Quota
```

Of is the branch of this quota that owners name, as Bucket.Of names one.

### Quota.Refund

```go
func (q *Quota) Refund(ctx context.Context, key any) error
```

Refund gives one use back to each of key's windows that has not reset since; a window never goes below nothing.

### Quota.RefundN

```go
func (q *Quota) RefundN(ctx context.Context, key any, n int64) error
```

RefundN gives n uses back, as Refund gives one.

## QuotaOption

```go
type QuotaOption func(*quotaSettings)
```

QuotaOption is one of a quota's windows.

### Window

```go
func Window(name string, limit int64, span time.Duration) QuotaOption
```

Window lets a key use up to limit every span, from its first use: Window("weekly", 300, 7\*24\*time.Hour). A name is \[a-z]\[a-z0-9\_]{0,31}, and a quota has one to eight windows.

## QuotaUsage

```go
type QuotaUsage struct {
	Allowance
	Windows map[string]WindowUsage
}
```

QuotaUsage is what a quota answers a key: whether its use passed, how many more would pass now and, when it did not, how long until it would, as a limiter's Allowance says; and each window by its name.

## Raw

```go
type Raw struct {
	Kind  RawKind
	Int   int64
	Bytes []byte
}
```

Raw is a value as its row keeps it: nothing, an integer or bytes.

A bucket of Raw reads what a bucket of any type wrote, and what it writes is read by a bucket of the type that kept it the same way. So a program without the types, such as the server for its clients, reads and writes every bucket:

	Raw{}                                    nothing, as a struct{} of a set
	Raw{Kind: RawInt, Int: 42}               an integer, as a bool or an int64
	Raw{Kind: RawBytes, Bytes: []byte("x")}  bytes, as a string, a []byte, a float's bits or JSON

## RawConfig

```go
type RawConfig struct {
	// contains filtered or unexported fields
}
```

RawConfig is what a config keeps, its changed fields as JSON by path, for a program that types them itself, as the server does for its clients. It checks that a value is JSON and the config within its bounds, and nothing a type would.

### OpenRawConfig

```go
func OpenRawConfig(ctx context.Context, state *Store, name string) (*RawConfig, error)
```

OpenRawConfig opens the config name of kv.db, creating it the first time. A name that holds values, counters or a limiter is ErrInvalid.

### RawConfig.Change

```go
func (r *RawConfig) Change(ctx context.Context, set map[string]json.RawMessage, reset []string) error
```

Change keeps set and forgets reset in one transaction; every watcher of the config sees the change once it commits.

### RawConfig.Kept

```go
func (r *RawConfig) Kept() (map[string]json.RawMessage, int64)
```

Kept is the changed fields and how many changes the config has had since this process opened it.

### RawConfig.Watch

```go
func (r *RawConfig) Watch(ctx context.Context) iter.Seq2[map[string]json.RawMessage, int64]
```

Watch yields the changed fields now and after each change, until ctx ends. A watcher that falls behind skips to the latest: it never sees a state the config did not have.

## RawKind

```go
type RawKind uint8
```

RawKind is which of the three a Raw holds.

### RawNothing

```go
const (
	RawNothing RawKind = iota
	RawInt
	RawBytes
)
```

## Source

```go
type Source struct {
	Path string
	// Value is its JSON, or *** for a secret.
	Value string
	// From is "default", "env NAME", "kept", or "" for T's zero value.
	From string
	// Ignored says why a kept value is left out, when it is.
	Ignored string
}
```

Source is where one field's value came from.

## Store

```go
type Store struct {
	// contains filtered or unexported fields
}
```

### Open

```go
func Open(ctx context.Context, store *tinystore.Store, _ Options) (*Store, error)
```

Open opens kv.db inside the store. The store closes it and, unless it is Manual, deletes expired keys every minute, and every ten seconds while the last pass stopped at its bound with expired or cleared rows left.

### Store.Close

```go
func (s *Store) Close(ctx context.Context) error
```

Close lets the work in flight finish, writes what LoseAtMost counters hold and the renewals reads asked for, and closes kv.db; cancellation stops waiting, not the cleanup. The store calls it: an application closes the store instead.

### Store.Maintain

```go
func (s *Store) Maintain(ctx context.Context) (Maintenance, error)
```

Maintain writes what LoseAtMost counters hold and the renewals Sliding reads asked for, deletes the rows a marked Clear hid, then deletes expired keys and the values they spilled. It deletes expired keys 10,000 a transaction and at most ten transactions a call.

The store calls it every minute unless it is Manual, and every ten seconds while the last call stopped at one of those bounds with rows left. It flushes counters and renewals on their own intervals.

No read returns an expired or cleared key, whether Maintain has deleted it or not.

### Store.Snapshot

```go
func (s *Store) Snapshot(ctx context.Context, dir string) ([]tinystore.SnapshotFile, error)
```

Snapshot copies kv.db into dir while the engine keeps working.

### Store.Tx

```go
func (s *Store) Tx(ctx context.Context, work func(*Tx) error) error
```

Tx runs work in one writer transaction over any buckets of kv.db: nil commits, an error or a panic rolls back. It never spans two engines.

### Store.View

```go
func (s *Store) View(ctx context.Context, read func(*Tx) error) error
```

View runs read against one snapshot of kv.db, held at most five seconds, as a records read is; a write inside it is ErrInvalid.

## Tx

```go
type Tx struct {
	// contains filtered or unexported fields
}
```

Tx is one transaction of kv.db, given to the function passed to Store.Tx or Store.View and used by the goroutine that runs it. A bucket works in it through WithTx.

## Version

```go
type Version struct {
	// contains filtered or unexported fields
}
```

Version is the revision of kv.db that wrote a value. It never repeats in a file, not after a delete, an expiry or a reopen, so an old IfVersion cannot pass against a key deleted and written again. Two versions are equal or not; a version marshals to text, so that it can travel to a page and back.

### Version.MarshalText

```go
func (v Version) MarshalText() ([]byte, error)
```

### Version.String

```go
func (v Version) String() string
```

### Version.UnmarshalText

```go
func (v *Version) UnmarshalText(text []byte) error
```

## WindowUsage

```go
type WindowUsage struct {
	Used, Limit, Left int64
	// ResetAt is when the window ends, after which the next use starts
	// another; zero for a window not started.
	ResetAt time.Time
}
```

WindowUsage is one window of a key.

<!-- Generated by task reference from kv/. Edit the doc comments there, not this file. -->
