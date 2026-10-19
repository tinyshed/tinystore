# KV API for Rust

Every public type, trait and function of the KV engine in `tinystore::kv`, generated from its source. The [KV guide](../../chat/sessions-and-limits.md) explains how to use them, and the [Bun and Node](../bun/kv.md), [Python](../python/kv.md) and [Go](../go/kv.md) pages list the same API.

## Store

```rust
impl Store
```

### Store.bucket

```rust
pub fn bucket<V: Value>(&self, name: &str) -> BucketBuilder<V>
```

A bucket of values of type `V` by key, in the store's kv.db; the engine
opens with the first handle.

### Store.counters

```rust
pub fn counters(&self, name: &str) -> CountersBuilder
```

Numbers by key that add up, in the store's kv.db: attempts, hits, uses.

### Store.once

```rust
pub fn once<V: Value>(&self, name: &str) -> OnceBuilder<V>
```

Functions run once a key, their answers kept in the store's kv.db: a
request sent twice gets the first one's answer instead of doing its work
again, as an idempotency key promises.

### Store.quota

```rust
pub fn quota(&self, name: &str) -> QuotaBuilder
```

Uses a key may make in windows that each reset on their own, in the
store's kv.db: `store.quota("ai").window("daily", 100, DAY)`.

### Store.rate_limit

```rust
pub fn rate_limit(&self, name: &str) -> RateLimitBuilder
```

Requests let through at a steady rate a key, with room for a burst, in
the store's kv.db: `store.rate_limit("api").rate(100, Duration::from_secs(1))`.

## Allowance

```rust
pub struct Allowance {
    pub ok: bool,
    pub left: u64,
    pub retry_at: Option<SystemTime>,
    pub windows: Vec<Window>,
}
```

What a rate limit or a quota answers a request: whether it passed, and
what is left.

### Allowance.window

```rust
pub fn window(&self, name: &str) -> Option<&Window>
```

The window named `name`, of a quota's answer.

## Window

```rust
pub struct Window {
    pub name: String,
    pub used: u64,
    pub limit: u64,
    pub left: u64,
    pub resets_at: Option<SystemTime>,
}
```

One window of a quota, as a key stands in it.

## All

```rust
pub struct All<V> { /* private fields */ }
```

Every key of a branch, a page at a time.

## Bucket

```rust
pub struct Bucket<V> { /* private fields */ }
```

Values of one type by key. A handle is cheap to clone and to keep; `under`
gives the same bucket seen from a branch.

### Bucket.set

```rust
pub fn set(&self, key: impl Key, value: &V) -> Result<()>
```

Writes `value` under `key`. A key that is there keeps its ttl; a new one
takes the bucket's, and an idle key lives its span from the write.

### Bucket.create

```rust
pub fn create(&self, key: impl Key, value: &V) -> Result<bool>
```

Writes `value` only when `key` is not there, and says whether it did; an
expired key is not there. A key that is there is `false`, never an error.

### Bucket.key

```rust
pub fn key(&self, key: impl Key) -> KeyCall<'_, V>
```

A call on `key` with what it asks beside its value, a new expiry or a
version the key must still be at, run by its last step:
`sessions.key(&token).if_version(seen).set(&next)`.

### Bucket.get

```rust
pub fn get(&self, key: impl Key) -> Result<Option<V>>
```

### Bucket.entry

```rust
pub fn entry(&self, key: impl Key) -> Result<Option<Entry<V>>>
```

The value with its version and expiry.

### Bucket.take

```rust
pub fn take(&self, key: impl Key) -> Result<Option<V>>
```

Reads `key` and removes it in one commit: of two callers taking the same
key, one gets the value. A value that no longer reads as the bucket's
type is `Corrupt`, and stays.

### Bucket.list

```rust
pub fn list(&self, limit: usize, after: Option<&str>) -> Result<Page<V>>
```

One page of this branch's own keys, after `after` when it continues
another; `limit` keys at most, one at least, and never more than
`PAGE_KEYS`.

### Bucket.all

```rust
pub fn all(&self) -> All<V>
```

Every key of this branch, a page at a time. No snapshot is held between
pages, so a slow loop keeps no reader, and a key written meanwhile may
or may not be met.

### Bucket.under

```rust
pub fn under(&self, owner: impl Key) -> Bucket<V>
```

The same bucket seen from the branch `owner` names under this one:
`sessions.under(user_id).clear()` signs a user out everywhere.

### Bucket.has

```rust
pub fn has(&self, key: impl Key) -> Result<bool>
```

### Bucket.delete

```rust
pub fn delete(&self, key: impl Key) -> Result<bool>
```

Removes `key` and says whether a live key was there.

### Bucket.expire

```rust
pub fn expire(&self, key: impl Key, ttl: Duration) -> Result<bool>
```

Gives a live key a new expiry, `ttl` from now, as Redis's EXPIRE does,
keeping its value and version; says whether the key was there.

### Bucket.clear

```rust
pub fn clear(&self) -> Result<()>
```

Removes every key of this branch and of the branches under it, at once
for every reader, however many there are.

### Bucket.add

```rust
pub fn add(&self, key: impl Key) -> Result<bool>
```

Adds `key` to a bucket of keys alone, a set, and says whether it was new:
`seen.add(event_id)` is false for an event already handled.

## BucketBuilder

```rust
pub struct BucketBuilder<V> { /* private fields */ }
```

A bucket being opened: its name, its type, how its keys expire, whether
its values are encrypted.

### BucketBuilder.ttl

```rust
pub fn ttl(self, ttl: Duration) -> Self
```

Keys expire this long after they are written, unless a write says
otherwise.

### BucketBuilder.idle

```rust
pub fn idle(self, idle: Duration) -> Self
```

Keys expire this long after they were last read or written.

### BucketBuilder.encrypted

```rust
pub fn encrypted(self) -> Self
```

Keeps every value sealed with the store's encryption key, as a password
or a token is kept: the file holds what nobody reads without the key,
and a value copied under another key does not open. A key is not
sealed, so a secret is a value and never a key. A name keeps whether
it is encrypted: it opens the same way every time.

```rust
let passwords = store.bucket::<String>("source-passwords").encrypted().open()?;
passwords.set("source-42", &"hunter2".to_owned())?;
```

### BucketBuilder.open

```rust
pub fn open(self) -> Result<Bucket<V>>
```

## Entry

```rust
pub struct Entry<V> {
    pub key: String,
    pub value: V,
    pub version: Version,
    pub expires_at: Option<SystemTime>,
}
```

A key's value with what a conditional write needs.

## PAGE_KEYS

```rust
pub const PAGE_KEYS: usize
```

Keys a page holds at most.

## Page

```rust
pub struct Page<V> {
    pub entries: Vec<Entry<V>>,
    pub next: Option<String>,
}
```

One page of a branch's keys, in the byte order of their text.

## Version

```rust
pub struct Version(_)
```

The revision of kv.db that wrote a value. It never repeats, across deletes,
expiry and restarts, and it compares only for equality.

## Counters

```rust
pub struct Counters { /* private fields */ }
```

Numbers by key that only add up; a counter that is not there holds 0.
Every handle on a name counts the same way, in the file or in memory.

### Counters.under

```rust
pub fn under(&self, owner: impl Key) -> Counters
```

The same counters seen from the branch `owner` names under these.

### Counters.add

```rust
pub fn add(&self, key: impl Key, n: i64) -> Result<i64>
```

Adds `n` to the counter under `key` and returns what it holds now. A
counter that is not there starts from 0, its ttl from now; a sum past
the range of an `i64` is `Limit` and changes nothing.

### Counters.get

```rust
pub fn get(&self, key: impl Key) -> Result<i64>
```

What the counter under `key` holds; 0 when it is not there.

### Counters.delete

```rust
pub fn delete(&self, key: impl Key) -> Result<bool>
```

Removes the counter under `key` and says whether it was there.

### Counters.clear

```rust
pub fn clear(&self) -> Result<()>
```

Removes every counter of this branch and of the branches under it, at
once for every reader, however many there are.

## CountersBuilder

```rust
pub struct CountersBuilder { /* private fields */ }
```

Counters being opened: their name, how long a counter lasts, and whether
they are kept in memory between flushes.

### CountersBuilder.ttl

```rust
pub fn ttl(self, ttl: Duration) -> Self
```

A counter lasts this long from its first add, then starts again from 0:
the adds after the first do not move it.

### CountersBuilder.flush_every

```rust
pub fn flush_every(self, span: Duration) -> Self
```

Keeps the counters in memory and writes what changed once a span, so
that an add costs no commit and a crash loses at most the last span.
Without it each add is written before it returns.

### CountersBuilder.open

```rust
pub fn open(self) -> Result<Counters>
```

## Maintenance

```rust
pub struct Maintenance {
    pub expired: usize,
    pub cleared: usize,
    pub renewed: usize,
    pub flushed: usize,
}
```

What one maintenance pass did.

## KeyCall

```rust
pub struct KeyCall<'a, V> { /* private fields */ }
```

A call on one key with what it asks beside its value, run by its last step:
`sessions.key(&token).if_version(seen).set(&next)`.

### KeyCall.ttl

```rust
pub fn ttl(self, ttl: Duration) -> Self
```

The key expires `ttl` from now, for `set`, `create` and `expire`.

### KeyCall.expires_at

```rust
pub fn expires_at(self, time: SystemTime) -> Self
```

The key expires at `time`, for `set`, `create` and `expire`.

### KeyCall.if_version

```rust
pub fn if_version(self, version: Version) -> Self
```

The call applies only to a live key still at `version`: a key written,
expired or deleted since is a `Conflict`.

### KeyCall.set

```rust
pub fn set(self, value: &V) -> Result<()>
```

### KeyCall.create

```rust
pub fn create(self, value: &V) -> Result<bool>
```

Writes only when the key is not there, and says whether it did.

### KeyCall.take

```rust
pub fn take(self) -> Result<Option<V>>
```

### KeyCall.delete

```rust
pub fn delete(self) -> Result<bool>
```

Removes the key and says whether a live key was there.

### KeyCall.expire

```rust
pub fn expire(self) -> Result<bool>
```

Gives a live key the expiry `ttl` or `expires_at` named, keeping its
value and version, and says whether it was there.

## Once

```rust
pub struct Once<V> { /* private fields */ }
```

Functions run at most once a key, their answers kept: a charge, an import,
a webhook's effect, done once however often the request arrives.

### Once.under

```rust
pub fn under(&self, owner: impl Key) -> Once<V>
```

The same answers seen from the branch `owner` names under these.

### Once.run

```rust
pub fn run<E: From<Error>>(
    &self,
    key: impl Key,
    work: impl FnOnce() -> Result<V, E>,
) -> Result<V, E>
```

The answer kept under `key`, or `work`'s, which is then kept.

While another run of the key is under way, in this process or through
its server, this one waits for it and returns its answer. An error keeps
nothing, so the next run runs again; an answer meant to be kept, a card
declined, is a value rather than an error. An answer that could not be
kept is still returned, and the failure logged.

### Once.get

```rust
pub fn get(&self, key: impl Key) -> Result<Option<V>>
```

The answer kept under `key`.

### Once.delete

```rust
pub fn delete(&self, key: impl Key) -> Result<bool>
```

Forgets the answer kept under `key`, so that the next run runs again;
says whether there was one.

## OnceBuilder

```rust
pub struct OnceBuilder<V> { /* private fields */ }
```

Answers being opened: their name and type, and how long one is kept.

### OnceBuilder.keep

```rust
pub fn keep(self, keep: Duration) -> Self
```

How long an answer is kept from when it was made; a day unless said.
After that, a run of its key runs again.

### OnceBuilder.open

```rust
pub fn open(self) -> Result<Once<V>>
```

## Key

```rust
pub trait Key
```

What names a key, an owner or a folder: text, or an integer spelled in
decimal, so that `42` and `"42"` are one key.

### Key.text

```rust
fn text(&self) -> Cow<'_, str>
```

## Quota

```rust
pub struct Quota { /* private fields */ }
```

Uses a key may make in each of its windows, such as 100 messages every five
hours and 300 a week, counted by one `allow`.

A window starts at a key's first use after the last one ended and lasts its
span, so each key resets on its own:

```text
window("session", 100, 5 h): first use 13:42 → resets at 18:42;
                             a use at 19:07 starts the next, which resets at 00:07
```

An `allow` counts in every window or in none, in one durable write: two
quotas asked one after the other would leave the first counted when the
second refused.

### Quota.under

```rust
pub fn under(&self, owner: impl Key) -> Quota
```

The same quota seen from the branch `owner` names under this one, whose
keys are counted apart.

### Quota.allow

```rust
pub fn allow(&self, key: impl Key) -> Result<Allowance>
```

Uses one of each of `key`'s windows, or none when one has no room left.
`retry_at` is when the windows that refused reset.

### Quota.allow_n

```rust
pub fn allow_n(&self, key: impl Key, n: u64) -> Result<Allowance>
```

Uses `n` of each of `key`'s windows at once, or none when one has no
room for `n`. More than a window's limit never passes, and is `Invalid`.

### Quota.peek

```rust
pub fn peek(&self, key: impl Key) -> Result<Allowance>
```

`key`'s windows, using nothing: `ok` says whether one more use would
pass now.

### Quota.refund

```rust
pub fn refund(&self, key: impl Key) -> Result<()>
```

Gives one use back to each of `key`'s windows that has not reset since.

### Quota.refund_n

```rust
pub fn refund_n(&self, key: impl Key, n: u64) -> Result<()>
```

Gives `n` uses back, never below nothing.

### Quota.reset

```rust
pub fn reset(&self, key: impl Key) -> Result<()>
```

Forgets `key`'s windows, so that its next use starts each anew.

## QuotaBuilder

```rust
pub struct QuotaBuilder { /* private fields */ }
```

A quota being opened: its name and its windows.

### QuotaBuilder.window

```rust
pub fn window(self, name: &str, limit: u64, span: Duration) -> Self
```

Lets a key use up to `limit` every `span` from its first use. A name is
a lowercase letter, then up to 31 of them, digits and '_'; a quota has
one to eight windows.

### QuotaBuilder.open

```rust
pub fn open(self) -> Result<Quota>
```

## RateLimit

```rust
pub struct RateLimit { /* private fields */ }
```

Requests let through at a steady rate a key, with room for a burst.

It is the generic cell rate algorithm: a key holds one time, when its next
request is due, so no window has an edge that lets twice the rate through,
and a key costs one number.

```text
rate(3, 3 s), burst 3: three pass at once, and the fourth may pass 1 s later
```

The times live in memory and reach kv.db every second and at close: a
crash forgets at most the last second, which lets at most one burst more
through. A key whose time has come is as one never seen.

### RateLimit.under

```rust
pub fn under(&self, owner: impl Key) -> RateLimit
```

The same rate limit seen from the branch `owner` names under this one,
whose keys are counted apart.

### RateLimit.allow

```rust
pub fn allow(&self, key: impl Key) -> Result<Allowance>
```

Asks for one request of `key`.

### RateLimit.allow_n

```rust
pub fn allow_n(&self, key: impl Key, n: u64) -> Result<Allowance>
```

Asks for `n` requests of `key` at once: all of them pass or none does.
More than the burst never passes, and is `Invalid`.

### RateLimit.peek

```rust
pub fn peek(&self, key: impl Key) -> Result<Allowance>
```

Whether one request of `key` would pass now, using nothing.

### RateLimit.reset

```rust
pub fn reset(&self, key: impl Key) -> Result<()>
```

Forgets `key`'s requests, so that its next one has the whole burst.

## RateLimitBuilder

```rust
pub struct RateLimitBuilder { /* private fields */ }
```

A rate limit being opened: its name, its rate and its burst.

### RateLimitBuilder.rate

```rust
pub fn rate(self, count: u64, per: Duration) -> Self
```

Lets `count` requests a key through every `per`.

### RateLimitBuilder.burst

```rust
pub fn burst(self, burst: u64) -> Self
```

How many requests may pass at once before the rate holds the next; the
rate's count unless said.

### RateLimitBuilder.open

```rust
pub fn open(self) -> Result<RateLimit>
```

## TxBucket

```rust
pub struct TxBucket<'a, V> { /* private fields */ }
```

A bucket's calls inside a transaction.

### TxBucket.get

```rust
pub fn get(&self, key: impl Key) -> Result<Option<V>>
```

### TxBucket.entry

```rust
pub fn entry(&self, key: impl Key) -> Result<Option<Entry<V>>>
```

### TxBucket.has

```rust
pub fn has(&self, key: impl Key) -> Result<bool>
```

### TxBucket.set

```rust
pub fn set(&self, key: impl Key, value: &V) -> Result<()>
```

### TxBucket.create

```rust
pub fn create(&self, key: impl Key, value: &V) -> Result<bool>
```

### TxBucket.take

```rust
pub fn take(&self, key: impl Key) -> Result<Option<V>>
```

Takes `key`; a value that no longer reads as the bucket's type is
`Corrupt`, and stays, and the transaction goes on.

### TxBucket.delete

```rust
pub fn delete(&self, key: impl Key) -> Result<bool>
```

### TxBucket.expire

```rust
pub fn expire(&self, key: impl Key, ttl: Duration) -> Result<bool>
```

### TxBucket.key

```rust
pub fn key(&self, key: impl Key) -> KeyCall<'a, V>
```

A call on `key` with what it asks beside its value, inside the
transaction: `tx.with(&sessions).key(&token).if_version(seen).set(&next)`.

### TxBucket.clear

```rust
pub fn clear(&self) -> Result<()>
```

Removes every key of the bucket's branch and of the branches under it.

### TxBucket.list

```rust
pub fn list(&self, limit: usize, after: Option<&str>) -> Result<Page<V>>
```

One page of the branch's own keys, as `Bucket::list` reads it.

## TxCounters

```rust
pub struct TxCounters<'a> { /* private fields */ }
```

Counters' calls inside a transaction. Counters kept in memory join no
transaction: their calls through it are `Invalid`.

### TxCounters.add

```rust
pub fn add(&self, key: impl Key, n: i64) -> Result<i64>
```

### TxCounters.get

```rust
pub fn get(&self, key: impl Key) -> Result<i64>
```

### TxCounters.delete

```rust
pub fn delete(&self, key: impl Key) -> Result<bool>
```

## Bytes

```rust
pub struct Bytes(pub Vec<u8>)
```

Bytes kept as they are rather than as a JSON list of numbers.

## Value

```rust
pub trait Value
```

A type a bucket can keep: anything serde reads and writes.

## maintain

```rust
pub fn maintain(store: &Store) -> Result<Maintenance>
```

Writes what kv keeps in memory, then deletes what expired and what large
clears hid. The store runs it every minute, and writes memory every second;
a store opened without background work leaves both to the caller.

<!-- Generated by task reference from crates/tinystore through rustdoc. Edit the doc comments there, not this file. -->
