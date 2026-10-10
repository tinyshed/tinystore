# kv: the API book

Draft, 9 October 2026, after two newcomer checks and an independent review
(below). The application's current state: sessions, one-time codes, flags,
the answers of requests sent twice, counters and limits. This book is the API
before the code; [dx.md](../dx.md) has the rules it follows. TypeScript comes
first in each section because most callers will read it; Python, Go and Rust
follow when they spell something differently. The Rust column is built
(`crates/tinystore/src/kv`), all but a bucket opened from an SQL database,
which comes with sqldb in phase 2.

## What a newcomer learns

| Concept                 | In one line                                                                            |
|-------------------------|----------------------------------------------------------------------------------------|
| bucket                  | a named collection of values of one type, by key                                       |
| key                     | text: a string, or a number spelled in decimal                                         |
| `ttl` and `idle`        | a key expires a span after it was written, or a span after it was last read or written |
| version                 | what a key was when you read it, to write only if nobody changed it since              |
| `under`                 | a branch of a bucket, such as one user's sessions, cleared in one call                 |
| counters                | numbers by key that only add up                                                        |
| `rateLimit` and `quota` | may this request pass: a smooth rate, or uses in windows that reset                    |
| `once`                  | run a function once a key and keep its answer, for requests sent twice                 |

Everything opens from the store, named by what it is: `store.bucket`,
`store.counters`, `store.rateLimit`, `store.quota`, `store.once`, as
`store.queue` will for jobs. Where it lives is said by what opened it: the
store keeps these in its own kv.db, and the same calls on an SQL database,
`db.bucket('sessions')`, keep them in that database's file, so that they
commit with its rows (decision 15).

## Open a bucket

```ts
const sessions = store.bucket<Session>('sessions', { idle: '30d' })
const codes = store.bucket<number>('login-codes', { ttl: '15m', type: 'int' })
const seen = store.bucket('stripe-events', { ttl: '7d' })      // keys alone: a set
```

```python
sessions = store.bucket("sessions", Session, idle="30d")
codes = store.bucket("login-codes", int, ttl="15m")
seen = store.bucket("stripe-events", ttl="7d")
```

```go
sessions, err := kv.Bucket[Session](store, "sessions", kv.Idle(30*24*time.Hour))
codes, err := kv.Bucket[int64](store, "login-codes", kv.TTL(15*time.Minute))
seen, err := kv.Bucket[struct{}](store, "stripe-events", kv.TTL(7*24*time.Hour))
```

```rust
let sessions = store.bucket::<Session>("sessions").idle(Duration::from_hours(24 * 30)).open()?;
let codes = store.bucket::<i64>("login-codes").ttl(Duration::from_mins(15)).open()?;
let seen = store.bucket::<()>("stripe-events").ttl(Duration::from_hours(24 * 7)).open()?;
```

- A bucket's name is `[a-z0-9][a-z0-9_-]{0,63}` and keeps its role: a name
  that holds counters does not open as a bucket of values.
- `ttl` and `idle` are the default of every key written without its own; they
  do not go together.
- A value is kept by its type: text and bytes as they are, integers and
  booleans as integers, floats bit for bit (`-0`, NaN payloads and infinities
  included), nothing for a set, JSON for the rest. TypeScript loses the type at
  run time, so `type` names it for the few that are not JSON: `'string'`,
  `'bytes'`, `'int'`, `'float'`, `'bool'`, and `'bigint'` for integers past
  what a number holds exactly. In Rust the type parameter is enough: anything
  serde reads and writes.
- Go spells it `kv.Bucket[T](store, …)` because a Go method takes no type
  parameter.

## Read and write

```ts
await sessions.under(user.id).set(digest(token), { user: user.id, device })
const session = await sessions.under(user.id).get(digest(token))   // Session | undefined
const userId = await codes.take(digest(code))          // read and remove in one step
const isNew = await seen.add(event.id)                 // false when it was there
const created = await sessions.under(user.id).create(digest(token), session)   // false when it was there
await sessions.under(user.id).expire(digest(token), '1h')   // expires an hour from now
```

```rust
sessions.under(user_id).set(&digest(&token), &session)?;
let session: Option<Session> = sessions.under(user_id).get(&digest(&token))?;
let user: Option<i64> = codes.take(&digest(&code))?;
```

- `set` writes; a key that exists keeps its expiry, an idle key lives its span
  from the write, and a call that gives an expiry sets it:
  `set(key, value, { ttl: '1h' })`, in Rust `bucket.key(k).ttl(d).set(&v)`.
- `get` is `undefined` (`None`, `nil`) for a key that is not there or expired.
- `add` puts a key in a set; `create(key, value)` writes a key only when it is
  not there. Both return `false` when it is there, and never throw for that.
- `take` reads a key and removes it in one commit: of two callers taking a
  one-time code at once, one gets it. A value that no longer reads as the
  bucket's type is `corrupt`, and stays.
- `expire(key, span)` gives a key a new expiry, as Redis's `EXPIRE` does: it
  may lengthen or shorten its life, and keeps its value.
- `delete` removes a key and says whether it was there.
- A write returns once it is durable; writes from many callers share a commit.
  A store opened with `durability: 'os'` returns it once the operating system
  has it, and a bucket kept in a database commits as that database does.
- A token or a code is a key through its digest: the file then holds nothing
  that signs anybody in, and an error that names the key names the digest.
- Every call blocks its thread in Rust, as file access does; async code runs it
  through `spawn_blocking`. The pipe's hosts never block: their calls are
  answered through the event loop.

## Versions

```ts
const entry = await settings.entry('home')         // { key, value, version, expiresAt } | undefined
await settings.set('home', next, { ifVersion: entry.version })   // ConflictError if it changed
```

```rust
let entry = settings.entry("home")?;               // Option<Entry { key, value, version, expires_at }>
settings.key("home").if_version(entry.version).set(&next)?;      // a Conflict error if it changed
```

- A version is the revision of kv.db that wrote the value; it never repeats,
  across deletes, expiry and restarts. It compares only for equality.
- `ifVersion` works on `set`, `take`, `delete` and `expire`. A key that changed,
  expired or was deleted since is a conflict. In TypeScript it takes a version
  and never `undefined`, so `{ ifVersion: entry?.version }` does not compile:
  an absent key would write without a check.
- To write only a key that is not there, use `create`.
- In Rust a call with options is a chain on its key that ends in its verb:
  `.key(k)`, then `.ttl(d)`, `.expires_at(t)` or `.if_version(v)`, then `.set`,
  `.create`, `.take`, `.delete` or `.expire`. A ttl on a take or a delete,
  which write no value, is `invalid`.

## Expiry

| Option            | What happens                                              |
|-------------------|-----------------------------------------------------------|
| `ttl: '15m'`      | the key expires 15 minutes after it was written           |
| `expiresAt: date` | the key expires at that time                              |
| `idle: '30d'`     | the key expires 30 days after it was last read or written |
| none              | the bucket's own, or never                                |

An expired key is gone for every call at once: reads miss it, `create` and
`add` take it, `ifVersion` conflicts with it. The store deletes its row later.
A `get`, `entry` or `has` of an `idle` key renews it once a thirtieth of the
span has passed, without waiting for a commit: the renewal is written with the
others a second later, or before the read returns when the key has less than a
minute left. `list` and `all` renew nothing, so a sweep over sessions keeps
none of them alive.

## Branches

```ts
await sessions.under(user.id).set(digest(token), session)
await sessions.under(user.id).clear()                  // signed out everywhere
for await (const { key, value } of sessions.under(user.id).all()) { … }
const page = await sessions.under(user.id).list({ limit: 100, after: page.next })   // { entries, next }
```

- `under(...owners)` names a branch: whose keys these are. A branch exists
  while it holds keys. Owners are kept apart from keys and from each other, so
  `under('a').under('b')` is never `under('a/b')`.
- `clear()` removes a branch and every branch under it, at once for every
  reader, however many keys it holds. On the bucket itself it empties the
  bucket.
- `all()` walks every key a page at a time, holding nothing between pages;
  `list()` reads one page, at most 1000 keys, in the byte order of the keys
  (`"10"` before `"9"`), and `next` is where the following page starts.

## Counters

```ts
const attempts = store.counters('login-attempts', { ttl: '15m' })
const n = await attempts.add(clientIp, 1)              // the new count: 1, 2, 3…
const hits = store.counters('page-hits', { flushEvery: '1s' })
```

```rust
let attempts = store.counters("login-attempts").ttl(Duration::from_mins(15)).open()?;
let n: i64 = attempts.add(&client_ip, 1)?;
let hits = store.counters("page-hits").flush_every(Duration::from_secs(1)).open()?;
```

- An absent or expired counter is 0. A counter's window starts with its first
  add and does not slide: 15 minutes after the first attempt it starts from 0.
  A lockout that should not reset on a clock's edge is a `rateLimit`.
- `flushEvery: '1s'` keeps counters in memory and writes them every second: a
  crash loses at most that second, and an add costs no commit. Without it every
  add is a durable write. A sum past what an int64 holds is `limit` and changes
  nothing.
- Counters kept in memory join no transaction; counters written each add do.

## Limits

```ts
const api = store.rateLimit('api', { rate: '100/s', burst: 20 })
const { ok, left, retryAt } = await api.allow(userId)   // retryAt: a Date

const ai = store.quota('ai', { daily: '100/1d', weekly: '300/7d' })
const answer = await ai.allow(userId)                   // counts in every window, or in none
await ai.refund(userId)
```

```rust
let api = store.rate_limit("api").rate(100, Duration::from_secs(1)).burst(20).open()?;
let allowed: kv::Allowance = api.allow(&user_id)?;
let ai = store
    .quota("ai")
    .window("daily", 100, Duration::from_hours(24))
    .window("weekly", 300, Duration::from_hours(24 * 7))
    .open()?;
let answer = ai.allow(&user_id)?;   // answer.window("weekly")
```

- `rateLimit` lets `rate` requests a key through on average and `burst` at
  once, with no window edge that lets twice the rate through. Its times live in
  memory and reach the file every second: a crash lets at most one burst more
  through.
- `quota` counts uses in each named window; a window starts at a key's first
  use and resets on its own. `allow` counts in all of them or in none.
- Both answer rather than throw, with one shape: `ok`, `left`, `retryAt`, the
  time a refused request may try again, and for a quota each window's `used`,
  `limit`, `left` and `resetsAt`. `peek` gives the answer one request would get
  and uses nothing; `reset` forgets a key, so that its next request starts
  afresh; a quota's `refund` gives uses back.
- More at once than the burst or a window's limit never passes, and is
  `invalid`.

## Once

```ts
const charges = store.once<Receipt>('charges', { keep: '1d' })
const receipt = await charges.run(requestId, () => pay.charge(order, requestId))
```

```rust
let charges = store.once::<Receipt>("charges").keep(Duration::from_hours(24)).open()?;
let receipt = charges.run(&request_id, || pay.charge(&order, &request_id))?;
```

- `run` returns the answer kept for the key, or runs the function and keeps
  what it returns for `keep`. After that the key runs again: pass the same id
  to the payment provider too.
- A run of the same key elsewhere, in this process or another, waits for it and
  returns its answer. A run of a key inside its own run is `invalid`, since it
  would wait for itself.
- An error keeps nothing: the next `run` runs again. A declined card that must
  not be charged again is an answer, not an error.
- In Rust the function's error is the caller's own type, which a `tinystore`
  error converts into: `run::<E: From<tinystore::Error>>`.
- A memo is `once` with a short `keep`: the callers that ask for a key at
  once, in any process, share one run of the function, and the next
  `keep` answer from what it kept.

```ts
const panels = store.once<Panel[]>('panels', { keep: '30s' })
const data = await panels.run(`dashboard:${id}`, () => computePanels(id))
```

## Transactions

```ts
await store.tx(async tx => {
	const left = (await tx.with(stock).get(sku)) ?? 0
	if (left < 1) throw new SoldOut()
	await tx.with(stock).set(sku, left - 1)
	await tx.with(orders).set(id, order)
})
```

```rust
let placed = store.tx(|tx| -> Result<bool, ShopError> {
    let left = tx.with(&stock).get(&sku)?.unwrap_or(0);
    if left < 1 {
        return Err(ShopError::SoldOut);   // rolls back
    }
    tx.with(&stock).set(&sku, &(left - 1))?;
    tx.with(&orders).set(&order_id, &order)?;
    Ok(true)                              // commits
})?;
```

- The transaction takes a handle in, `tx.with(handle)`, so that every call
  inside starts with `tx.`. A call made around it from inside, `stock.get(sku)`,
  is `invalid` in Rust: it would read the file without the transaction's own
  writes, or wait for the writer the transaction holds.
- `store.tx` is a transaction of one of the store's own files: kv.db for
  buckets and counters, jobs.db for queues, whichever its first handle is
  kept in. A handle of the other is `invalid`, since no write is atomic across
  two files; a program that commits keys and jobs together opens both from a
  database, `db.bucket` and `db.queue`, and uses `db.tx`.
- In Rust a transaction holds its file's writer alone while it reads and
  decides, so two orders cannot both take the last item; it commits when the
  function returns `Ok` and rolls back on an error or a panic. A call that
  fails inside it leaves the transaction as it was before the call.
- Across the pipe and the network a transaction never holds the writer while
  the host awaits: its reads go at once, its writes are sent together when the
  function returns, with a check that nothing it read has changed, and the
  function runs again when something did, five times at most before
  `conflict`. A read after a write in the same transaction sees the write. A
  write answers what the commit will do as the reads saw it: `create` and
  `add` say whether they will write, `take` and `delete` what was there, and
  a counter's `add` gives no count, which only the commit knows.
- Writes that need no reads need no transaction: separate calls already share
  commits.
- Python spells it `tx.with_(stock)`, `with` being a keyword there.

## Errors

| Error             | When                                                                                                  | What to do                        |
|-------------------|-------------------------------------------------------------------------------------------------------|-----------------------------------|
| `invalid`         | a bad name, key or option; a value the bucket cannot keep; a call around a transaction from inside it | fix the call                      |
| `conflict`        | `ifVersion` did not match                                                                             | read again and decide             |
| `limit`           | a value over 1 MiB, a counter past int64, the store's memory                                          | write less                        |
| `corrupt`         | a stored value does not read as the bucket's type                                                     | open the bucket with its type     |
| `closed`          | the store closed                                                                                      | open it again                     |
| `unavailable`     | another process holds the writer past the busy timeout                                                | try again                         |
| `outcome unknown` | the commit failed after the write ran                                                                 | read the key before writing again |

Every error names the bucket and the key, owners first:
`kv bucket sessions: key "42/9f86d0…": conflict`.

## Bounds

| What                               | Bound                                                     |
|------------------------------------|-----------------------------------------------------------|
| a key with its owners              | 1 KiB                                                     |
| a value                            | 1 MiB; over 512 bytes it has a row of its own             |
| a page                             | 1000 keys                                                 |
| a bucket's name                    | 64 bytes                                                  |
| a quota                            | one to eight windows                                      |
| counters and rate limits in memory | 100,000 keys a name; past them a change waits for a flush |

## Closing

`store.close()` writes what memory holds, counters and rate limits and the
renewals reads asked for, then lets go of the directory. In Rust a store whose
last clone is dropped closes the same way; a handle that outlives its store is
`closed`.

## Newcomer check, 9 October

A Haiku agent that had read nothing about TinyStore read 22 call sites. What
it got wrong or doubted, and what changed:

| It read                                                  | Change                                   |
|----------------------------------------------------------|------------------------------------------|
| `touch(key, { ttl })` as "extend", though it can shorten | `expire(key, span)`, Redis's word        |
| `of('ip')` as a dimension and `of(user.id)` as a scope   | `of` only names whose keys these are     |
| `sessions.set(token)` and then `of(user.id).clear()`     | the examples write through `of(user.id)` |
| `create` as maybe throwing on a key that is there        | `false`, never an error; `add` for sets  |
| `value: 'int'` as data                                   | `type: 'int'`                            |
| `retryAfter` without a unit                              | `retryAt`, a time                        |
| `max` on counters as a query                             | dropped                                  |
| `tx.get(stock, sku)` beside `stock.get(sku)`             | `stock.in(tx).get(sku)`                  |
| a quota window named `session` beside sessions           | `hourly`                                 |

`take`, `once` and `durability` were read right; their bullets above now say
what the agent asked about.

## Second check and review, 9 October

Three Haiku agents read the Rust and TypeScript drafts with no other context:
one said what each of 32 call sites does, one compared spellings blind, one
reviewed the whole API as a developer choosing a library.

| They found                                                                                   | Change                                                                                      |
|----------------------------------------------------------------------------------------------|---------------------------------------------------------------------------------------------|
| `set_with(k, v, kv::if_version(..))` the hardest line: nothing says the write can be refused | Rust's options are a chain on the key that ends in its verb: `key(k).if_version(v).set(&v)` |
| `of(owner)` read as a constructor, as `Array.of` is                                          | `under(owner)`                                                                              |
| `stock.within(tx)` read as a filter; a call outside the transaction easy to miss             | `tx.with(stock)`, the owner's idea too; a call around it is `invalid` in Rust               |
| `on` in `tx.on(stock)` read as subscribing to an event                                       | `with`                                                                                      |
| `check` on a quota is a spend in other limiters                                              | `peek`, on rate limits too                                                                  |
| `delete` on a quota, and no way to unblock a key of a rate limit                             | `reset` on both                                                                             |
| two answer types, `Allowance` and `Usage`, for one decision                                  | one `Allowance`, a quota's with its windows                                                 |
| a store dropped without close: are counters in memory lost?                                  | a real leak: the engine held the store, which never closed; now the last drop closes it     |
| a sweep over sessions keeping them alive                                                     | `list` and `all` renew nothing, and the book says so                                        |
| tokens as keys reach the file and the errors                                                 | the examples key by a digest                                                                |
| `idempotent` proposed for `once`, `answers`, `run_once`                                      | kept `once`: the owner finds `idempotent` heavy; `run_once` is open                         |
| `{ in: db }` read as a filter or a copy                                                      | `db.bucket(…)`, decision 15                                                                 |
| `store.kv.bucket` read as a second store                                                     | flat names, as drafted                                                                      |

Still open: whether `run` becomes `run_once`; whether a TypeScript `tx` runs its
function again only when asked, since a function with an effect outside the
store must not run twice; whether `once` takes a fingerprint of what it was
asked, so that a key reused with another order is a conflict, as Stripe's
idempotency keys are; `retryAt` or a span, `retryAfter`, which an HTTP header
wants; whether `durability: '1s'`, which a newcomer read as how long counts
are kept, needs another word.

## Third check, 9 October

A Haiku agent read ten TypeScript call sites of the SDK as built, with four
facts about it and nothing else.

| It found                                                                      | Change                                                                |
|-------------------------------------------------------------------------------|-----------------------------------------------------------------------|
| `{ ifVersion: entry?.version }` writes without a check when the key is absent | `ifVersion` takes a version, never `undefined`: it does not compile   |
| `durability: '1s'` read as how long counts are kept, by a second reader       | `flushEvery: '1s'`; `durability` is a file's mode, `'full'` or `'os'` |
| a read after a write in a transaction taken to see the old value              | the book says a read sees the transaction's own writes                |

Kept as they are, by the owner's leave: `{ ok }`, although
`if (await api.allow(id))` is always true, an answer being an object, as
Upstash's is; `expire(key, date)` beside `expire(key, '2h')`, a date being
unambiguous where Redis has `EXPIREAT`; `add`, which puts a key in a set as
`Set.add` does and adds to a counter as `AtomicLong` does.

## Was, in Go

| Was                                                     | Now                                                       |
|---------------------------------------------------------|-----------------------------------------------------------|
| `kv.Open(ctx, store, opts)` then `OpenBucket(state, …)` | `store.bucket(…)`; the engine opens with the first bucket |
| `Of(owners...)`                                         | `under`                                                   |
| `Sliding(d)`                                            | `idle`                                                    |
| `DefaultTTL(d)` on a bucket                             | `ttl` on the bucket                                       |
| `SetIfAbsent`, `SetEntryIfAbsent`                       | `create`, and `add` for sets                              |
| `Touch`                                                 | `expire`                                                  |
| `GetEntry`, `SetEntry`                                  | `entry`; `set` returns nothing                            |
| `Scan`                                                  | `list`                                                    |
| `LoseAtMost(d)`                                         | `flushEvery: '1s'`                                        |
| `OpenLimiter`, `Rate(n, per)`, `Burst`, `RetryAfter`    | `rateLimit`, `rate: '100/s'`, `burst`, `retryAt`          |
| `OpenQuota`, `Window(name, n, span)`, `Get`, `Delete`   | `quota`, `{ name: 'n/span' }`, `peek`, `reset`            |
| `OpenOnce`                                              | `once`                                                    |
| `OpenConfig`                                            | `store.config`, in the core                               |
| `Options.In`                                            | `db.bucket(…)`                                            |
| `Tx`, `WithTx(tx)`, `View`                              | `store.tx`, `tx.with(handle)`; no view                    |
| `Written`, `Deleted`, `Cleared`                         | `tx.with(bucket).set`, `.delete`, `.clear` in `db.tx`     |
