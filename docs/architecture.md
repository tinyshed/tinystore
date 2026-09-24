# Architecture

The contract for the runtime and every engine beyond metrics. What is built
today is `codec/`, `internal/sqlite/` and `metrics/`; everything else here is
designed and agreed, not built. [samples/](samples/README.md) holds a compiling
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
path, err := store.Claim("metrics.db")  // reserve a file; ErrInUse if taken
err = store.Attach(engine)              // Close will close it
logger := store.Logger("metrics")       // the application's logger, engine=metrics
store.Every("metrics maintenance", time.Minute, engine.maintain)
now := store.Now()                      // the store's clock, replaceable in tests
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
- Metrics maintenance runs every minute by default: a forgotten `Maintain` fills
  heads until ingest refuses samples.
- `Options.Manual` stops all background work, and the application calls
  `Maintain` itself. Tests use it.
- `Close` cancels background work and waits for it before closing engines.
- Self-metrics and log flushing run only when enabled.
- `Every` is in memory. Durable, retried, leased work is the jobs engine.

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
  Today `metrics` defines its own; they move to the root with the runtime.
- An error about one item names it. An `Ingest` refused because of one series
  returns a `*metrics.SeriesError` carrying that series' labels (`errors.As`).
  `DropSeries(ctx, labels)`, which removes a series that cannot be repaired, is
  not built; see `docs/rewrite.md`.

## Engines

**metrics** (built). Contract: `AGENTS.md`, `metrics/README.md`,
`docs/aggregate-contract.md`.

**records** (designed). Structured records in `records.db`: time, level,
message and attributes, appended through the slog handler and read by time
range. Retention and indexing by attribute are decided when it is built.

**sqldb** (designed, prototyped). The application writes the schema and the
SQL; TinyStore owns the file, the connections, the migrations and the
transaction lifecycle.

```go
id, err := app.Scalar[int64](ctx, `insert into users (name) values (?) returning id`, name)
user, err := app.One[User](ctx, `select id, name, created_at from users where id = ?`, id)
users, err := app.All[User](ctx, `select id, name, created_at from users order by id`)
err = app.Transaction(ctx, func(tx *sqldb.Tx) error { … })
```

- `One`: no row is `sql.ErrNoRows`, two rows are `ErrManyRows`. `Scalar` reads
  one column. `Exec`, `Query` and `QueryRow` stay as in `database/sql`; there is
  no `Raw()`, so nobody closes the pool under the store.
- A struct takes columns by its `db` tag, or by field name in snake_case
  (`CreatedAt` → `created_at`); embedded structs' fields count. Mapping uses
  `reflect` without `MethodByName`.
- `Transaction`: nil commits, an error or a panic rolls back. `Tx` has the same
  methods. Generic methods need concrete types (Go 1.27 refuses them in
  interfaces), so `DB` and `Tx` are concrete.
- Migrations are `*.sql` files applied in name order, all pending ones in one
  transaction, with checksums of the applied ones verified on every open. A
  changed applied migration, or a database newer than the binary, refuses to
  open.

**kv** (boundary only). Mutable values by bucket and key in `kv.db`.

**blobs** (boundary only). Metadata in SQLite, bytes in files and segments
under `blobs/`; external bytes are durable before the metadata that points at
them commits, and readers hold a lease that collection respects.
`docs/storage-runtime-direction.md` has the reasoning.

**jobs** (boundary only). A durable queue in `jobs.db` with leases, retries and
schedules.

**Backup** (designed). One format for every engine: a zip holding
`manifest.json` (format, time, engines, schema versions, checksums) and a
`VACUUM INTO` snapshot of each file. Restore runs before `Open`, never on an
open store.

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
  and an application using them, as text files that build once renamed.
- Commit `407e728`, merged into `main`: the metrics ingest path rewritten in
  the target style, behaviour and bytes unchanged; see `docs/rewrite.md`.
