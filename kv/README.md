# kv

An application's current state, in `kv.db` inside a `tinystore.Store`:
sessions, one-time codes, claims, drafts, flags, settings, a cache of another
service's answers. Buckets hold values of one type by key, keys sit in
branches, expiry runs on the store's clock and a version never repeats. The
design and the measurements behind it are [docs/kv.md](../docs/kv.md).

```go
state, err := kv.Open(ctx, store, kv.Options{}) // data/kv.db

sessions, err := kv.OpenBucket[Session](ctx, state, "sessions", kv.DefaultTTL(30*24*time.Hour))
codes, err := kv.OpenBucket[int64](ctx, state, "login-codes", kv.DefaultTTL(15*time.Minute))
seen, err := kv.OpenBucket[struct{}](ctx, state, "stripe-events") // a set
attempts, err := kv.OpenCounters(ctx, state, "login-attempts", kv.DefaultTTL(15*time.Minute))

err = sessions.Of(user.ID).Set(ctx, token, Session{Device: device})
s, found, err := sessions.Of(user.ID).Get(ctx, token)
page, err := sessions.Of(user.ID).Scan(ctx, kv.Query{Limit: 20})
userID, found, err := codes.Take(ctx, digest(code)) // read and burn
claim, first, err := seen.SetEntryIfAbsent(ctx, event.ID, struct{}{}, kv.TTL(10*time.Minute))
err = seen.Set(ctx, event.ID, struct{}{}, kv.IfVersion(claim.Version), kv.TTL(7*24*time.Hour))
n, err := attempts.Of("ip").Add(ctx, clientIP, 1) // 1, 2, 3…, and from 1 again fifteen minutes after the first
```

## Contracts

- **A key is text.** A string, a `[]byte` or an integer; an integer is its
  decimal spelling, so `42` and `"42"` are one key. Anything else, an empty
  key, or a path of owners and key over 1 KiB is `tinystore.ErrInvalid`.
  `Of(owners...)` names a branch by the same rule; a branch exists while it
  holds keys. `Scan` reads a branch's own keys, a page at a time, in the byte
  order of their text.
- **A value is kept by its type.** `[]byte` and `string` as their bytes; bool
  and integers up to 32 unsigned bits as an integer of the row; `uint64` as
  eight bytes; floats as their bits, `-0` and a NaN's payload included;
  `struct{}` as nothing, so a bucket of them is a set; anything else as JSON,
  and a value JSON cannot write is `ErrInvalid` at `Set`. `WithCodec` replaces
  the choice. A value over 512 bytes lives in a row of its own; one over 1 MiB
  is `ErrLimit`.
- **A counter is an int64 a key.** `OpenCounters` opens them: `Add` returns
  the new value, `Max` keeps the larger, and an absent or expired counter is
  0. An expired counter starts again from zero with the counters'
  `DefaultTTL`, and a live one keeps its expiry, so a window of attempts does
  not slide. A sum past the int64 range is `tinystore.ErrLimit` and changes
  nothing, since SQLite would quietly write a REAL. An option of one kind
  does not compile for the other: `DefaultTTL` fits both.
- **`LoseAtMost(d)` counters live in memory.** The first change of a key
  since the last flush reads it from the file once; after that `Add`, `Max`
  and `Delete` change memory and return, and every `d`, `Close` and `Maintain`
  write what changed, 10,000 keys a transaction. A crash loses at most the
  last `d` of changes, a `Delete`'s as well. Past 100,000 keys waiting a
  change flushes first. Handles on one name opened with the same `d` share
  the memory; another `d`, or none, is `ErrInvalid`, and so is such a counter
  inside `Tx`.
- **Expiry follows three rules.** A key created without `kv.TTL` or
  `kv.ExpireAt` gets the bucket's `DefaultTTL`, if it has one; a later `Set`
  keeps the expiry a live key has; `Touch` gives a new expiry and keeps the
  value and version. An expired key is absent to every call: reads do not see
  it, `SetIfAbsent` claims it, `IfVersion` conflicts with it. `Maintain`,
  every minute unless the store is Manual, deletes expired rows.
- **A version never repeats.** It is the revision of the file that wrote the
  value, kept across a delete, an expiry and a reopen. `kv.IfVersion` lets
  `Set`, `SetEntry`, `Take`, `Delete` and `Touch` apply only to a live key at
  that version; anything else is `tinystore.ErrConflict`. A version marshals
  to text and compares only for equality.
- **A write returns once it is durable.** Writes from many goroutines wait for
  the file's one writer and commit together, each in a savepoint of one
  transaction, with one fsync; a write that fails rolls back alone. A caller
  whose context ends before its write starts writes nothing; a write that has
  started finishes with its group. A group whose commit fails answers
  `kv.ErrOutcomeUnknown`, and its caller reads the key before writing again.
- **A read is one statement.** It sees the last commit before it, without a
  transaction of its own. `Store.View` reads several keys from one snapshot,
  held at most five seconds; `Store.Tx` writes several buckets in one
  transaction, nil committing, an error or a panic rolling back. A bucket works
  in either through `WithTx`; a write inside `View` is `ErrInvalid`.
- **An error names its key.** A call refused because of one key is a
  `*kv.KeyError` with the bucket and the path of owners and key; `errors.Is`
  finds the store's sentinel in it. A bucket name is `[a-z0-9][a-z0-9_-]{0,63}`
  and keeps its kind: a name holding counters does not open for values.

## Not built yet

`Sliding` expiry, `Clear`. [docs/kv.md](../docs/kv.md) says what each is to
be.
