# kv: the API book

Draft, 9 October 2026, after one newcomer check (below). The application's
current state: sessions, one-time codes, flags, the answers of requests sent
twice, counters and limits. This book is the API before the code;
[dx.md](../dx.md) has the rules it follows. TypeScript comes first in each
section because most callers will read it; Python, Go and Rust follow when
they spell something differently. The Rust column is built for buckets,
branches, expiry and versions (`crates/tinystore/src/kv`); counters, limits,
`once` and transactions are not yet.

## What a newcomer learns

| Concept                 | In one line                                                                 |
|-------------------------|-----------------------------------------------------------------------------|
| bucket                  | a named collection of values of one type, by key                            |
| key                     | text: a string, or a number spelled in decimal                              |
| `ttl` and `idle`        | a key expires a span after it was written, or a span after it was last read |
| version                 | what a key was when you read it, to write only if nobody changed it since   |
| `of`                    | a branch of a bucket, such as one user's sessions, cleared in one call      |
| counters                | numbers by key that only add up                                             |
| `rateLimit` and `quota` | may this request pass: a smooth rate, or uses in windows that reset         |
| `once`                  | run a function once a key and keep its answer, for requests sent twice      |

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
sessions, err := kv.OpenBucket[Session](store, "sessions", kv.Idle(30*24*time.Hour))
codes, err := kv.OpenBucket[int64](store, "login-codes", kv.TTL(15*time.Minute))
seen, err := kv.OpenBucket[struct{}](store, "stripe-events", kv.TTL(7*24*time.Hour))
```

```rust
let sessions = store.bucket::<Session>("sessions").idle(days(30)).open()?;
let codes = store.bucket::<i64>("login-codes").ttl(minutes(15)).open()?;
let seen = store.bucket::<()>("stripe-events").ttl(days(7)).open()?;
```

- A bucket's name is `[a-z0-9][a-z0-9_-]{0,63}` and keeps its role: a name
  that holds counters does not open as a bucket of values.
- `ttl` and `idle` are the default of every key written without its own; they
  do not go together.
- A value is kept by its type: text and bytes as they are, integers and
  booleans as integers, floats bit for bit (`-0`, NaN payloads and infinities
  included), nothing for a set, JSON for the rest. TypeScript loses the type at
  run time, so `type` names it for the few that are not JSON: `'string'`,
  `'bytes'`, `'int'`, `'float'`, `'bool'`. In Rust the type parameter is
  enough: anything serde reads and writes.

## Read and write

```ts
await sessions.of(user.id).set(token, { user: user.id, device })
const session = await sessions.of(user.id).get(token)   // Session | undefined
const userId = await codes.take(code)                  // read and remove in one step
const isNew = await seen.add(event.id)                 // false when it was there
await sessions.of(user.id).expire(token, '1h')         // expires an hour from now
```

- `set` writes; a key that exists keeps its expiry unless the call gives one:
  `set(key, value, { ttl: '1h' })`.
- `get` is `undefined` (`None`, `nil`) for a key that is not there or expired.
- `add` puts a key in a set; `create(key, value)` writes a key only when it is
  not there. Both return `false` when it is there, and never throw for that.
- `take` reads a key and removes it in one commit: of two callers taking a
  one-time code at once, one gets it.
- `expire(key, span)` gives a key a new expiry, as Redis's `EXPIRE` does: it
  may lengthen or shorten its life, and keeps its value.
- `delete` removes a key and says whether it was there.
- A write returns once it is durable; writes from many callers share a commit.

## Versions

```ts
const entry = await settings.entry('home')         // { key, value, version, expiresAt } | undefined
await settings.set('home', next, { ifVersion: entry.version })   // ConflictError if it changed
```

- A version is the revision of kv.db that wrote the value; it never repeats,
  across deletes, expiry and restarts. It compares only for equality.
- `ifVersion` works on `set`, `take`, `delete` and `expire`. A key that changed,
  expired or was deleted since is a conflict.
- To write only a key that is not there, use `create`.

## Expiry

| Option            | What happens                                              |
|-------------------|-----------------------------------------------------------|
| `ttl: '15m'`      | the key expires 15 minutes after it was written           |
| `expiresAt: date` | the key expires at that time                              |
| `idle: '30d'`     | the key expires 30 days after it was last read or written |
| none              | the bucket's own, or never                                |

An expired key is gone for every call at once: reads miss it, `create` and
`add` take it, `ifVersion` conflicts with it. The store deletes its row later.
A read of an `idle` key renews it once a thirtieth of the span has passed.

## Branches

```ts
await sessions.of(user.id).set(token, session)
await sessions.of(user.id).clear()                     // signed out everywhere
for await (const { key, value } of sessions.of(user.id).all()) { … }
const page = await sessions.of(user.id).list({ limit: 100, after: page.next })   // { entries, next }
```

- `of(...owners)` names a branch: whose keys these are. A branch exists while
  it holds keys.
- `clear()` removes a branch and every branch under it, at once for every
  reader, however many keys it holds.
- `all()` walks every key a page at a time, holding nothing between pages;
  `list()` reads one page, at most 1000 keys, in the byte order of the keys,
  and `next` is where the following page starts.

## Counters

```ts
const attempts = store.counters('login-attempts', { ttl: '15m' })
const n = await attempts.add(clientIp, 1)              // the new count: 1, 2, 3…
```

- An absent or expired counter is 0. A counter's window starts with its first
  add and does not slide: 15 minutes after the first attempt it starts from 0.
- `durability: '1s'` keeps counters in memory and writes them every second: a
  crash loses at most that second. Without it every add is a durable write.

## Limits

```ts
const api = store.rateLimit('api', { rate: '100/s', burst: 20 })
const { ok, left, retryAt } = await api.allow(userId)   // retryAt: a Date

const ai = store.quota('ai', { hourly: '100/5h', weekly: '300/7d' })
const usage = await ai.allow(userId)                    // counts in every window, or in none
await ai.refund(userId)
```

- `rateLimit` lets `rate` requests a key through on average and `burst` at
  once, with no window edge that lets twice the rate through.
- `quota` counts uses in each named window; a window starts at a key's first
  use and resets on its own. `allow` counts in all of them or in none, `check`
  counts nothing, `refund` gives a use back.
- Both answer rather than throw: `ok`, `left`, and `retryAt`, the time a refused
  request may try again.

## Once

```ts
const charges = store.once<Receipt>('charges', { keep: '1d' })
const receipt = await charges.run(requestId, () => pay.charge(order, requestId))
```

- `run` returns the answer kept for the key, or runs the function and keeps
  what it returns for `keep`. After that the key runs again: pass the same id
  to the payment provider too.
- A run of the same key elsewhere, in this process or another, waits for it and
  returns its answer.
- An error keeps nothing: the next `run` runs again.

## Transactions

```ts
await store.tx(async tx => {
	const left = (await stock.in(tx).get(sku)) ?? 0
	if (left < 1) throw new SoldOut()
	await stock.in(tx).set(sku, left - 1)
	await orders.in(tx).set(id, order)
})
```

- A transaction holds kv.db's writer alone while it reads and decides, so two
  orders cannot both take the last item; it commits when the function returns
  and rolls back when it throws.
- Writes that need no reads need no transaction: separate calls already share
  commits.

## Errors

| Error             | When                                                      | What to do                        |
|-------------------|-----------------------------------------------------------|-----------------------------------|
| `invalid`         | a bad name, key or option; a value the bucket cannot keep | fix the call                      |
| `conflict`        | `ifVersion` did not match                                 | read again and decide             |
| `limit`           | a value over 1 MiB, the store's memory                    | write less                        |
| `corrupt`         | a stored value does not read as the bucket's type         | open the bucket with its type     |
| `closed`          | the store closed                                          | open it again                     |
| `unavailable`     | another process holds the writer past the busy timeout    | try again                         |
| `outcome unknown` | the commit failed after the write ran                     | read the key before writing again |

Every error names the bucket and the key, owners first:
`kv bucket sessions: key "42/token": conflict`.

## Bounds

| What                  | Bound                                         |
|-----------------------|-----------------------------------------------|
| a key with its owners | 1 KiB                                         |
| a value               | 1 MiB; over 512 bytes it has a row of its own |
| a page                | 1000 keys                                     |
| a bucket's name       | 64 bytes                                      |

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
what the agent asked about. The next check runs on this version.

## Was, in Go

| Was                                                     | Now                                                       |
|---------------------------------------------------------|-----------------------------------------------------------|
| `kv.Open(ctx, store, opts)` then `OpenBucket(state, …)` | `store.bucket(…)`; the engine opens with the first bucket |
| `Sliding(d)`                                            | `idle`                                                    |
| `DefaultTTL(d)` on a bucket                             | `ttl` on the bucket                                       |
| `SetIfAbsent`, `SetEntryIfAbsent`                       | `create`, and `add` for sets                              |
| `Touch`                                                 | `expire`                                                  |
| `GetEntry`, `SetEntry`                                  | `entry`; `set` returns nothing                            |
| `Scan`                                                  | `list`                                                    |
| `LoseAtMost(d)`                                         | `durability: '1s'`                                        |
| `OpenLimiter`, `Rate(n, per)`, `Burst`, `RetryAfter`    | `rateLimit`, `rate: '100/s'`, `burst`, `retryAt`          |
| `OpenQuota`, `Window(name, n, span)`                    | `quota`, `{ name: 'n/span' }`                             |
| `OpenOnce`                                              | `once`                                                    |
| `OpenConfig`                                            | `store.config`, in the core                               |
| `Options.In`                                            | `database: db`                                            |
| `Written`, `Deleted`, `Cleared`                         | `bucket.in(tx).set`, `.delete`, `.clear`                  |
