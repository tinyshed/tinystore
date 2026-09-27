# KV: the application's current state

The design of the kv engine, built: `kv/` holds everything below but what
[Not in the first version](#not-in-the-first-version) leaves, and its contract
is [kv/README.md](../kv/README.md). What lies under [Storage](#storage) was
measured by [the mechanics round](reports/kv-mechanics-2026-09-26.md) on one
development machine and on a production host.

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
- **An option of one kind does not compile for the other.** `Sliding` and
  `WithCodec` are a bucket's, `LoseAtMost` is counters', `DefaultTTL` fits
  both, so `OpenBucket(…, kv.LoseAtMost(time.Second))` is a compile error
  rather than an `ErrInvalid` at start.

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
All(ctx) iter.Seq2[kv.Entry[V], error]                            // the same, a page at a time, no snapshot held
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
Sliding(30d)       a key created gets 30d, and a Get, GetEntry or Has renews it to now + 30d
                   once a thirtieth of the term, a day, has passed since it last did; Scan does not
Touch              replaces the expiry and nothing else
```

  An attempt counter keeps its fixed fifteen minutes: its `Add`s do not slide
  it. `Sliding` and `DefaultTTL` on one bucket are `ErrInvalid`.
- **A read does not write.** A `Sliding` renewal waits for the next flush, a
  second at most, and a crash forgets the renewals since the last one;
  otherwise an entry read at `t` lives at least until `t + term − term/30`. A
  key read in its last minute is renewed before the read returns, since the
  flush might come after it expired.
- **A renewal is bound to the row its read saw**, its version and its expiry,
  and changes nothing else. A key written again since, even with the very
  expiry the read saw, touched, deleted or cleared keeps what was done to it;
  its next read asks again.

```sql
update cells set expires = :until where bucket = ?1 and path = ?2 and version = :seen and expires = :seen_expiry
```

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
- **`kv.IfVersion(v)`** lets `Set`, `SetEntry`, `Take`, `Delete` and `Touch`
  apply only to a live key with version `v`; anything else is `ErrConflict`.

## Counters

- **An `int64` a key.** `Add` returns the new value, `Max` keeps the larger,
  and `Get` of an absent key is `0`.
- **An overflow is refused.** An `Add` past the `int64` range is `ErrLimit`,
  since SQLite would quietly turn an overflowing integer sum into a `REAL`.
- **`kv.LoseAtMost(d)`** keeps the counters in memory, and memory is the
  truth for the keys it holds: the file is behind it by `d` at most. The first
  change of a key since the last flush reads its row once; after it `Add`,
  `Max` and `Delete` change memory and return. A flush every `d`, on `Close`
  and on `Maintain` writes what changed, 10,000 keys a transaction, and lets go
  of what did not change again. A crash loses at most the last `d` of every
  mutation, a `Delete`'s as an `Add`'s: a deleted counter can come back. Past
  100,000 keys waiting, the change that reaches the bound flushes first, so
  memory stays bounded however many distinct keys arrive.
- **One name, one way of keeping it.** Handles on counters of one name opened
  with the same `LoseAtMost` share one memory; opening them again with another
  interval, or without `LoseAtMost`, is `ErrInvalid`, since one handle would
  read the file while another holds newer numbers.

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
  after it. A `LoseAtMost` counter joins no transaction and is `ErrInvalid`
  inside one: a transaction promises its writes are in the file when it
  commits, and memory cannot keep that promise without holding the key until
  the transaction ends. Counters a transaction needs are opened without it.
- `View` holds its snapshot for at most five seconds, as a records read does,
  and a write inside it is `ErrInvalid`.

## Under a flood

**A refused request writes nothing durable.** A flood takes two paths, a
session read and a counter's `Add`, and neither waits for a disk: a read is one
point statement, a `LoseAtMost` counter lives in memory between flushes, and a
`Sliding` renewal waits for the flush. What writes durably, a sign-in, a code,
a claim, a draft, comes after a limit has let the request in. On the
development machine a point read held 97,000 to 244,000 a second over eight
readers in the container, an `Add` millions, and grouped durable writes
53,000, against 106 requests in the busiest second of the production services
that log theirs. On that production host, two vCPUs, the reads held 100,000 a
second and grouped writes 55,000.

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

```text
buckets    id | name | kind                                     values or counters
cells      bucket | path | version | expires | value | spill     without rowid, key (bucket, path)
           index (expires, bucket, path) where expires is not null
spilled    id | value                                            values over 512 bytes
branches   bucket | prefix | cleared                             a Clear past 10,000 keys
meta       name | value                                          the revision's high-water mark
```

- **4 KiB pages.** At 1 KiB a row with a 256-byte value no longer fits what a
  page keeps of a row, and SQLite leaves most of an overflow page empty: 1,158
  bytes a row and a lookup three times slower.
- **A value over 512 bytes spills** to `spilled`, and its cell keeps the id. Up
  to 512 bytes a row costs what spilling does, 745 against 698 bytes, and a
  `Get` reads one row; a 1024-byte value in the row costs 4,740 bytes against
  1,480.
- **One table for every kind.** `value` has no declared type, so a row holds a
  blob, an integer or nothing, and a member of a set costs its path, its
  version and one header byte.
- **An operation runs whole in the writer.** Its statements read and write
  the key's row inside one savepoint of the writer's transaction, so no other
  write comes between them; a `Take` is one `delete … returning`.

```sql
-- Take: read and burn
delete from cells where bucket = ?1 and path = ?2 and (expires is null or expires > ?3)
returning version, value, spill;
```

- **A point read is one statement without a transaction.** A statement is its
  own snapshot, and without `View`'s transaction around it eight readers
  served 8 to 31 % more Gets a second. `internal/sqlite` gains that path; a
  read of several statements, a `Scan` page or a `View`, keeps its snapshot.
- **Durable writes commit in groups.** A caller that finds no leader commits
  every write queued behind it, each in a savepoint, and hands the lead to the
  first caller still waiting; no goroutine is started, and a caller alone pays
  one commit as before. A transaction each held 340 `Set`s a second in the
  container at any concurrency, 512 callers waiting 1.5 s at the median;
  grouped, 53,000 a second at 9 ms. The failure paths are
  [the contract](group-commit-contract.md)'s.
- **A `LoseAtMost` flush writes at most 10,000 keys a transaction**, 45 to 92
  ms of the writer, so that a flood of distinct keys does not hold a sign-in
  behind half a second of flush; 100,000 keys may wait.
- **`Clear` deletes up to 10,000 keys in its own transaction**, 35 to 45 ms. A
  larger branch is marked in `branches` with the file's revision: under its
  prefix a row of that version or older is gone. Every statement that finds
  live rows skips such a row, so the keys are hidden to readers and writers
  alike the moment the mark commits, and maintenance deletes them 10,000 a
  transaction, then the mark. A key written after the Clear has a later
  version, so it is a new key and stays; paths never change. A million keys in
  one transaction held the writer for 5.3 to 8.7 s.

```text
Clear(42) at revision R
  under 01 42 00, version ≤ R   → hidden now, deleted by maintenance
  under 01 42 00, version > R   → written after the Clear, kept
  01 42 00 FF …, a name "42\x00" → not under the branch: after the prefix comes FF, not a mark
```

- **A `Clear` of `LoseAtMost` counters** holds their flushes back and their
  changes out while it runs, and every read of their memory from its commit
  until memory has let go of the branch, so that no flush writes a cleared
  counter again and no read finds one after the commit. One that rolls back
  leaves memory as it was; one whose commit fails may be in the file, so
  memory lets go of the branch as a crash would, within `LoseAtMost`'s loss,
  and the Clear is `ErrOutcomeUnknown`.
- **Maintenance runs through `Store.Every`**: the `LoseAtMost` and `Sliding`
  flushes, the rows a mark hid, expired rows in batches through the expiry
  index, spilled rows no cell names any more.
- **Backup copies `kv.db`** like any engine's file; the deltas a `LoseAtMost`
  counter holds in memory are not in the copy.

## Bounds

| Object | Limit |
|---|---:|
| A key's path, owners and key | 1 KiB |
| A value | 1 MiB |
| A `Scan` page | 1000 keys, 4 MiB of values |
| A `View` snapshot | 5 s |
| `Sliding` renewals | one a key per thirtieth of the term; 100,000 waiting, past them the next read asks again |
| A value kept in its row | 512 bytes; a larger one spills |
| Keys waiting in a `LoseAtMost` bucket | 100,000 |
| A `LoseAtMost` flush | 10,000 keys a transaction |
| A `Clear` in its own transaction | 10,000 keys; a larger one is marked, and is `ErrLimit` inside `Tx` |
| Durable writes committed together | 1024 |

## Gates

The five cases are the gates' workloads.

| Promise | What enforces it |
|---|---|
| a `Set` that returned survives an abrupt exit | `TestAWriteThatReturnedSurvivesAnAbruptExit` |
| an expired key is absent to every operation | `TestAnExpiredKeyIsAbsentToEveryOperation` |
| a default TTL is given once, at creation | `TestADefaultTTLIsGivenOnceAtCreation` |
| a sliding read writes at most once per refresh | `TestASlidingReadWritesAtMostOncePerRefresh` |
| a renewal never extends a newer incarnation of its key | `TestARenewalDoesNotExtendANewerIncarnation` |
| a key read in its last minute is renewed at once | `TestAReadNearItsExpiryRenewsAtOnce` |
| `All` walks pages and holds no snapshot between them | `TestAllWalksEveryKeyAPageAtATime` |
| an integer key is its decimal text | `TestAnIntegerKeyIsItsDecimalText` |
| a version never repeats | `TestAVersionNeverRepeatsAfterDeleteExpiryOrReopen` |
| one of concurrent `Take`s gets the value | `TestConcurrentTakesGiveTheValueOnce` |
| a stale claim cannot finish or delete the next | `TestAStaleClaimCannotFinishOrDeleteTheNext` |
| one of two versioned writes conflicts | `TestOneOfTwoVersionedWritesConflicts` |
| an overflowing counter is refused, not rounded | `TestAnOverflowingCounterIsRefusedRatherThanRounded` |
| `LoseAtMost` loses no more than it says | `TestLoseAtMostLosesNoMoreThanItsInterval` |
| counters of one name keep their numbers one way | `TestCountersOpenAgainOnlyAsTheyWereOpened` |
| a `LoseAtMost` counter joins no transaction | `TestALoseAtMostCounterRefusesATransaction` |
| the counters waiting for a flush stay within their bound | `TestWaitingCountersStayWithinTheirBound` |
| a bucket keeps its kind under its data | `TestABucketCannotChangeItsKindUnderItsData` |
| `Clear` empties a branch and those under it at once, over the bound and under it | `TestClearEmptiesTheBranchAndThoseUnderIt` |
| a cleared key is absent to every operation | `TestAClearedKeyIsAbsentToEveryOperation` |
| a `Clear` never brings back counters waiting for a flush | `TestAClearDoesNotResurrectCountersWaitingForTheFlush` |
| a failed `Clear` keeps what counters wait to flush | `TestAFailedClearKeepsTheCountersWaitingForTheFlush` |
| a `Clear` whose commit fails lets memory go as a crash would | `TestAClearWhoseCommitFailsLetsGoAsACrashWould` |
| Clears beside changes and flushes keep their branches apart | `TestClearsBesideChangesAndFlushesKeepTheirBranchesApart` |
| a `Clear` inside `Tx` deletes what it clears or refuses | `TestAClearInATransactionOverTheBoundIsRefused` |
| a refused write fails alone in its group | `TestGroupedWritesShareACommitAndFailAlone` |
| a caller cancelled before its turn writes nothing | `TestACallerCancelledBeforeItsTurnWritesNothing` |
| a value over 512 bytes reads back from `spilled` | `TestALargeValueSpillsAndReadsBack` |

## Not in the first version

Each waits for a workload that needs it and a measurement that pays for it.

- `KeepInMemory(max)`: a bucket held whole in memory, a `Set` past `max`
  refused, nothing evicted. A cache budget for the whole file comes first.
- A filter that answers a miss without SQLite.
- `History`, with `At(t)` and `ErrTooOld` before its window, and `Watch`.
- A GCRA limiter, one `int64` a key, and `AddWithin` for quotas.
- Listing a branch's branches; secondary indexes.

## What was measured

The prototype `spike/kv_*` on one AMD Ryzen 7 7700 with an NVMe disk, in a
`golang:1.27` container and on Windows 11, and on a production host with two
vCPUs while its services ran;
[the round](reports/kv-mechanics-2026-09-26.md) has the environment, the
commands and every figure.

| | Container | Windows | Production host |
|---|---:|---:|---:|
| durable `Set`s a second, 512 callers: a transaction each, grouped | 343; 53,338 | 654; 84,808 | 1,506; 54,971 |
| point `Get`s a second, one million sessions, eight callers: `View`, a statement | 187,403; 244,127 | 108,160; 132,204 | 58,529; 100,292 |
| the same at ten million sessions | 96,696; 111,084 | — | — |
| a `LoseAtMost` flush of 100,000 keys, new and onto them | 547 ms; 853 ms | 730 ms; 926 ms | 744 ms; 1,014 ms |
| `Clear` of 10,000 and 1,000,000 keys in one transaction | 35 ms; 5.3 s | 45 ms; 8.7 s | 52 ms; 11.1 s |
| production services that log their requests: busiest minute, peak second | | | 4.9 and 106 requests a second |

## Open

- **Whether a production host's acknowledged fsync is durable**: its virtual
  disk answered an fsync in about 0.6 ms, faster than this machine's NVMe.
- **The read path past eight readers**, `mmap_size`, and one page cache for the
  file rather than 1 MiB a connection: 64 MiB a connection helped on Windows
  and not in the container.
- **A spilled value's `Get`**, two rows, not timed.
- **The writer's cache** under random upserts into a large file: ten million
  sessions took 6 min 44 s to write, 25,000 a second.
- **Services that do not log their requests.** The production rates are a
  floor.
