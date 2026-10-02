# Architecture

The contract for the runtime and every engine beyond metrics. What is built
today is `codec/`, `internal/sqlite/`, `metrics/`, which opens through the
store, `sqldb/`, `records/`, `kv/`, `jobs/`, `blobs/`, `backup/`, and the root's lifecycle: `Open`, `Close`, the directory lock, `Claim`,
`Attach`, `Logger`, `Now`, `Every`, the memory budget and snapshots. Everything else here is designed and
agreed, not built. [examples/notes](../examples/notes/main.go) is a program
using the built part, and [samples/](https://github.com/tinyshed/research/blob/main/tinystore/samples/README.md) holds a reference
rewrite of one metrics path. A section that describes something unbuilt says so.

## What TinyStore is

An embedded data runtime for one Go program: exact metrics, structured records
(logs and events), SQL databases the application owns, KV, blobs and jobs, in
one directory, with bounded memory, no daemon and no cgo. A network service
wraps the same runtime from a module of its own, and `tinystore serve` runs
it; its SDKs are designed, not built. [server.md](server.md) is its contract
and [wire.md](wire.md) its bytes.

Each engine keeps its own semantics. The exactness rules in `AGENTS.md` are the
metrics engine's contract, not every engine's; an engine's contract lives in
its own README.

## One directory, a file per engine

```text
data/
├── metrics.db      metrics
├── records.db      records: logs and events
├── jobs.db         jobs
├── kv.db           kv
├── blobs/          blobs: blobs.db, uploads/, objects/
├── server/         SERVE and the socket, while the directory's sidecar runs
└── sql/
    └── app.db      databases the application names
```

- Every engine owns its file: its own `application_id`, migrations, writer and
  budgets. SQLite has one writer per file, so no engine ever waits on another
  engine's writer.
- No write is atomic across two files. The one flow that wanted it most, a
  row and the job it asks for, puts the queue in the row's file: a jobs store
  opened `In` an sqldb database keeps its tables there, named
  `_tinystore_jobs…` with a migration history of their own, and a `Batch`
  commits the job with the rows, one savepoint of one grouped commit. The
  application chooses that; by default each engine keeps its own file, and a
  file shared shares its writer. Materialising metrics from events is an
  idempotent recompute.
- A damaged file fails that engine's `Open` and nothing else.
- Application databases live only under `sql/`, so their names cannot collide
  with engine files. A name is `[a-z0-9][a-z0-9_-]{0,63}`; `Store.Claim` refuses
  a name already taken with `ErrInUse`.
- One store owns the directory. `Open` holds `data/LOCK` until `Close`, so a
  second store, in this process or another, is refused with `ErrInUse` at once
  rather than with a busy error on some later write. It is `flock` on Linux,
  macOS, the BSDs and illumos and an unshared `CreateFile` on Windows, both from
  `syscall`; the operating system releases it when the process dies. Where
  neither exists, `Open` logs a warning and runs unlocked.

## Opening: the store first, engines against it

```go
store, err := tinystore.Open(ctx, "./data", tinystore.Options{Logger: logger})
if err != nil {
	return err
}
defer store.Close(context.WithoutCancel(ctx)) // every engine, the last opened first

cpu, err := metrics.Open(ctx, store, metrics.Options{Retention: 30 * 24 * time.Hour})
events, err := records.Open(ctx, store, records.Options{})
app, err := sqldb.Open(ctx, store, "app", migrations, schema) // data/sql/app.db
```

Decided, with the alternatives that lost:

| choice | why |
|---|---|
| engines open against the store and the caller keeps the handle | like `database/sql`: no accessors, no registry, no type assertions, no half-open engine |
| `store.Metrics()`, `store.SQL("app")` rejected | the root would import every engine, and every program would link all of them |
| `metrics.Of(store)`, `tinystore.Engine[T](store)` rejected | a registry and a type assertion behind a call that looks free |
| declare engines, then `tinystore.Open(…, engines...)` rejected | an engine would exist before it is open, and every method would check |
| one `Close`, on the store | one lifecycle; an engine cannot be closed under another |
| no standalone `metrics.Open(ctx, path)` once the root exists | one way to open anything |

## What the store gives an engine

```go
path, release, err := store.Claim("metrics.db") // ErrInUse if taken; release if Open fails
err = store.Attach(engine)              // Close will close it
logger := store.Logger("metrics")       // the application's logger, engine=metrics
store.EveryEngine("metrics", "metrics maintenance", time.Minute, engine.maintain)
now := store.Now()                      // the store's clock, replaceable in tests
reserved, err := store.Reserve(ctx, n)  // n bytes of the store's memory, in arrival order
reserved.Shrink(m)                      // keep m of them, having learnt what the work holds
now, err := store.ReserveNow(n)         // never waits: ErrLimit unless n bytes are free
```

Every engine's `Open` has the same shape: claim, open and migrate its file,
attach, register background work, log that it opened. See `records.Open`,
the shortest of them.

These methods are exported because engines live in other packages; the
application may call `Every` too. Moving them behind an internal handle is
possible if exporting them turns out to be a mistake.

## Background work

- Only the store starts goroutines. Engines register periodic work with
  `Store.Every`; a failure is logged at Warn and the next run still happens.
  The same failure repeated is logged once per ten minutes with its count, and
  the first success after it at Info, so a full disk is not a line a minute:

  ```text
  00:00  disk full        → Warn "background work failed" failures=1
  00:01…00:09 the same    → counted
  00:10  disk full        → Warn failures=11
  00:11  success          → Info "background work recovered" failures=11
  ```
- Metrics maintenance runs every minute by default: a forgotten `Maintain` fills
  heads until ingest refuses samples.
- `Options.Manual` stops all background work, and the application calls
  `Maintain` itself. Tests use it.
- `Close` cancels background work and waits for it before closing engines.
- Self-metrics and log flushing run only when enabled.
- `Every` is in memory. Durable, retried, leased work is the jobs engine.

## Memory

`Options.Memory` bounds the bytes that every engine's in-flight work holds at
once. An engine reserves before it materialises (`Store.Reserve`); waiters are
served in arrival order, a cancelled waiter leaves the queue, and a reservation
larger than the whole budget is refused with `ErrLimit`. Work that learns its
size as it goes reserves its worst case and shrinks to what it holds. Work
holding a file's writer reserves with `ReserveNow`, which never waits: the
writes it would wait for hold their memory while they wait for that writer. Zero leaves each
engine to its own per-call limits; `Store.Memory` reports use, peak and
capacity. What was the metrics engine's `WorkBudget` lives here: a default
`Read` reserves its worst case,
about 34 MiB, so 256 MiB admits seven at once and queues the eighth.

Without `Options.Memory` each engine still bounds how much of its work runs at
once, and a call's weight: metrics through `MaxConcurrentReads`,
`MaxConcurrentIngest` and its batch limits, records through two reads, two
appends and 4 MiB an append. The gate that lets work in while an engine is
open, and the slots, are `internal/admission`, the same in every engine.

## Time

The store's clock is the one every engine reads (`Store.Now`, replaceable
through `Options.Clock`). An engine that keeps an observation by its time, a
sample or a record, accepts only what falls in its window, read once a call:

```text
now 12:00, Retention 14 days, ClockSkew 10 minutes → [12:00 fourteen days ago, 12:10]
older   ErrTooOld     it would never be read
later   ErrTooNew     it would hold what it lands in past retention
```

A record from a browser whose clock says 2100 would otherwise keep its segment
until 2100, and a sample from a wrong clock would hold its series' watermark in
the future for good. The window trusts the store's clock: a server running an
hour slow refuses correct data until `ClockSkew` is raised. Mimir and Loki make
the same trade, ten minutes by default. Schedules, which the jobs engine will
keep, are meant to be in the future and are not observations.

## Logs

In: TinyStore logs through the application's `*slog.Logger`
(`Options.Logger`, discarded when nil). Engines log through
`Store.Logger(name)`, which adds `engine=name`.

| level | what |
|---|---|
| Info | opened, closed; background work recovered |
| Warn | series suspended (with its file-local id and persisted reason), background work failed; a failed store Close |
| Error | corruption detected |
| Debug | a background maintenance pass or records flush, summarised |

This table describes the built event categories. Ordinary caller-visible
limit and query-deadline errors are returned, not logged again; migration
notifications are not implemented. [The diagnostics audit](https://github.com/tinyshed/research/blob/main/tinystore/reports/readability-2026-09-27.md)
records the original gaps, and [the logging round](https://github.com/tinyshed/research/blob/main/tinystore/reports/logging-2026-09-27.md)
states what changed. Background engine work uses `Store.EveryEngine` to keep
`engine=name` on failure and recovery records. The public `Store.Every` still
registers application work without an engine attribute.

Nothing is logged per sample or per query on a hot path. Debug summaries
check the level before building their fields. The supplied `Options.Logger`
is called synchronously: its application's handler determines logging
latency. The built-in records handler queues or drops without waiting.

Out: the records engine provides `Handler(stream) slog.Handler`, which keeps
an application's own lines and writes each to stderr as it is logged, pretty on
a terminal and JSON otherwise, so that one handler is the program's logger:

```go
logger := slog.New(events.Handler("notes", records.Redact("password")))
```

The handler never blocks its caller: a bounded queue, flushed by background
work and on `Close`; when it is full a record is dropped and counted. It refuses
lines from the records engine itself, or writing a log would log again.

## Self-metrics

With `Options.SelfMetrics: true`, the store captures available reports every
15 seconds and attempts one final report before closing the engines, bounded
to five seconds. A Manual store instead
calls `FlushSelfMetrics(ctx)`. Without an opened metrics engine it writes
nothing. The root imports no engine: `Reporter.Report` and `SelfWriter.WriteSelf`
are the interfaces by which an attached engine participates.

The runtime reports its reserved memory (not process RSS) and engine count.
Metrics and records report their existing counters; KV, jobs, blobs and SQL
do not yet supply reports. These become ordinary series:

```text
{Engine: "records", Name: "written_records", Value: 2}
→ tinystore_written_records{engine="records"}  2
```

`Stats()` stays each engine's pull API.
Reports are bounded to 256 fixed engine/name pairs, with names at most 64
characters; bucket, queue, key and path labels do not belong in them. Values
are unsigned integer counters or gauges. Integers above 2^53 are refused,
not rounded into float64. Self-writes do not increment user ingest/rejection
counters, and a reporter reads counters without running SQL or writing data.

## Errors

- The root package defines the shared sentinels (`ErrInvalid`, `ErrLimit`,
  `ErrClosed`, `ErrInUse`, `ErrConflict`, `ErrCorrupt`, `ErrTooOld`,
  `ErrTooNew`, `ErrSuspended`); engines wrap them, so `errors.Is` means the
  same everywhere. `metrics` keeps its own messages, and each of its errors
  wraps the root's.
- A limit a call reached is a `*tinystore.LimitError` where the engine knows
  its numbers: which bound, what the call would have taken of it, and the
  bound, `decoded samples: 120000 past 100000`, so that a caller can say which
  limit to raise or how much less to ask for. A query's budgets, the store's
  memory, a record's, a job value's and an object's size are named so far.
- An error about one item names it. An `Ingest` refused because of one series
  returns a `*metrics.SeriesError` carrying that series' name and labels
  (`errors.As`), and `DropSeries(ctx, name, labels)` removes a series that
  cannot be repaired; a records `Append` refused because of one record returns a
  `*records.RecordError`, and a read over a row that no longer reads a
  `*records.DamageError`, whose `Damage` is what `Drop` removes.

## Engines

**metrics** (built). Contract: `AGENTS.md`, `metrics/README.md`,
`docs/aggregate-contract.md`. `Ingest` stays for whoever receives samples from
elsewhere; an application measuring itself uses instruments:

```go
requests := stats.Counter("http_requests_total")
inflight := stats.Gauge("http_requests_inflight")
stats.GaugeFunc("notes", func(ctx context.Context) (float64, error) {
	n, err := sqldb.Scalar[int](ctx, app, `select count(*) from notes`)
	return float64(n), err
})

requests.With("route", "/notes").Inc()
inflight.Add(1)
```

The engine keeps their values in memory and ingests them every `Options.Flush`
(15 s by default) and on `Close`; a Manual store flushes on `stats.Flush(ctx)`.
What is stored is the value at each flush, so a gauge's resolution is the flush
interval, and a counter reset by a restart is a reset `increase` already
counts. A new label set beyond `MaxSeries` is refused with `ErrLimit` and
logged once. Histograms wait: bucketed approximations are not the exact answer
this engine promises.

**records** (built; contract in `records/README.md`, design in
[records.md](records.md)). Logs and events in `records.db`, one model: a time,
a stream the application names, an event name, an optional level and body,
trace and span ids, a context and attributes, every value kept as its JSON
spelling. The slog handler and `Append` write a durable head; maintenance
seals a head into a segment row and rows of up to 1024 records in event-time
order, each column written the cheapest exact way by computed size. `Read`
pages in event-time order from one snapshot, pruned by a covering time index,
level masks, the keys each segment holds and blooms over trace ids and id-like
attributes; `Follow` reads sealed segments through a `(segment, row)` cursor.
A quiet stream's small segments merge four of a size at a time, and each
keeps its place for that cursor. Retention removes whole segments, fourteen
days by default. On the research corpora it keeps 7.89 bytes a frontend
record and 16.55 a production log line, whose own time it keeps apart from
its text;
[the engine report](https://github.com/tinyshed/research/blob/main/tinystore/reports/records-engine-2026-09-25.md) and
[the stamps round](https://github.com/tinyshed/research/blob/main/tinystore/reports/records-stamps-2026-09-26.md) have the rest.

```go
logger := slog.New(logs.Handler("notes"))
err = logs.Append(ctx, records.Record{At: t, Stream: "web", Name: "click", Attrs: attrs})
page, err := logs.Scan(ctx, records.Query{Since: time.Hour, Attrs: []records.Field{records.String("requestId", id)}})
batch, err := logs.Follow(ctx, cursor, 1000)
```

### Where records is going (designed, not built)

Records will also take logs from programs that are not Go and not embedded,
through a server that is a module of its own: OTLP, JSON and plain text never
enter the root module's dependencies. The design follows from that caller.

Built since, inside the engine: `Lines(stream)`, a writer for another
program's output. It joins a record's lines at ingest by a continuation rule,
keeps a text line byte for byte, and a JSON object line or a logfmt line as
its fields when they spell it again, naming the record `log`, `json` or
`logfmt` so that it can be spelled back, and maps only the level. Its choices
differ from the design below in one place, measured on the production corpus:
a record's time is when its first line arrived, since a time whose zone a line
does not say cannot be placed and its text keeps it at a few bits.

**A stream names a source class, not every combination of metadata.** Producer
and session metadata can share context snapshots. Unique session and trace ids
must not create permanent streams, writers or buffers. Physical grouping,
dictionary scope and query indexes are separate decisions with separate
density, memory and selective-read gates; a mixed-source compression block
must not silently become a promise about indexed source queries.

**Each line is parsed on its own; a stream may only hint.** A line starting
with `{` is JSON, `key=value` pairs are logfmt, anything else is text whose
whole line is the body. A line that fails its hinted format is kept as text,
never dropped. Nested objects flatten to dotted keys (`http.status`); arrays
stay one JSON value. Time and level come from known keys (`time`, `ts`,
`@timestamp`, `level`, `severity`) or the start of a text line, else the
observed time is used. A full OpenTelemetry mapping remains a design
requirement.

**Lines become records before they are parsed.** A multi-line record, a Java
stack trace, is joined at ingest by a continuation rule, before any format is
detected.

**Text is kept byte for byte; JSON is kept by value.** A text body comes back
exactly, or its original is stored when a template cannot reproduce it. An
adapter keeps a JSON line's key order, integers as integers and fractions as
fractions, a number no int64 or float64 holds exactly as text, and an absent
key, `null` and `""` as three values; whitespace and escaping are not kept,
since a byte-exact JSON line costs its whole length. A stream that must keep
the original bytes (an audit) asks for it and pays.

**FTS5 is for the application's data.** A record is found by its time,
level, keys and blooms; FTS5, with its ranking and phrases, stays for `sqldb`
and the application's own text, where the first round measured it costing more
than the records it indexed.

**sqldb** (built; contract in `sqldb/README.md`, design in [sqldb.md](sqldb.md)).
The application writes the SQL; TinyStore owns the file, the connections, the
migrations, the values between Go and SQLite and the transactions. A struct
declares a table's columns, `Table[T]` what a struct cannot say, `Schema(...)`
prints the `STRICT` DDL, migrations stay `.sql` files that `Open` checks the
file against, a typed `Insert` writes one row, and queries stay SQL.

What a call's name starts with says where it runs: `Exec…` and `Insert` may
write and go to the file's one writer; everything else reads, on eight
`query_only` readers.

```go
user, found, err := sqldb.One[User](ctx, app, `select * from users where id = ?`, id)
users, err := sqldb.All[User](ctx, app, `select * from users order by id limit 50`)
count, err := sqldb.Scalar[int](ctx, app, `select count(*) from users`)

user, err = sqldb.Insert(ctx, app, Users, User{Email: email, CreatedAt: store.Now()})
user, found, err = sqldb.ExecOne[User](ctx, app, `update users set name = ? where id = ? returning *`, name, id)
result, err := app.Exec(ctx, `delete from sessions where expires_at < ?`, now)

err = app.Tx(ctx, func(tx *sqldb.Tx) error { … }) // several statements, one writer transaction
```

- A write sent to a read fails at once with `SQLITE_READONLY` and writes
  nothing; sqldb returns `ErrInvalid` naming the `Exec…` form to use.
- A read is one prepared statement without a transaction, its own snapshot;
  each connection keeps 128 compiled statements, the least recently used
  closed first. `One` answers `found`; `All` holds its rows in the store's
  memory up to 64 MiB; `Each` decodes a row at a time from a snapshot held at
  most five seconds.
- Rows decode through a plan found once for a type and its columns; a
  parameter is written by its Go type, as its column holds it: time as unix
  milliseconds, a known uuid as text, `JSON[T]` as JSON, `Date` as
  `YYYY-MM-DD`. A value that does not decode, or one SQLite would change, is
  `ErrInvalid` naming the column and the field.
- `Exec`s from many goroutines commit in groups, each in its savepoint; a
  group's ten seconds count from when it holds the writer, so a long `Tx`
  fails none of the writes behind it.
- The typed calls are package functions taking `*DB` or `*Tx`, as pgx's and
  sqlc's are, rather than generic methods, which would raise the Go version
  every importer needs; mapping uses `reflect` without `MethodByName`.
- Migrations are `*.sql` files applied in name order through
  `internal/sqlite.Migrate`, the one every engine uses: all pending ones in one
  transaction with foreign keys off and `foreign_key_check` before the commit,
  checksums of the applied ones verified on every open. A changed applied
  migration, another engine's file, a database newer than the binary, or a
  file that does not match the schema refuses to open.
- `sqldbtest.CheckSchema` compares the schema with what the migrations make,
  in the program's own tests, and `go tool tinystore migrate … new …`, of the
  module `cmd/tinystore`, runs that test to write the next migration.

**kv** (built; contract in `kv/README.md`, design in [kv.md](kv.md)).
The application's current state in `kv.db`: buckets of one value type with
text keys and branches, expiry by the store's clock, fixed or sliding, versions
that never repeat, writes from many goroutines committed together by
`internal/sqlite.File.UpdateGrouped`, point reads by `File.Lookup` without a
transaction, counters, `LoseAtMost` ones in memory between flushes, and
`Clear` of a branch however large.

```go
state, err := kv.Open(ctx, store, kv.Options{})
sessions, err := kv.OpenBucket[Session](ctx, state, "sessions", kv.Sliding(30*24*time.Hour))
s, found, err := sessions.Of(userID).Get(ctx, token)
```

**blobs** (built; contract in [blobs/README.md](../blobs/README.md), design
in [blobs.md](blobs.md)). The application's files in `blobs/`: rows in
`blobs.db`, bytes up to 16 KiB inline and above it a file each, synced and
renamed into `objects/` before the row that names it commits, so a crash
leaves no row naming missing bytes and the next open removes what uploads
left. A file is never written or renamed again, and a reader's lease is its
open file, opened through an `os.Root` so that Windows lets the engine remove
it underneath; a snapshot is what collection waits for, since it links the
files its copy of `blobs.db` names.

```go
objects, err := blobs.Open(ctx, store, blobs.Options{})
avatars, err := blobs.OpenBucket(ctx, objects, "avatars", blobs.MaxSize(20<<20))
obj, err := avatars.Of(user.ID).Put(ctx, "original", r.Body, blobs.Size(r.ContentLength))
```

**jobs** (built, a first slice: [jobs/README.md](../jobs/README.md); the
design is [jobs.md](jobs.md)). Work that
runs at its time, in `jobs.db`: typed queues whose jobs wait for their time, a
lease that gives a job back when its worker vanishes, retries, repeats kept as
cron text, at least once. Every call is one operation of a protocol a server
could speak to another language; `Work` is a loop over `Claim` and `Ack`.

```go
queues, err := jobs.Open(ctx, store, jobs.Options{})
reminders, err := jobs.OpenQueue[Reminder](ctx, queues, "reminders")
err = reminders.Enqueue(ctx, Reminder{User: 42, Text: "call mom"}, jobs.At(evening))
go reminders.Work(ctx, remind, jobs.Workers(4))
```

**Backup** (built). One format for every engine: a zip holding
`manifest.json` (format, time, engines, schema versions, sizes, checksums) and
a `VACUUM INTO` snapshot of each file. The root only copies files
(`Store.Snapshot`, over every engine implementing `Snapshotter`, into a
directory inside the store's own, so on the data's disk); the zip lives in the
`backup` package, so `archive/zip`'s 200 KiB is paid only by programs that back
up:

```go
err := backup.Write(ctx, store, file)                   // the store keeps working
err = backup.Restore(ctx, "./data", archive, size)       // before tinystore.Open
```
 A snapshot runs on a short-lived connection opened read-only at
the file: the `query_only` readers refuse `VACUUM INTO`, and a read-only file
lets the writer keep writing. Each file is one moment of its engine; two
engines are two moments, as no write spans them. `backup.Restore` runs before
`Open`, into an empty directory, and checks every size and checksum.

No engine gets a package before its first working code.

## Dependencies and weight

- The root module requires only `klauspost/compress` and `ncruces/go-sqlite3`
  (`TestTheModuleCarriesOnlyTheEngine`). A service or an integration is a module
  of its own.
- The root package imports no engine. Engines import the root and `internal/`,
  never each other. `internal/` has no engine vocabulary.

What linking costs, as a probe binary grows on 24 September 2026: linux/amd64,
Go 1.27.1, `CGO_ENABLED=0 go build -trimpath -ldflags="-s -w"` at `cad0ea8`,
against a program that only prints:

| a probe that | adds |
|---|---:|
| opens a file through `internal/sqlite`, creates a table, reads it | 5 004 KiB |
| does what `internal/sizeprobe` does with `metrics` | 6 716 KiB |
| the metrics probe plus one `slog.New(slog.NewTextHandler(…)).Info(…)` | +104 KiB |
| the metrics probe plus one `archive/zip` entry | +200 KiB |

SQLite is the floor every engine shares; an engine's own code is what a program
pays on top of it.

On 26 September 2026 `internal/sizeprobe` also opens `records` and uses its
handler, `Append`, `Maintain`, `Read`, `Follow` and `Drop`. Built the same way
from Windows 11 at the commit that adds this paragraph, the metrics probe adds
6 888 KiB and the probe with records 7 380 KiB: records costs a program 492 KiB
beside metrics.

Later on 26 September the probe also opens `kv` and uses a bucket's writes and
reads, a transaction and `Maintain`. Built the same way from Windows 11, it
adds 7 564 KiB, and 7 380 without `kv` at the same commit: kv costs a program
184 KiB beside metrics and records.

At the end of 26 September the probe also opens sliding sessions, walks them
with `All`, clears a branch and counts attempts in memory with `LoseAtMost`.
Built the same way from Windows 11, it adds 7 812 KiB, against 7 724 at
`7178c38` before them: the rest of kv costs 88 KiB.

On 27 September the probe also opens jobs: keyed, timed and repeating
enqueues in a transaction, an update, a cancel, a read and a walk, a schedule,
a claim extended and snoozed by hand, a Work loop and maintenance. Built the
same way from Windows 11, it adds 8 064 KiB, against 7 812 at `24d9b91`
without it: jobs costs a program 252 KiB.

On 27 September the probe also opens blobs: a Put and an upload, a reader
that seeks and reads a range, Stat, Copy, Move, a conditional Delete, a walk,
Usage, a Clear and maintenance. Built the same way from Windows 11, it adds
8 388 KiB, against 8 104 without it at `624263f`: blobs costs a program
284 KiB.

## Where the examples are

- `examples/notes`: a program using the store, `sqldb`, metrics instruments,
  `records`, `kv`, `jobs`, `blobs` and `backup`, built and tested with the
  module. It replaces the text prototype the runtime was designed from.
- Commit `407e728`, merged into `main`: the metrics ingest path rewritten in
  the target style, behaviour and bytes unchanged; see research's `rewrite.md`.
