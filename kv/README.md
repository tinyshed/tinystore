# kv

An application's current state, in `kv.db` inside a `tinystore.Store`:
sessions, one-time codes, claims, drafts, flags, settings, a cache of another
service's answers. Buckets hold values of one type by key, keys sit in
branches, expiry runs on the store's clock and a version never repeats. The
design and the measurements behind it are [docs/kv.md](../docs/kv.md).

```go
state, err := kv.Open(ctx, store, kv.Options{}) // data/kv.db

sessions, err := kv.OpenBucket[Session](ctx, state, "sessions", kv.Sliding(30*24*time.Hour))
codes, err := kv.OpenBucket[int64](ctx, state, "login-codes", kv.DefaultTTL(15*time.Minute))
seen, err := kv.OpenBucket[struct{}](ctx, state, "stripe-events") // a set
attempts, err := kv.OpenCounters(ctx, state, "login-attempts",
	kv.DefaultTTL(15*time.Minute), kv.LoseAtMost(time.Second))

err = sessions.Of(user.ID).Set(ctx, token, Session{Device: device})
s, found, err := sessions.Of(user.ID).Get(ctx, token) // and thirty more days from now, once a day
err = sessions.Of(user.ID).Clear(ctx)                 // signed out everywhere
for entry, err := range sessions.Of(user.ID).All(ctx) { … }
userID, found, err := codes.Take(ctx, digest(code)) // read and burn
claim, first, err := seen.SetEntryIfAbsent(ctx, event.ID, struct{}{}, kv.TTL(10*time.Minute))
err = seen.Set(ctx, event.ID, struct{}{}, kv.IfVersion(claim.Version), kv.TTL(7*24*time.Hour))
n, err := attempts.Of("ip").Add(ctx, clientIP, 1) // in memory: 1, 2, 3…, and from 1 again fifteen minutes on
```

[example_test.go](example_test.go) runs the five cases of
[docs/kv.md](../docs/kv.md) as examples, and `go doc` shows them.

## Contracts

- **A key is text.** A string, a `[]byte` or an integer; an integer is its
  decimal spelling, so `42` and `"42"` are one key. Anything else, an empty
  key, or a path of owners and key over 1 KiB is `tinystore.ErrInvalid`.
  `Of(owners...)` names a branch by the same rule; a branch exists while it
  holds keys. `Scan` reads a branch's own keys, a page at a time, in the byte
  order of their text: at most 1000 keys and 4 MiB of values a page, which
  ends before the value that would pass them, the next page beginning there.
- **A value is kept by its type.** `[]byte` and `string` as their bytes; bool
  and integers up to 32 unsigned bits as an integer of the row; `uint64` as
  eight bytes; floats as their bits, named or not, `-0`, a NaN's payload
  and whether it signals included;
  `struct{}` as nothing, so a bucket of them is a set; anything else as JSON,
  and a value JSON cannot write is `ErrInvalid` at `Set`. `WithCodec` replaces
  the choice. A value over 512 bytes lives in a row of its own; one over 1 MiB
  is `ErrLimit`.
- **`kv.Raw` is a value as its row keeps it**: nothing, an integer or bytes,
  an empty string as empty bytes. A bucket of `Raw` reads what a bucket of any
  type wrote under its name, and what it writes a bucket of a type kept the
  same way reads, so that a program without the types reads and writes every
  bucket; the server does, for its clients in other languages.
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
  write what changed, 10,000 keys a transaction. A crash loses the changes
  since the last flush that committed, a `Delete`'s as well: `d`, and on a
  busy file the time the flush waited for the writer and took. Memory holds at most 100,000
  keys, changed or not: a change of a key it does not hold flushes first when
  it is full, and while the file refuses that flush the change is refused
  with the flush's error. Handles on one name opened with the same `d` share
  the memory; another `d`, or none, is `ErrInvalid`, and so is such a counter
  inside `Tx`.
- **`Clear` removes a branch and every branch under it.** Up to 10,000 keys
  go in one transaction; a larger branch is marked cleared at the file's
  revision, which hides its keys from every call the moment it commits, and
  `Maintain` deletes them 10,000 a transaction. A key written after the Clear
  is a new key. Inside `Tx` a branch over 10,000 keys is `tinystore.ErrLimit`.
  A Clear of `LoseAtMost` counters drops what their memory holds under the
  branch once it commits, so no flush writes it again; one that fails leaves
  memory as it was, and one whose commit fails, `kv.ErrOutcomeUnknown`, lets
  go of the branch as a crash would.
- **`Sliding(term)` keeps a key term from its last read.** A key created gets
  term; a `Get`, `GetEntry` or `Has` renews a live key to term from now once
  a thirtieth of the term has passed since it last did, and `Scan` does not. A
  read writes nothing: the renewal waits a second for the flush, bound to the
  version and expiry the read saw, so it never extends a key written again,
  touched or deleted since. A key in its last minute is renewed before its
  read returns. `Sliding` beside `DefaultTTL` is `ErrInvalid`.
- **`All(ctx)` walks a branch** in `Scan`'s pages: `for entry, err := range
  sessions.Of(uid).All(ctx)`. No snapshot is held between pages, so a slow
  loop keeps no reader open, and a key written during the walk may or may not
  be met; `View` reads one snapshot.
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
  transaction, with one fsync; a write that fails rolls back alone. A `Take`
  decodes what it took inside its savepoint, so a value that no longer
  decodes, or a codec that panics, is `ErrCorrupt` and the value stays, inside
  `Tx` as well. A caller
  whose context ends before its write starts writes nothing; a write that has
  started finishes with its group. A group whose commit fails answers
  `kv.ErrOutcomeUnknown`, and its caller reads the key before writing again.
- **A read is one statement.** It sees the last commit before it, without a
  transaction of its own. `Store.View` reads several keys from one snapshot,
  held at most five seconds; `Store.Tx` writes several buckets in one
  transaction, nil committing, an error or a panic rolling back. A bucket works
  in either through `WithTx`; a write inside `View` is `ErrInvalid`.
- **A call holds the store's memory before it makes a value.** Under
  `Options.Memory` a write takes its turn and the memory its row may hold
  before it encodes: a `[]byte` or `string` its length, a number eight bytes,
  and a value JSON or a codec encodes the largest, 1 MiB, until the encoding
  says what it holds. A write waiting for its turn has made nothing. `Get`,
  `GetEntry` and `Take` hold the largest value while they read one,
  `SetIfAbsent` holds one beside its own for the value it may find, and a
  `Scan` page its 4 MiB and the row past them. A call inside `Tx` or `View`
  waits for none of it, since the calls holding memory wait for the writer or
  the reader it holds: it takes what is free, and past that it is
  `tinystore.ErrLimit` at once, for the caller to run it again.
- **An error names its key.** A call refused because of one key is a
  `*kv.KeyError` with the bucket and the path of owners and key; `errors.Is`
  finds the store's sentinel in it. A bucket name is `[a-z0-9][a-z0-9_-]{0,63}`
  and keeps its kind: a name holding counters does not open for values.

## Testing without waiting

A Manual store with a clock the test moves runs no background work, so
expiry, renewals and flushes happen when the test says:

```go
now := time.Now()
store, err := tinystore.Open(ctx, t.TempDir(), tinystore.Options{Manual: true, Clock: func() time.Time { return now }})
state, err := kv.Open(ctx, store, kv.Options{})
codes, err := kv.OpenBucket[int64](ctx, state, "login-codes", kv.DefaultTTL(15*time.Minute))

err = codes.Set(ctx, "code", 42)
now = now.Add(16 * time.Minute)
_, found, err := codes.Take(ctx, "code") // false: expired
_, err = state.Maintain(ctx)             // writes counters and renewals, deletes what expired or was cleared
```

## Not in the first version

What [docs/kv.md](../docs/kv.md) leaves for later: a bucket held in memory, a
filter that answers a miss without SQLite, history and watching, a rate
limiter, listing a branch's branches.
