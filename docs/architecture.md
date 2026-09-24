# Architecture

The contract for the runtime and every engine beyond metrics. What is built
today is `codec/`, `internal/sqlite/`, `metrics/`, which opens through the
store, `sqldb/`, `records/`, and the root's lifecycle: `Open`, `Close`, the directory lock, `Claim`,
`Attach`, `Logger`, `Now`, `Every` and the memory budget. Everything else here is designed and
agreed, not built. [samples/](samples/README.md) holds a compiling
prototype of this API and a reference rewrite of one metrics path. A section
that describes something unbuilt says so.

## What TinyStore is

An embedded data runtime for one Go program: exact metrics, structured records
(logs and events), SQL databases the application owns, KV, blobs and jobs, in
one directory, with bounded memory, no daemon and no cgo. A network service, if
one comes, wraps the same runtime from a module of its own.

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
├── blobs/          blobs: objects, segments, packs
└── sql/
    └── app.db      databases the application names
```

- Every engine owns its file: its own `application_id`, migrations, writer and
  budgets. SQLite has one writer per file, so no engine ever waits on another
  engine's writer.
- No write is atomic across two engines. The one flow that wanted it,
  materialising metrics from events, is an idempotent recompute.
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
app, err := sqldb.Open(ctx, store, "app", migrations) // data/sql/app.db
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
store.Every("metrics maintenance", time.Minute, engine.maintain)
now := store.Now()                      // the store's clock, replaceable in tests
release, err := store.Reserve(ctx, n)   // n bytes of the store's memory, in arrival order
```

Every engine's `Open` has the same shape: claim, open and migrate its file,
attach, register background work, log that it opened. See
`samples/runtime/metrics/metrics.go.txt`.

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
larger than the whole budget is refused with `ErrLimit`. Zero leaves each
engine to its own per-call limits; `Store.Memory` reports use, peak and
capacity. What was the metrics engine's `WorkBudget` lives here: a default
`Read` reserves its worst case,
about 34 MiB, so 256 MiB admits seven at once and queues the eighth.

## Logs

In: TinyStore logs through the application's `*slog.Logger`
(`Options.Logger`, discarded when nil). Engines log through
`Store.Logger(name)`, which adds `engine=name`.

| level | what |
|---|---|
| Info | opened, migration applied, closed |
| Warn | series suspended (with its labels), a limit reached, a snapshot deadline, background work failed |
| Error | corruption detected |
| Debug | a maintenance pass or a flush, summarised |

Nothing is logged per sample or per query on a hot path, and attributes are not
built unless the level is enabled.

Out: the records engine provides `Handler() slog.Handler`, so an application
sends its own logs to both places with the standard library:

```go
logger := slog.New(slog.NewMultiHandler(console, events.Handler()))
```

The handler never blocks its caller: a bounded queue, flushed by background
work and on `Close`; when it is full a record is dropped and counted. It refuses
lines from the records engine itself, or writing a log would log again.

## Self-metrics

An engine that can describe its work implements `Report() []tinystore.Measure`;
the metrics engine also implements `WriteSelf`. With `Options.SelfMetrics` set,
the store writes every report as ordinary series:

```text
{Engine: "records", Name: "written_records", Value: 2}
→ tinystore_written_records{engine="records"}  2
```

`Stats()` stays each engine's pull API.

## Errors

- The root package defines the shared sentinels (`ErrInvalid`, `ErrLimit`,
  `ErrClosed`, `ErrInUse`, `ErrConflict`, `ErrCorrupt`, `ErrTooOld`,
  `ErrSuspended`); engines wrap them, so `errors.Is` means the same everywhere.
  `metrics` keeps its own messages, and each of its errors wraps the root's.
- An error about one item names it. An `Ingest` refused because of one series
  returns a `*metrics.SeriesError` carrying that series' labels (`errors.As`),
  and `DropSeries(ctx, labels)` removes a series that cannot be repaired.

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

**records** (built; contract in `records/README.md`). Structured records in
`records.db`: time, level, message and attributes, appended through the slog
handler and read by time range and level, at most 10000 at a time. Retention
is by age, fourteen days by default, checked hourly; records are indexed by
time only, and an index by attribute waits for a query that needs one.

**sqldb** (built; contract in `sqldb/README.md`). The application writes the schema and the
SQL; TinyStore owns the file, the connections, the migrations and the
transaction lifecycle.

What a method's name starts with says where it runs: `Exec…` may write and
goes to the file's one writer; everything else reads, on the `query_only`
readers.

```go
user, err := sqldb.One[User](ctx, app, `select id, name from users where id = ?`, id)
users, err := sqldb.All[User](ctx, app, `select id, name from users order by id limit ?`, 50)
count, err := sqldb.Scalar[int](ctx, app, `select count(*) from users`)

id, err := sqldb.ExecScalar[int64](ctx, app, `insert into users (name) values (?) returning id`, name)
user, err = sqldb.ExecOne[User](ctx, app, `update users set name = ? where id = ? returning id, name`, name, id)
result, err := app.Exec(ctx, `delete from sessions where expires_at < ?`, now)

err = app.Tx(ctx, func(tx *sqldb.Tx) error { … }) // several statements, one writer transaction
```

- A write sent to a read fails at once with `SQLITE_READONLY` and writes
  nothing; sqldb returns `ErrInvalid` naming the `Exec…` form to use.
- `One`: no row is `sql.ErrNoRows`, two rows are `ErrManyRows`; an `ExecOne`
  whose `returning` found no row is `sql.ErrNoRows` too. `Scalar` reads one
  column. There is no `Raw()`, so nobody closes the pool under the store.
- The typed reads are package functions taking `*DB` or `*Tx`, as pgx's and
  sqlc's are, rather than generic methods, which would raise the Go version
  every importer needs.
- A struct takes columns by its `db` tag, or by field name in snake_case
  (`CreatedAt` → `created_at`); embedded structs' fields count. Mapping uses
  `reflect` without `MethodByName`.
- `Tx`: nil commits, an error or a panic rolls back.
- Migrations are `*.sql` files applied in name order through
  `internal/sqlite.Migrate`, the one metrics uses: all pending ones in one
  transaction, checksums of the applied ones verified on every open. A changed
  applied migration, another engine's file, or a database newer than the binary
  refuses to open.

**kv** (boundary only). Mutable values by bucket and key in `kv.db`.

**blobs** (boundary only). Metadata in SQLite, bytes in files and segments
under `blobs/`; external bytes are durable before the metadata that points at
them commits, and readers hold a lease that collection respects.
`docs/storage-runtime-direction.md` has the reasoning.

**jobs** (boundary only). A durable queue in `jobs.db` with leases, retries and
schedules.

**Backup** (designed). One format for every engine: a zip holding
`manifest.json` (format, time, engines, schema versions, sizes, checksums) and
a `VACUUM INTO` snapshot of each file. The root only copies files
(`Store.Snapshot`, over every engine implementing `Snapshotter`); the zip lives
in the `backup` package, so `archive/zip`'s 200 KiB is paid only by programs
that back up. A snapshot runs on a short-lived connection opened read-only at
the file: the `query_only` readers refuse `VACUUM INTO`, and a read-only file
lets the writer keep writing. Each file is one moment of its engine; two
engines are two moments, as no write spans them. `backup.Restore` runs before
`Open`, into an empty directory, and checks every size and checksum.

No engine gets a package before its first working code.

## Dependencies and weight

- The root module requires only `klauspost/compress` and `modernc.org/sqlite`
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

## Where the samples are

- `samples/runtime/`: the runtime, a stand-in metrics engine, `records`, `sqldb`
  and an application using them, as text files that build once renamed. It
  predates the lock, the memory budget, instruments and the `Exec…` split of
  sqldb; where it differs, this file decides.
- Commit `407e728`, merged into `main`: the metrics ingest path rewritten in
  the target style, behaviour and bytes unchanged; see `docs/rewrite.md`.
