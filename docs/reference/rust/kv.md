# KV API for Rust

Every public type, trait and function of the KV engine in `tinystore::kv` and `tinystore::aio::kv`, generated from its source. The [KV guide](../../guides/kv/README.md) explains how to use them, and the [Bun and Node](../bun/kv.md), [Python](../python/kv.md) and [Go](../go/kv.md) pages list the same API.

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

## aio::Store

```rust
impl aio::Store
```

### aio::Store.bucket

```rust
pub fn bucket<V: Value>(&self, name: &str) -> BucketBuilder<V>
```

A bucket of values of type `V` by key, in the store's kv.db, whose
calls are awaited.

### aio::Store.counters

```rust
pub fn counters(&self, name: &str) -> CountersBuilder
```

Numbers by key that add up, in the store's kv.db, whose calls are
awaited.

### aio::Store.once

```rust
pub fn once<V: Value>(&self, name: &str) -> OnceBuilder<V>
```

Functions run once a key, their answers kept in the store's kv.db, as
`crate::Store::once` keeps them, the functions here async.

### aio::Store.quota

```rust
pub fn quota(&self, name: &str) -> QuotaBuilder
```

Uses a key may make in windows that each reset on their own, in the
store's kv.db, whose calls are awaited.

### aio::Store.rate_limit

```rust
pub fn rate_limit(&self, name: &str) -> RateLimitBuilder
```

Requests let through at a steady rate a key, with room for a burst, in
the store's kv.db, whose calls are awaited.

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
kept is still returned, to this caller and to those that waited for this
run, and the failure logged.

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

## aio::kv::All

```rust
pub struct All<V> { /* private fields */ }
```

Every key of a branch, a page at a time: a `Stream` of entries, which
ends once the last page is read, or after the error that stopped it, a
store closed included.

## aio::kv::Bucket

```rust
pub struct Bucket<V> { /* private fields */ }
```

Values of one type by key, their calls awaited: the same bucket as
`crate::kv::Bucket`, cheap to clone and to keep.

### aio::kv::Bucket.set

```rust
pub fn set(&self, key: impl Key, value: &V) -> impl Future<Output = Result<()>> + Send + 'static
```

Writes `value` under `key`, as `crate::kv::Bucket::set` does. The
value is encoded when the call is made.

### aio::kv::Bucket.create

```rust
pub fn create(
    &self,
    key: impl Key,
    value: &V,
) -> impl Future<Output = Result<bool>> + Send + 'static
```

Writes `value` only when `key` is not there, and says whether it did.

### aio::kv::Bucket.key

```rust
pub fn key(&self, key: impl Key) -> KeyCall<'_, V>
```

A call on `key` with what it asks beside its value, run by its last
step: `sessions.key(&token).if_version(seen).set(&next).await`.

### aio::kv::Bucket.get

```rust
pub fn get(&self, key: impl Key) -> impl Future<Output = Result<Option<V>>> + Send + 'static
```

The value under `key`. A key of an idle bucket read in its last
minute is renewed before the read answers: that read runs on the
store's pool, where it may wait for the writer.

### aio::kv::Bucket.entry

```rust
pub fn entry(
    &self,
    key: impl Key,
) -> impl Future<Output = Result<Option<Entry<V>>>> + Send + 'static
```

The value with its version and expiry, read as `get` reads it.

### aio::kv::Bucket.has

```rust
pub fn has(&self, key: impl Key) -> impl Future<Output = Result<bool>> + Send + 'static
```

Whether `key` is there, read as `get` reads it.

### aio::kv::Bucket.take

```rust
pub fn take(&self, key: impl Key) -> impl Future<Output = Result<Option<V>>> + Send + 'static
```

Reads `key` and removes it in one commit: of two callers taking the
same key, one gets the value. A value that no longer reads as the
bucket's type is `Corrupt`, and stays.

### aio::kv::Bucket.delete

```rust
pub fn delete(&self, key: impl Key) -> impl Future<Output = Result<bool>> + Send + 'static
```

Removes `key` and says whether a live key was there.

### aio::kv::Bucket.expire

```rust
pub fn expire(
    &self,
    key: impl Key,
    ttl: Duration,
) -> impl Future<Output = Result<bool>> + Send + 'static
```

Gives a live key a new expiry, `ttl` from now, keeping its value and
version; says whether the key was there.

### aio::kv::Bucket.clear

```rust
pub fn clear(&self) -> impl Future<Output = Result<()>> + Send + 'static
```

Removes every key of this branch and of the branches under it, at once
for every reader, however many there are.

### aio::kv::Bucket.list

```rust
pub fn list(
    &self,
    limit: usize,
    after: Option<&str>,
) -> impl Future<Output = Result<Page<V>>> + Send + 'static
```

One page of this branch's own keys, after `after` when it continues
another, as `crate::kv::Bucket::list` reads it.

### aio::kv::Bucket.all

```rust
pub fn all(&self) -> All<V>
```

Every key of this branch, a page at a time, as a `Stream`. No
snapshot is held between pages, so a slow loop keeps no reader, and a
key written meanwhile may or may not be met. Dropped, it stops the
page under way and reads no more.

### aio::kv::Bucket.under

```rust
pub fn under(&self, owner: impl Key) -> Bucket<V>
```

The same bucket seen from the branch `owner` names under this one:
`sessions.under(user_id).clear().await` signs a user out everywhere.

### aio::kv::Bucket.blocking

```rust
pub fn blocking(&self) -> &Bucket<V>
```

The same bucket, for its blocking calls.

### aio::kv::Bucket.add

```rust
pub fn add(&self, key: impl Key) -> impl Future<Output = Result<bool>> + Send + 'static
```

Adds `key` to a bucket of keys alone, a set, and says whether it was
new: `seen.add(event_id).await` is false for an event already handled.

## aio::kv::BucketBuilder

```rust
pub struct BucketBuilder<V> { /* private fields */ }
```

A bucket being opened, as `crate::kv::BucketBuilder` is: its name, its
type, how its keys expire, whether its values are encrypted.

### aio::kv::BucketBuilder.ttl

```rust
pub fn ttl(self, ttl: Duration) -> Self
```

Keys expire this long after they are written, unless a write says
otherwise.

### aio::kv::BucketBuilder.idle

```rust
pub fn idle(self, idle: Duration) -> Self
```

Keys expire this long after they were last read or written.

### aio::kv::BucketBuilder.encrypted

```rust
pub fn encrypted(self) -> Self
```

Keeps every value sealed with the store's encryption key, as
`crate::kv::BucketBuilder::encrypted` does.

### aio::kv::BucketBuilder.open

```rust
pub fn open(self) -> Result<Bucket<V>>
```

Opens the bucket: a plain call, made once at start, since the engine's
first handle may migrate kv.db.

## aio::kv::KeyCall

```rust
pub struct KeyCall<'a, V> { /* private fields */ }
```

A call on one key with what it asks beside its value, run by its last
step, awaited: `sessions.key(&token).if_version(seen).set(&next).await`.

### aio::kv::KeyCall.ttl

```rust
pub fn ttl(self, ttl: Duration) -> Self
```

The key expires `ttl` from now, for `set`, `create` and `expire`.

### aio::kv::KeyCall.expires_at

```rust
pub fn expires_at(self, time: SystemTime) -> Self
```

The key expires at `time`, for `set`, `create` and `expire`.

### aio::kv::KeyCall.if_version

```rust
pub fn if_version(self, version: Version) -> Self
```

The call applies only to a live key still at `version`: a key written,
expired or deleted since is a `Conflict`.

### aio::kv::KeyCall.set

```rust
pub fn set(self, value: &V) -> impl Future<Output = Result<()>> + Send + 'static
```

### aio::kv::KeyCall.create

```rust
pub fn create(self, value: &V) -> impl Future<Output = Result<bool>> + Send + 'static
```

Writes only when the key is not there, and says whether it did.

### aio::kv::KeyCall.take

```rust
pub fn take(self) -> impl Future<Output = Result<Option<V>>> + Send + 'static
```

### aio::kv::KeyCall.delete

```rust
pub fn delete(self) -> impl Future<Output = Result<bool>> + Send + 'static
```

Removes the key and says whether a live key was there.

### aio::kv::KeyCall.expire

```rust
pub fn expire(self) -> impl Future<Output = Result<bool>> + Send + 'static
```

Gives a live key the expiry `ttl` or `expires_at` named, keeping its
value and version, and says whether it was there.

## aio::kv::Counters

```rust
pub struct Counters { /* private fields */ }
```

Numbers by key that only add up, their calls awaited: the same counters
as `crate::kv::Counters`.

What the builder chose decides where a call runs, never the call:
counters written each add queue at kv.db's writer, and counters kept in
memory step there in the first poll.

On a node that copies kv.db, an add of counters written each add is queued
on that node and answered at once, on the store's pool, and a `get` reads
the node's copy there. Every other add, and each `delete` and `clear`,
passes on to the file's home and waits for its answer on a thread of its
own.

### aio::kv::Counters.add

```rust
pub fn add(&self, key: impl Key, n: i64) -> impl Future<Output = Result<i64>> + Send + 'static
```

Adds `n` to the counter under `key` and returns what it holds now, as
`crate::kv::Counters::add` does. Counters kept in memory answer in
the first poll, unless a key memory does not hold yet is read or
memory must flush first: that step runs on the store's pool, and once
polled it counts whether or not its future is still awaited.

### aio::kv::Counters.get

```rust
pub fn get(&self, key: impl Key) -> impl Future<Output = Result<i64>> + Send + 'static
```

What the counter under `key` holds; 0 when it is not there.

### aio::kv::Counters.delete

```rust
pub fn delete(&self, key: impl Key) -> impl Future<Output = Result<bool>> + Send + 'static
```

Removes the counter under `key` and says whether it was there.

### aio::kv::Counters.clear

```rust
pub fn clear(&self) -> impl Future<Output = Result<()>> + Send + 'static
```

Removes every counter of this branch and of the branches under it.
Counters kept in memory hold that memory across the clear's commit,
so their clear runs on the store's pool, to its end once polled.

### aio::kv::Counters.under

```rust
pub fn under(&self, owner: impl Key) -> Counters
```

The same counters seen from the branch `owner` names under these.

### aio::kv::Counters.blocking

```rust
pub fn blocking(&self) -> &Counters
```

The same counters, for their blocking calls.

## aio::kv::CountersBuilder

```rust
pub struct CountersBuilder { /* private fields */ }
```

Counters being opened, as `crate::kv::CountersBuilder` are: their
name, how long a counter lasts, and whether they are kept in memory
between flushes.

### aio::kv::CountersBuilder.ttl

```rust
pub fn ttl(self, ttl: Duration) -> Self
```

A counter lasts this long from its first add, then starts again from 0.

### aio::kv::CountersBuilder.flush_every

```rust
pub fn flush_every(self, span: Duration) -> Self
```

Keeps the counters in memory and writes what changed once a span: an
add is then a step in memory, answered in its first poll.

### aio::kv::CountersBuilder.open

```rust
pub fn open(self) -> Result<Counters>
```

Opens the counters: a plain call, made once at start.

## aio::kv::Once

```rust
pub struct Once<V> { /* private fields */ }
```

Functions run at most once a key, their answers kept, the functions
async: the same answers as `crate::kv::Once`, whose runs and those of
the blocking API wait for one another.

### aio::kv::Once.run

```rust
pub fn run<K, E, W, F>(
    &self,
    key: K,
    work: W,
) -> impl Future<Output = Result<V, E>> + Send + use<K, V, E, W, F>
where
    K: Key,
    E: From<Error> + Send,
    W: FnOnce() -> F + Send,
    F: Future<Output = Result<V, E>> + Send,
```

The answer kept under `key`, or the answer of the future `work`
gives, which is then kept, as `crate::kv::Once::run` keeps it:

```rust
let charges = store.once::<String>("charges").open()?;
let order = "order-7".to_owned();
let receipt = charges.run("key-7f", || charge(&order)).await?; // charged once, however often it is sent
```

While another run of the key is under way, in this process or through
its server, this one waits for it with no thread held, and returns its
answer. On a node that copies kv.db, the run claims its key at the
file's home, which waits for the run under way there, and this run
waits for the home's answer on a thread of its own.

`work` is called once the key is claimed, and its future is polled
inside this one's. The future is `Send`, and `'static` when `key`,
`work` and its future are, as `tokio::spawn` asks.

Dropped while it waits, the run leaves and nothing changes; while its
function runs, the key is let go and nothing is kept, so the next run
runs again; once the function answered, its answer is kept all the
same and the key let go after it. A run of a key inside its own run,
awaited or blocking, is `Invalid`.

### aio::kv::Once.get

```rust
pub fn get(&self, key: impl Key) -> impl Future<Output = Result<Option<V>>> + Send + 'static
```

The answer kept under `key`.

### aio::kv::Once.delete

```rust
pub fn delete(&self, key: impl Key) -> impl Future<Output = Result<bool>> + Send + 'static
```

Forgets the answer kept under `key`, so that the next run runs again;
says whether there was one.

### aio::kv::Once.under

```rust
pub fn under(&self, owner: impl Key) -> Once<V>
```

The same answers seen from the branch `owner` names under these.

### aio::kv::Once.blocking

```rust
pub fn blocking(&self) -> &Once<V>
```

The same answers, for their blocking calls.

## aio::kv::OnceBuilder

```rust
pub struct OnceBuilder<V> { /* private fields */ }
```

Answers being opened, as `crate::kv::OnceBuilder` are: their name and
type, and how long one is kept.

### aio::kv::OnceBuilder.keep

```rust
pub fn keep(self, keep: Duration) -> Self
```

How long an answer is kept from when it was made; a day unless said.

### aio::kv::OnceBuilder.open

```rust
pub fn open(self) -> Result<Once<V>>
```

Opens the answers: a plain call, made once at start.

## aio::kv::Quota

```rust
pub struct Quota { /* private fields */ }
```

Uses a key may make in each of its windows, their calls awaited: the same
quota as `crate::kv::Quota`. An `allow` counts in every window or in
none, in one durable write.

### aio::kv::Quota.allow

```rust
pub fn allow(&self, key: impl Key) -> impl Future<Output = Result<Allowance>> + Send + 'static
```

Uses one of each of `key`'s windows, or none when one has no room left.

### aio::kv::Quota.allow_n

```rust
pub fn allow_n(
    &self,
    key: impl Key,
    n: u64,
) -> impl Future<Output = Result<Allowance>> + Send + 'static
```

Uses `n` of each of `key`'s windows at once, or none when one has no
room for `n`. More than a window's limit never passes, and is `Invalid`.

### aio::kv::Quota.peek

```rust
pub fn peek(&self, key: impl Key) -> impl Future<Output = Result<Allowance>> + Send + 'static
```

`key`'s windows, using nothing: `ok` says whether one more use would
pass now.

### aio::kv::Quota.refund

```rust
pub fn refund(&self, key: impl Key) -> impl Future<Output = Result<()>> + Send + 'static
```

Gives one use back to each of `key`'s windows that has not reset since.

### aio::kv::Quota.refund_n

```rust
pub fn refund_n(&self, key: impl Key, n: u64) -> impl Future<Output = Result<()>> + Send + 'static
```

Gives `n` uses back, never below nothing.

### aio::kv::Quota.reset

```rust
pub fn reset(&self, key: impl Key) -> impl Future<Output = Result<()>> + Send + 'static
```

Forgets `key`'s windows, so that its next use starts each anew.

### aio::kv::Quota.under

```rust
pub fn under(&self, owner: impl Key) -> Quota
```

The same quota seen from the branch `owner` names under this one,
whose keys are counted apart.

### aio::kv::Quota.blocking

```rust
pub fn blocking(&self) -> &Quota
```

The same quota, for its blocking calls.

## aio::kv::QuotaBuilder

```rust
pub struct QuotaBuilder { /* private fields */ }
```

A quota being opened, as `crate::kv::QuotaBuilder` is: its name and its
windows.

### aio::kv::QuotaBuilder.window

```rust
pub fn window(self, name: &str, limit: u64, span: Duration) -> Self
```

Lets a key use up to `limit` every `span` from its first use.

### aio::kv::QuotaBuilder.open

```rust
pub fn open(self) -> Result<Quota>
```

Opens the quota: a plain call, made once at start.

## aio::kv::RateLimit

```rust
pub struct RateLimit { /* private fields */ }
```

Requests let through at a steady rate a key, with room for a burst, their
calls awaited: the same rate limit as `crate::kv::RateLimit`.

Its times live in memory, so each call is a step there, answered in its
first poll. A key memory does not hold yet is read from kv.db first, on
the store's pool when the read would wait in the poll, and an `allow` or
a `reset` polled once applies whether or not its future is still awaited.

On a node that copies kv.db, `allow` and `reset` pass on to the file's
home and wait for its answer on a thread of its own, and `peek` reads the
node's copy.

### aio::kv::RateLimit.allow

```rust
pub fn allow(&self, key: impl Key) -> impl Future<Output = Result<Allowance>> + Send + 'static
```

Asks for one request of `key`.

### aio::kv::RateLimit.allow_n

```rust
pub fn allow_n(
    &self,
    key: impl Key,
    n: u64,
) -> impl Future<Output = Result<Allowance>> + Send + 'static
```

Asks for `n` requests of `key` at once: all of them pass or none does.
More than the burst never passes, and is `Invalid`.

### aio::kv::RateLimit.peek

```rust
pub fn peek(&self, key: impl Key) -> impl Future<Output = Result<Allowance>> + Send + 'static
```

Whether one request of `key` would pass now, using nothing.

### aio::kv::RateLimit.reset

```rust
pub fn reset(&self, key: impl Key) -> impl Future<Output = Result<()>> + Send + 'static
```

Forgets `key`'s requests, so that its next one has the whole burst.

### aio::kv::RateLimit.under

```rust
pub fn under(&self, owner: impl Key) -> RateLimit
```

The same rate limit seen from the branch `owner` names under this one,
whose keys are counted apart.

### aio::kv::RateLimit.blocking

```rust
pub fn blocking(&self) -> &RateLimit
```

The same rate limit, for its blocking calls.

## aio::kv::RateLimitBuilder

```rust
pub struct RateLimitBuilder { /* private fields */ }
```

A rate limit being opened, as `crate::kv::RateLimitBuilder` is: its
name, its rate and its burst.

### aio::kv::RateLimitBuilder.rate

```rust
pub fn rate(self, count: u64, per: Duration) -> Self
```

Lets `count` requests a key through every `per`.

### aio::kv::RateLimitBuilder.burst

```rust
pub fn burst(self, burst: u64) -> Self
```

How many requests may pass at once before the rate holds the next; the
rate's count unless said.

### aio::kv::RateLimitBuilder.open

```rust
pub fn open(self) -> Result<RateLimit>
```

Opens the rate limit: a plain call, made once at start.

<!-- Generated by task reference from crates/tinystore through rustdoc. Edit the doc comments there, not this file. -->
