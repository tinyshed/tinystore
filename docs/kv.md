# KV: the application's current state

The design of the kv engine: agreed, not built. There is no `kv/` package yet.
The API and the contracts below are settled; what lies under
[Storage](#storage) is provisional until the round under [Open](#open) has
measured it.

## What it is for

The small state every service keeps beside its data: sessions, one-time
codes, attempt limits, webhook claims, drafts, feature flags, settings, a cache
of another service's answers. Today each of them is a table with its own
upsert, its own `expires_at` and its own sweeper, or a Redis for a handful of
keys.

- **One process's state.** One store owns the directory, so kv holds what one
  process decides. State that several instances share belongs in a server.
- **Not bytes, queries or work.** Large bytes are the blobs engine's, queries
  `sqldb`'s, durable retried work the jobs engine's, and no write spans two
  engines.

## The public surface

```go
state, err := kv.Open(ctx, store, kv.Options{}) // data/kv.db

sessions, err := kv.OpenBucket[Session](ctx, state, "sessions", kv.Sliding(30*24*time.Hour))
attempts, err := kv.OpenCounters(ctx, state, "login-attempts", kv.DefaultTTL(15*time.Minute))

s, found, err := sessions.Of(userID).Get(ctx, token)
err = sessions.Of(userID).Set(ctx, token, s, kv.TTL(time.Hour))
err = sessions.Of(userID).Delete(ctx, token)
n, err := attempts.Add(ctx, ip, 1)
```

- **A type is declared once.** `OpenBucket[V]` fixes the value's type, and the
  calls after it read like a Go map. A bucket's name and kind are kept in
  `kv.db` and checked when it opens: a bucket of counters opened as a bucket of
  values is `ErrInvalid`. Its options are the program's and may change between
  runs.
- **The engine opens against the store.** `kv.Open` claims `kv.db` as every
  engine claims its file, and the store closes it with the others.
- **A plain verb returns the least; its `Entry` twin returns the version.**
  `Get` and `GetEntry`, `Set` and `SetEntry`, `SetIfAbsent` and
  `SetEntryIfAbsent`.

```go
// Bucket[V]
Get(ctx, key) (V, bool, error)
GetEntry(ctx, key) (kv.Entry[V], bool, error)                    // Key, Value, Version, ExpiresAt
Has(ctx, key) (bool, error)
Set(ctx, key, value, opts...) error                               // kv.TTL, kv.ExpireAt, kv.IfVersion
SetEntry(ctx, key, value, opts...) (kv.Entry[V], error)
SetIfAbsent(ctx, key, value, opts...) (created bool, err error)
SetEntryIfAbsent(ctx, key, value, opts...) (kv.Entry[V], bool, error) // what it wrote, or what was there
Take(ctx, key, opts...) (V, bool, error)                          // read and delete in one step
Delete(ctx, key, opts...) error                                   // an absent key is not an error
Touch(ctx, key, opts...) (bool, error)                            // a new expiry; value and version kept
Scan(ctx, kv.Query) (kv.Page[V], error)                           // this branch's own keys
Clear(ctx) error                                                  // this branch and every branch under it
Of(owners ...any) *kv.Bucket[V]
WithTx(tx *kv.Tx) *kv.Bucket[V]

// Counters
Add(ctx, key, n) (int64, error)
Max(ctx, key, n) (int64, error)
Get(ctx, key) (int64, error)                                      // an absent key is 0
Delete(ctx, key) error
Clear(ctx) error
Of(owners ...any) *kv.Counters
WithTx(tx *kv.Tx) *kv.Counters
```

## Five cases

They are the API's examples, the gates' workloads and the round's.

**Sessions.** The cookie carries the user's id and a token.

```go
sessions, err := kv.OpenBucket[Session](ctx, state, "sessions", kv.Sliding(30*24*time.Hour))

token := rand.Text()
err = sessions.Of(user.ID).Set(ctx, token, Session{Device: r.UserAgent(), Since: time.Now()})

s, found, err := sessions.Of(c.UserID).Get(ctx, c.Token)           // an expired session is not found
err = sessions.Of(c.UserID).Delete(ctx, c.Token)                  // sign out here
err = sessions.Of(c.UserID).Clear(ctx)                            // and everywhere
page, err := sessions.Of(c.UserID).Scan(ctx, kv.Query{Limit: 20}) // "signed in on 3 devices"
```

**A sign-in link.** The code lives fifteen minutes and works once.

```go
codes, err := kv.OpenBucket[int64](ctx, state, "login-codes", kv.DefaultTTL(15*time.Minute))

code := rand.Text()
err = codes.Set(ctx, digest(code), user.ID) // a digest, so a copy of the file holds no working link

userID, found, err := codes.Take(ctx, digest(r.FormValue("code"))) // a second click finds nothing
```

**Attempt limits.** A window of fifteen minutes from the first attempt; a crash
may forget the last second of attempts, which a limit can afford.

```go
attempts, err := kv.OpenCounters(ctx, state, "login-attempts",
	kv.DefaultTTL(15*time.Minute), kv.LoseAtMost(time.Second))

byIP, err := attempts.Of("ip").Add(ctx, clientIP(r), 1)
byEmail, err := attempts.Of("email").Add(ctx, form.Email, 1)
refused := byIP > 20 || byEmail > 5
```

**Webhook claims.** A provider delivers an event until it gets a 200, so one
event can arrive twice. The claim's version keeps a handler whose claim expired
from finishing or deleting the next one's.

```go
seen, err := kv.OpenBucket[struct{}](ctx, state, "stripe-events")

claim, first, err := seen.SetEntryIfAbsent(ctx, event.ID, struct{}{}, kv.TTL(10*time.Minute))
if err != nil || !first {
	return err // nil: handled before, or being handled now
}
if err := handle(ctx, event); err != nil {
	return errors.Join(err, seen.Delete(ctx, event.ID, kv.IfVersion(claim.Version)))
}
return seen.Set(ctx, event.ID, struct{}{}, kv.IfVersion(claim.Version), kv.TTL(7*24*time.Hour))
```

A handler slower than its claim, or a crash after its side effect, still
handles an event twice: that is the limit of two systems without one
transaction, and kv adds no race of its own.

**Draft autosave from two tabs.** A save goes through only if nobody saved since
the tab read the draft.

```go
drafts, err := kv.OpenBucket[Draft](ctx, state, "drafts")

entry, found, err := drafts.Of(userID).GetEntry(ctx, noteID)                            // the version the tab read
entry, err = drafts.Of(userID).SetEntry(ctx, noteID, draft, kv.IfVersion(form.Version)) // ErrConflict: another tab saved
```

## Keys

- **A key is text.** A string, a `[]byte` or an integer; an integer is its
  decimal spelling, so `42` and `"42"` are one key, and a handler writing an
  `int64` from the database meets one reading `"42"` from a URL. Anything
  else, an empty key, or a path longer than 1 KiB is `ErrInvalid`; a longer
  identifier is hashed by the caller.
- **`Of` names a branch.** Its owners are keys by the same rule. A branch exists
  while it holds keys: there is nothing to create or drop. The type is the
  root, the owner a branch, the id the key, so Redis's `user:42:devices:iPhone`
  is `devices.Of(42).Set(ctx, "iPhone", d)`.
- **A branch is one range.** An owner is written after a mark and before a
  terminator, a key after a mark of its own, so a name may hold any byte:

```text
devices.Of("tenant-7", 42).Set(ctx, "iPhone", d)
→ 01 "tenant-7" 00 · 01 "42" 00 · 02 "iPhone"         a 00 inside a name is written 00 FF
```

  `Scan` reads the branch's own keys, the range after `prefix · 02`; `Clear`
  removes everything under the prefix. Keys come back in the byte order of
  their text: `"10"` before `"9"`.

## Values

- **By their type.** `[]byte` and `string` are kept as they are; `bool`,
  integers and floats as fixed-width binary, floats bit for bit; `struct{}` as
  nothing, so a bucket of `struct{}` is a set whose rows hold no value bytes;
  anything else as JSON through `encoding/json`, which goes on reading a struct
  that gained a field.
- **Refused, not changed.** A value `encoding/json` cannot write, a NaN or a
  channel, is `ErrInvalid` at `Set`. What it changes on the way back, an
  `int64` inside an `any` read as a `float64`, is the caller's to avoid, or
  `kv.WithCodec`'s.
- **At most 1 MiB.** Larger bytes are the blobs engine's, with their key kept
  here.

## Time

- **The store's clock, read once a call.** An expiry is a schedule, not an
  observation, so kv has no `ErrTooOld` window; `Options.Clock` moves a test a
  month ahead without sleeping.
- **Expired is absent to every operation.** `Get`, `Has` and `Scan` do not see
  it, `SetIfAbsent` claims it, `IfVersion` conflicts with it, `Take` finds
  nothing, and `Add` starts again from zero with a new expiry.
- **A key's expiry follows four rules**, the same in every bucket:

```text
DefaultTTL(15m)    a key created without kv.TTL gets it; a later Set or Add keeps the expiry the key has
kv.TTL, ExpireAt   replace the expiry
Sliding(30d)       a read renews it to now + 30d, at most once per RefreshEvery, a thirtieth of the term
Touch              replaces the expiry and nothing else
```

  An attempt counter keeps its fixed fifteen minutes: its `Add`s do not slide
  it.
- **A read does not write.** A `Sliding` renewal is written with the next
  flush, and a crash forgets the renewals since the last one; otherwise an
  entry read at `t` lives at least until `t + term − RefreshEvery`.
- **An expiry is not a version.** `Touch` and `Sliding` keep the version, so a
  renewal never fails a writer's `IfVersion`.

## Versions

- **Every write takes the file's next revision**, and an entry's version is the
  revision that wrote its value. It never repeats: not after `Delete` and a new
  `Set`, not after expiry, not after reopen, since the high-water mark is in
  the file. An old `IfVersion` cannot pass against a key deleted and written
  again.
- **Opaque.** Two versions are equal or not. A version marshals to text, so it
  goes to a page and comes back with the next save.
- **`kv.IfVersion(v)`** lets `Set`, `SetEntry`, `Take` and `Delete` apply only
  to a live key with version `v`; anything else is `ErrConflict`.

## Counters

- **An `int64` a key.** `Add` returns the new value, `Max` keeps the larger,
  and `Get` of an absent key is `0`.
- **An overflow is refused.** An `Add` past the `int64` range is `ErrLimit`,
  since SQLite would quietly turn an overflowing integer sum into a `REAL`.
- **`kv.LoseAtMost(d)`** changes memory and returns. A flush every `d`, and
  `Close`, writes the deltas in one transaction; a crash loses at most `d` of
  them. A `Get` sees the deltas not yet written. Past a bound of keys waiting,
  the `Add` that crosses it flushes before it returns, so memory stays bounded
  however many distinct keys arrive.

## Transactions and snapshots

```go
err = state.Tx(ctx, func(tx *kv.Tx) error {
	userID, found, err := codes.WithTx(tx).Take(ctx, digest(code))
	if err != nil {
		return err
	}
	if !found {
		return errInvalidCode
	}
	return sessions.WithTx(tx).Of(userID).Set(ctx, token, Session{Device: device})
})

err = state.View(ctx, func(tx *kv.Tx) error { … }) // reads from one snapshot
```

- `Tx` is one writer transaction over any buckets of `kv.db`: nil commits, an
  error or a panic rolls back. It never spans two engines.
- A handle from `WithTx` works inside its callback only and is `ErrClosed`
  after it. Inside `Tx` a `LoseAtMost` counter is as durable as the rest.
- `View` holds its snapshot for at most five seconds, as a records read does,
  and a write inside it is `ErrInvalid`.

## Under a flood

**A refused request writes nothing durable.** A flood takes two paths, a
session read and a counter's `Add`, and neither waits for a disk: a read is one
point statement, a `LoseAtMost` counter lives in memory between flushes, and a
`Sliding` renewal waits for the flush. What writes durably, a sign-in, a code,
a claim, a draft, comes after a limit has let the request in. The round
measures each path's ceiling against the peak a production service sees.

## Errors

| Sentinel | When |
|---|---|
| `ErrInvalid` | a key that is not text or an integer, an empty key, a path over 1 KiB, a value JSON cannot write, a bucket opened as another kind, a write inside `View` |
| `ErrLimit` | a value over 1 MiB, a counter past the `int64` range |
| `ErrConflict` | `IfVersion` against another version, or an expired or absent key |
| `ErrClosed` | the store closed, or a `WithTx` handle used after its callback |
| `ErrCorrupt` | a row that no longer decodes |

An error about one key is a `*kv.KeyError` naming its bucket and path;
`errors.Is` finds the store's sentinel in it.

## Storage

Provisional: the round settles the page size, the inline threshold and whether
writes need group commit.

```text
buckets   id | name | kind                                     values or counters
cells     bucket | path | version | expires | value | spill     without rowid, key (bucket, path)
          index (expires, bucket, path) where expires is not null
spilled   id | value                                            values past the inline threshold
meta      name | value                                          the revision's high-water mark
```

- **One table for every kind.** `value` has no declared type, so a row holds a
  blob, an integer or nothing, and a member of a set costs its path, its
  version and one header byte.
- **An operation is one statement.** SQLite's upsert, `returning` and
  `delete … returning` are the mutation language; no operation reads into Go
  and writes back.

```sql
-- Take: read and burn
delete from cells where bucket = ?1 and path = ?2 and (expires is null or expires > ?3)
returning version, value, spill;
```

- **A read is one statement** on a reader, and a `Scan` page comes from one
  snapshot.
- **Maintenance runs through `Store.Every`**: expired rows in batches through
  the expiry index, the `LoseAtMost` and `Sliding` flushes, spilled rows no
  cell names any more.
- **`Clear` deletes the branch's range in one transaction.** If the round finds
  that a large branch holds the writer too long, a branch gets a generation,
  hidden at once and swept by maintenance; the contract does not change.
- **A durable write is one transaction.** Group commit, under
  [its contract](group-commit-contract.md), comes if the round shows that
  independent writers need it.
- **Backup copies `kv.db`** like any engine's file; the deltas a `LoseAtMost`
  counter holds in memory are not in the copy.

## Bounds

| Object | Limit |
|---|---:|
| A key's path, owners and key | 1 KiB |
| A value | 1 MiB |
| A `Scan` page | 1000 keys, 4 MiB of values |
| A `View` snapshot | 5 s |
| `Sliding` renewals | one a key per thirtieth of the term |
| Keys waiting in a `LoseAtMost` bucket | set by the round |

## Gates

The five cases are the gates' workloads.

| Promise | What enforces it |
|---|---|
| a `Set` that returned survives an abrupt exit | `TestAWriteThatReturnedSurvivesAnAbruptExit` |
| an expired key is absent to every operation | `TestAnExpiredKeyIsAbsentToEveryOperation` |
| a default TTL is given once, at creation | `TestADefaultTTLIsGivenOnceAtCreation` |
| a sliding read writes at most once per refresh | `TestASlidingReadWritesAtMostOncePerRefresh` |
| an integer key is its decimal text | `TestAnIntegerKeyIsItsDecimalText` |
| a version never repeats | `TestAVersionNeverRepeatsAfterDeleteExpiryOrReopen` |
| one of concurrent `Take`s gets the value | `TestConcurrentTakesGiveTheValueOnce` |
| a stale claim cannot finish or delete the next | `TestAStaleClaimCannotFinishOrDeleteTheNext` |
| one of two versioned writes conflicts | `TestOneOfTwoVersionedWritesConflicts` |
| an overflowing counter is refused, not rounded | `TestAnOverflowingCounterIsRefusedRatherThanRounded` |
| `LoseAtMost` loses no more than it says | `TestLoseAtMostLosesNoMoreThanItsInterval` |
| a bucket keeps its kind under its data | `TestABucketCannotChangeItsKindUnderItsData` |
| `Clear` empties a branch and those under it at once | `TestClearEmptiesTheBranchAndThoseUnderIt` |

## Not in the first version

Each waits for a workload that needs it and a measurement that pays for it.

- `KeepInMemory(max)`: a bucket held whole in memory, a `Set` past `max`
  refused, nothing evicted. A cache budget for the whole file comes first.
- A filter that answers a miss without SQLite.
- `History`, with `At(t)` and `ErrTooOld` before its window, and `Watch`.
- A GCRA limiter, one `int64` a key, and `AddWithin` for quotas.
- Listing a branch's branches; secondary indexes.

## Open

The round before the code:

| Question | What it decides |
|---|---|
| durable `Set`s a second through `internal/sqlite` from 1, 8, 64 and 512 goroutines, a transaction each and grouped | whether group commit is in the first version |
| a point `Get` through `View`, which begins a transaction, against one statement without one: 1 and 10 million keys, hits and misses, 1 to 512 readers | whether `internal/sqlite` needs a point-read path, and the read ceiling |
| a `LoseAtMost` flush of 1,000 to 100,000 distinct keys | how many distinct keys a second a flood may bring, and the bound on keys waiting |
| `dbstat` by object for `cells` and its expiry index at 1 and 4 KiB pages, values of 16 to 4096 bytes | the page size and the inline threshold |
| `Clear` of 1,000 to 100,000 keys | whether a branch needs generations |
| the requests a second a production service peaks at, from the log corpus, as aggregates | how far those ceilings are from what production sees |
