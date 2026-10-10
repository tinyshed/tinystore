# Protocol 2

The protocol is the API of every language but Rust (decision 2), so protocol
2 is the API books spelled as messages. Nothing was released, so protocol 1 is
not spoken: a client of protocol 1 is told the server's protocol in `GOAWAY`.

## What stays

The lower half of [docs/wire.md](../docs/wire.md) holds as it is, and is built
in Rust (`wire::frame`, `wire::msgpack`, `wire::session`) against every vector
of `testdata/wire/vectors.json`:

- frames, their kinds and flags, streams, `CANCEL`, credit, `PING`, `GOAWAY`;
- the MessagePack profile: a message is a map from small unsigned keys, a
  field at its zero value is left out, a request with a field the server does
  not know is `unimplemented`;
- `HELLO` and `WELCOME`, and the error codes, which are `ErrorKind`'s.

## What changes

- **The messages and methods, from the books.** A call of a book is one
  method; a handle a book opens is one method that answers a handle; what a
  book returns is one message. The names are the books', so an SDK maps a call
  to a method without a table of its own.
- **One schema is the source.** `protocol/<engine>.wire` declares every message
  and method. A generator writes the codecs of Rust, TypeScript, Python and Go
  from it, and `testdata/wire/protocol.json`, a vector of every message, so
  that an SDK cannot drift from the server: the hand-written declarations of
  protocol 1 drifted in each SDK.
- **Handles a kind.** `kv.bucket.open`, `kv.counters.open`,
  `kv.rateLimit.open`, `kv.quota.open`, `kv.once.open`, `jobs.queue.open`,
  each with its own options, rather than one `kv.open` with flags.
- **A transaction across the wire is a checked batch.** A host never holds the
  writer while it awaits: the SDK reads at once, keeps what each read found,
  and sends the writes in one `kv.tx` with a check of each read, a version or
  an absence; the server applies all or none, and a failure names its place
  in `what`, `check` or `write` and its index, so that the SDK runs the
  function again only when a read changed.
- **A run is handed over.** `kv.once.run` is a stream whose `REQUEST` leaves
  the client's side open: a kept answer ends it in the `RESPONSE`; otherwise
  the `RESPONSE` hands the run to the client, which sends what to keep, or
  nothing when its function failed, as its last `DATA`, and the server ends
  the stream once it is kept. A run held elsewhere answers when that one ends;
  a `CANCEL`, or a connection that ends, lets the run go to the next caller.
- **A decision is an answer.** A job's handler returns `snooze`, `retry` or
  `fail` as the outcome of its run, which `jobs.work` carries back.

## The schema

A small language, read like code and parsed once, by the generator:

```text
# A bucket of values by key; a comment documents what follows it.
message kv.BucketOpen {
  1 name: str               # [a-z0-9][a-z0-9_-]{0,63}
  2 ttl: duration?
  3 idle: duration?
}

message kv.Call {
  1 handle: uint
  2 under: [key]
  3 key: key
  4 value: value?
  5 ttl: duration?
  6 expiresAt: time?
  7 ifVersion: bin?
}

method 0x0101 kv.bucket.open(kv.BucketOpen) -> Handle
method 0x0102 kv.get(kv.Call) -> kv.Entry
method 0x010a kv.list(kv.List) -> kv.Page
method 0x0131 kv.once.run(kv.Call) -> handover kv.Answer
```

| Type                                         | On the wire                               | Rust                                             | TypeScript                                                        |
|----------------------------------------------|-------------------------------------------|--------------------------------------------------|-------------------------------------------------------------------|
| `bool`, `uint`, `int`, `float`, `str`, `bin` | as the profile says                       | `bool`, `u64`, `i64`, `f64`, `String`, `Vec<u8>` | `boolean`, `number`, `number`, `number`, `string`, `Uint8Array`   |
| `int64`                                      | int, past what a JavaScript number holds  | `i64`                                            | `bigint`                                                          |
| `duration`                                   | uint, milliseconds                        | `Duration`                                       | a duration's text or milliseconds, refused bare at the SDK's edge |
| `time`                                       | int, unix milliseconds                    | `SystemTime`                                     | `Date`                                                            |
| `nanos`                                      | int, unix nanoseconds                     | `i64`                                            | `bigint`                                                          |
| `key`                                        | str, or bin when not UTF-8, or an integer | `String`                                         | `string`                                                          |
| `value`                                      | nil, int or bin: a kv row as it is kept   | `Raw`                                            | `Raw`                                                             |
| `json`                                       | str                                       | `String`                                         | `unknown`, parsed                                                 |
| `[T]`, `{T}`                                 | array; map of names                       | `Vec<T>`, `BTreeMap<String, T>`                  | `T[]`, `Record<string, T>`                                        |
| `T?`                                         | absent when not given                     | `Option<T>`                                      | `T \| undefined`                                                  |

A field without `?` is absent at its zero value and reads back as it. A
method's shape follows its arrow: one message, `handover T` for a run handed
to the client and its answer back, `exchange T for U` for items both ways, a
worker's jobs out and its answers back, and `download T until U` for items as
`DATA` and a trailer, which records and blobs will use. `Failure` is the
schema's own message, the one a stream that failed ends with, so that no
schema leaves it out. Its `what` holds the facts a program acts on, by name:
a limit's `limit`, `wanted` and `bound`; a broken constraint's `constraint`,
`table`, `columns` (as SQLite writes them, `org, email`) and `name`; the
`check` or `write` a kv transaction failed at.

## The generator

`crates/protocol`, a binary nothing ships, reads `protocol/*.wire` and writes:

| Output                                  | For                                                                                                                      |
|-----------------------------------------|--------------------------------------------------------------------------------------------------------------------------|
| `crates/tinystore/src/wire/protocol.rs` | the session's request and answer types, decoded whole or refused; built                                                  |
| `sdk/js/src/wire/protocol.ts`           | the Bun and Node SDK; built                                                                                              |
| `testdata/wire/protocol.json`           | a vector of every message, each field at a value of its type and then none at all, which Rust and every SDK's suite read |
| `sdk/python/…/protocol.py`              | the Python SDK, with it                                                                                                  |
| the Go SDK's `protocol.go`              | the Go SDK, in phase 4                                                                                                   |
| the method tables of `docs/wire.md`     | the guide to the protocol, when it is rewritten                                                                          |

Every output is committed, `just protocol` writes them again, and CI fails when
they are not what the schema writes (`just protocol-check`). The Rust file is
left out of rustfmt, and the TypeScript one imports every codec, used or not,
so that a change of the schema never moves its header; biome lets its unused
imports be.

## kv

| Method                                         | Request                                                                                              | Answer                                                                                       |
|------------------------------------------------|------------------------------------------------------------------------------------------------------|----------------------------------------------------------------------------------------------|
| `kv.bucket.open`                               | name, ttl or idle, the database whose file keeps it                                                  | a handle                                                                                     |
| `kv.get`                                       | handle, under, key                                                                                   | entry: found, value, version, expires at                                                     |
| `kv.has`                                       | handle, under, key                                                                                   | found                                                                                        |
| `kv.set`                                       | call: value, ttl or expires at, if version                                                           | version, expires at                                                                          |
| `kv.create`                                    | call: value, ttl or expires at                                                                       | created; the version and expiry the key has                                                  |
| `kv.take`                                      | call: if version                                                                                     | found, value                                                                                 |
| `kv.delete`                                    | call: if version                                                                                     | found                                                                                        |
| `kv.expire`                                    | call: ttl or expires at, if version                                                                  | found                                                                                        |
| `kv.clear`                                     | handle, under                                                                                        | —                                                                                            |
| `kv.list`                                      | handle, under, after, limit                                                                          | a page: its entries, as many as the limit asks and the body holds, and where the next starts |
| `kv.counters.open`                             | name, ttl, flush every                                                                               | a handle                                                                                     |
| `kv.counters.add`, `.get`, `.delete`, `.clear` | handle, under, key, n                                                                                | the value; found                                                                             |
| `kv.rateLimit.open`                            | name, rate, per, burst                                                                               | a handle                                                                                     |
| `kv.quota.open`                                | name, windows                                                                                        | a handle                                                                                     |
| `kv.allow`, `kv.peek`, `kv.reset`, `kv.refund` | handle, under, key, n                                                                                | an allowance: ok, left, retry at, windows                                                    |
| `kv.once.open`                                 | name, keep                                                                                           | a handle                                                                                     |
| `kv.once.run`                                  | handle, under, key                                                                                   | both ways: the kept answer, or the run handed over and its answer back                       |
| `kv.once.get`, `.delete`                       | handle, under, key                                                                                   | the answer; found                                                                            |
| `kv.tx`                                        | checks: a read's version or absence; writes: set, create, take, delete, expire, clear, counters' add | each write's answer; a failed check or write names its place                                 |

## jobs

| Method                        | Request                                                                                                          | Answer                                                                                         |
|-------------------------------|------------------------------------------------------------------------------------------------------------------|------------------------------------------------------------------------------------------------|
| `jobs.queue.open`             | name, attempts, backoff, timeout, concurrency, rate, dedupe, keep, max waiting, the database whose file keeps it | a handle                                                                                       |
| `jobs.schedule.open`          | name, every or a cron and its time zone, attempts, backoff, timeout                                              | a handle on the schedule's one job, which no add, set or update takes                          |
| `jobs.add`, `.set`, `.update` | handle, id, value as JSON, at or delay, group, every or a cron and its time zone                                 | changed: whether an add added and an update changed; a set says nothing                        |
| `jobs.cancel`                 | handle, id                                                                                                       | changed: whether there was a job                                                               |
| `jobs.get`                    | handle, id                                                                                                       | the job: found, value, state, at, attempt, ahead, progress, error, group, repeat, its last run |
| `jobs.list`                   | handle, prefix, state, after, limit                                                                              | a page: its jobs, as many as the limit asks and the body holds, and where the next starts      |
| `jobs.work`                   | handle, concurrency, until idle                                                                                  | both ways: held jobs out, answers back                                                         |
| `jobs.step`, `.keep`          | a run, a step's name; keep's answer as JSON                                                                      | the answer kept, found false for none; nothing                                                 |
| `jobs.watch`                  | handle, id                                                                                                       | a download: the job as it is, then again each time it changes, until it ends                   |

`jobs.work` is a worker whose handlers are the client's. The server runs the
queue's loop on a thread of the store's, claiming as every worker does, and
sends each job it hands over as a held job, its run numbered on the
connection, no more unanswered at once than the client's `concurrency` and
within the stream's credit. The client answers each on the same stream: done,
retry, snooze, fail, or back, uncounted; a time as a delay counts on the
server's clock. A progress settles nothing.

- **A cancel** of a job the client holds sends it again, `cancelled`, so that
  its handler stops; what the client says of it after settles nothing.
- **A stop**, or the server's `GOAWAY`, hands no job more, and the server ends
  the stream once the jobs in hand are answered and written. With `untilIdle`
  it ends once nothing is due and nothing is held, which is `runDue`.
- **The client's `DATA`·END**, a `CANCEL` or a connection that ends fails the
  attempt of each job the client holds, as a worker that died would, and the
  jobs it was never sent go back uncounted.

`jobs.watch` reads the job again at every commit of its queue and sends it
when its state, place, attempt, time, progress or error changed; a client
behind is sent the latest, within its credit. The job's end is its last DATA,
done, failed or `cancelled`, and the stream's `DATA`·END follows; an id with
no job ends the stream at once, and the server's `GOAWAY` ends it
`unavailable`, to watch again on another connection.

## sql

| Method      | Request                                                                  | Answer                                                                                                               |
|-------------|--------------------------------------------------------------------------|----------------------------------------------------------------------------------------------------------------------|
| `sql.open`  | name, migrations: each file's name and SQL; none opens the file as it is | a handle                                                                                                             |
| `sql.query` | handle, text, the values of its `?`s, want: all, one or scalar           | a download: the rows in the RESPONSE when they fit one message, else their parts as DATA, the last ending the stream |
| `sql.exec`  | handle, text, values                                                     | changes, and the rowid of the row it inserted                                                                        |
| `sql.batch` | handle, statements                                                       | what each one changed                                                                                                |
| `sql.tx`    | handle                                                                   | both ways: calls in, answers out                                                                                     |

A statement travels as text and the values of its `?`s, each as SQLite keeps
it: nil, an integer, a float, a str or bin. An SDK builds its queries into
text and values before they leave, so the server sees SQL alone.
`sql.query` sends a write with `returning` to the writer and answers its rows
once its commit is durable; a part of the rows is at most half the client's
stream credit, the first naming the columns. `sql.exec` and `sql.batch` are
answered from the shared commit's completion, as kv's writes are.

`sql.tx` holds the database's writer. The client's calls come as DATA, each
answered as a DATA: its rows, what it changed, or why it failed, which leaves
the transaction as it was before the call. The client's last DATA·END says
whether to commit, and the server's DATA·END `{}` follows once it is done.
The server's own timer rolls a transaction back past five seconds, its
client's pauses included, and ends the stream `limit`; a CANCEL or the
connection's end rolls it back too.

A bucket or a queue opened with a database's handle, `database` in
`kv.bucket.open` or `jobs.queue.open`, lives in the database's file. Inside
`sql.tx` a call of theirs is a DATA whose `want` is `call`, the method and its
request in `method` and `body`; it runs in the transaction, in a savepoint of
its own, and its answer is the method's own, in `body`. A handle's first open
writes the file, so a client opens the database's handles before `sql.tx`,
whose transaction holds the writer an open would wait for.

## The server

| Method         | Request                                   | Answer                                                                   |
|----------------|-------------------------------------------|--------------------------------------------------------------------------|
| `server.stop`  | nothing                                   | nothing; then the server closes, refused by a store its program holds    |
| `server.clock` | a time to set, a span to move by, neither | the time it reads once moved; a private server's, refused on system time |

`protocol/server.wire` holds them; the connection's own messages, `HELLO`,
`WELCOME`, `GOAWAY` and `Failure`, are `protocol/connection.wire`'s. Neither
is an engine's, so neither is behind a feature.

## Open

- Whether method numbers stay fixed or `WELCOME` gives each engine its byte by
  name, as an engine of others will need ([architecture.md](architecture.md));
  built-in engines keep fixed bytes until then.
