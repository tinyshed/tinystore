# TinyStore

An embedded data runtime for Go, on SQLite: exact metrics first, then records,
SQL databases the application owns, KV, blobs and jobs, in one directory with a
file per engine, bounded memory, no daemon and no cgo. It is a library, so the
only thing a consumer sees is a handle and the promises this file makes about
it. `tinyshed/dashbin` is the first caller and not the owner.

This file is the contract for anyone — human or agent — changing the repository.
Keep it short and factual, and update it when a decision moves. Nothing here
should be a fact a ten-second grep would answer. The runtime and every engine
beyond metrics are specified in [docs/architecture.md](docs/architecture.md);
the rewrite of the metrics code for people follows [docs/rewrite.md](docs/rewrite.md).

## Status

Built: `codec/` (1..240 ordered samples to a checked body, every IEEE-754 bit
preserved, a fuzzed decoder that refuses corruption), `internal/sqlite/` (files,
one pinned writer with bounded prepared programs, bounded readers, checked
migrations) and `metrics/` (registry and postings with exact counts, a packed
durable head, atomic bounded ingestion, `Read`, `Stream` and a raw-decoding exact
`Aggregate` from one snapshot, version-checked sealing into merged groups,
batched publication, per-series quarantine, retention with series reclamation,
reopen). [metrics/README.md](metrics/README.md) states each of those contracts
and its limits; this file does not repeat them. The `tinystore` root holds a
directory and its lifecycle: `Open` with the directory lock, `Close`, `Claim`,
`Attach`, `Logger`, `Now`, `Every` and the memory budget (`Options.Memory`,
`Reserve`), and metrics opens through it, with instruments (`Counter`,
`Gauge`, `GaugeFunc`) for an application measuring itself. Every engine takes a
time only inside its window of the store's clock, and lets work in through the
gate and slots of `internal/admission`. `sqldb` gives the
application its own databases in `sql/<name>.db`: the application's SQL,
tables declared by structs and `Table[T]`, the `STRICT` DDL they print,
`.sql` migrations that `Open` checks the file against, a typed `Insert`,
values Go and SQLite agree on, point reads as one prepared statement, `Exec`s
committed in groups, and `sqldbtest.CheckSchema` with `go tool tinystore`,
the module `cmd/tinystore`, writing the next migration
([sqldb/README.md](sqldb/README.md), [docs/sqldb.md](docs/sqldb.md),
[the round](docs/reports/sqldb-mechanics-2026-09-28.md),
[the engine's](docs/reports/sqldb-engine-2026-09-28.md)). `records`
keeps logs and events in `records.db`: a
durable head, segments of event-time blocks written column by column, a line's
own time kept apart from its text, a quiet stream's small segments merged
four of a size, paged reads pruned by time, level, keys and blooms, a follow
cursor that a merge leaves in its place, a writer for another program's lines
that joins their stack traces, keeps JSON and logfmt lines as fields and finds
their levels, and rows that no longer read reported once and removed by `Drop`
([records/README.md](records/README.md)). `kv` keeps the application's
current state in `kv.db`: buckets of one value type and counters, keys in
branches, expiry by the store's clock, sliding or fixed, versions that never
repeat, writes committed in groups, point reads without a transaction,
counters kept in memory between flushes when a crash may lose a second of
them, and a branch cleared at once however large ([kv/README.md](kv/README.md)).
`Store.Snapshot` copies every engine's files while it works, blobs' linked
rather than copied, and `backup` writes those copies as one zip and restores
it before `Open`.

`records` is built to [docs/records.md](docs/records.md), except the
per-segment text sample, which one zstd frame a segment bounds at 0.48 bytes a
record on the production corpus, and text templates beyond a line's own time,
which cost more than zstd there; its format is version one and reads no
earlier prototype.
[examples/notes](examples/notes/main.go) is a program using all of it.

`jobs` keeps work that runs at its time in `jobs.db`: typed queues ordered by
time, leases and keys in tables of their own, retries, repeats kept as cron
text, and a Work loop that holds two jobs a worker and claims and settles in
one write ([jobs/README.md](jobs/README.md), [docs/jobs.md](docs/jobs.md),
[the round](docs/reports/jobs-mechanics-2026-09-27.md),
[the engine's](docs/reports/jobs-engine-2026-09-27.md)).

`blobs` keeps the application's files in `blobs/`: objects under keys that
are paths, their rows in `blobs.db` with the bytes up to 16 KiB, and above it
a file each, written in `uploads/` and renamed into `objects/` before the row
that names it commits; readers that keep what they opened, whole reads
checked by their SHA-256, `Copy` and `Move` that share the bytes, a `Clear`
however large, a scrub, and snapshots that link the files
([blobs/README.md](blobs/README.md), [docs/blobs.md](docs/blobs.md),
[the round](docs/reports/blobs-mechanics-2026-09-27.md)).

The server is being built to [docs/server.md](docs/server.md) and
[docs/wire.md](docs/wire.md), in `server/`, a module of its own: one
protocol for a sidecar that Bun and Python start and for a remote server,
MessagePack in twelve-byte frames over any byte stream, and no transaction
held across the network. Built: `server/wire` (frames, the profile, the
handshake, errors and kv's messages, with the vectors every SDK is tested
against), a session (credit, cancelling, GOAWAY, silence, workers that keep
their stacks, the first sender writing), Unix sockets, TCP and TLS with
tokens, Windows named pipes, and kv through `kv.Raw`, jobs with a remote
Work loop, blobs, sql, whose data connections pass a check of their own in
two lines, SQLite's tokens and the program it compiles, records, another
program's lines included, and metrics; and `tinystore serve`, a private child
on stdin and stdout, the directory's sidecar published in `server/SERVE`
under the store's lock and gone once idle, or a remote server with TLS and
tokens; [docs/server.md](docs/server.md) "Building it" says where each slice
stands and how the next goes on. Designed, not built: the SDKs, and
self-metrics.

Unfinished in metrics: the versioned exact summary shortcut for aggregates,
steady-state performance, and the gaps listed in `docs/rewrite.md`. Prototype
density figures are not engine guarantees. `spike/` preserves the experiments
behind the decisions.

Nothing is released: there is no tag, and no database written by an earlier
revision has to be read. Readers for earlier formats are deleted, not kept,
and an engine's schema changes inside its `0001` migration: a second one is
added only after a release.

Do not describe unbuilt behaviour as though it works.

## Shape

| Path                 | What it is                                                                    |
|----------------------|-------------------------------------------------------------------------------|
| `codec/`             | the block codec and the payload format. Knows samples and bytes, nothing else |
| `metrics/`           | the metrics API and its registry, head, groups, query and retention           |
| `sqldb/`             | the application's SQL databases: tables from structs, checked migrations, typed reads and writes |
| `records/`           | logs and events: a head, event-time segments, paged reads, a follow cursor    |
| `kv/`                | the application's current state: typed buckets, branches, expiry, versions    |
| `jobs/`              | work that runs at its time: queues ordered by time, leases, retries, repeats  |
| `blobs/`             | the application's files: objects by path, inline or a file each, checked reads |
| `backup/`            | every engine's file in one checked zip, and its restore before `Open`         |
| `internal/sqlite/`   | file handles, read/write transactions and checked migrations                  |
| `internal/admission/` | an engine's open gate and the slots that bound its concurrent work          |
| `internal/dirlock/`  | the directory's `LOCK`, one store a directory, per platform                   |
| `spike/`             | prototypes and measurements, skipped unless `TINYSTORE_SPIKE=1`               |
| `tools/`             | a second module pinning developer tools. Two files, never hand-edited         |
| `server/`            | a module of its own: the store served to other processes, sessions, listeners, handlers |
| `server/wire/`       | the protocol's bytes: frames, the MessagePack profile, messages, codes, vectors |
| `server/spike/`      | the server measured through `tinystore serve`, skipped unless `TINYSTORE_SPIKE=1` |
| `cmd/tinystore/`     | the one executable, a module of its own: `serve`, and `migrate` and `schema` for sqldb |
| `docs/`              | the design, the format, the numbers, the open questions; `reports/` the rounds |
| `examples/`          | programs using the public API, built and tested with the module               |
| `docs/samples/`      | where the reference rewrite of one metrics path lives                         |
| `.github/workflows/` | the authoritative clean builds                                                |
| `.agents/skills/`    | how the recurring work is done; `.claude/skills/` points to it                |

The first engine keeps its implementation in one package; split it only when
a dependency boundary needs a package, not to mirror the execution steps:

```text
tinystore (root)    the runtime: directory, lifecycle, logger, background work, shared errors
metrics/            public API and private implementation files
records/ sqldb/ …   one package per engine, each arriving with its first working code
internal/sqlite/    mechanics shared by every engine; no engine vocabulary
internal/admission/ the gate and the slots every engine lets work in through
internal/dirlock/   the LOCK that makes one store a directory, per platform
bench/              a module of its own: corpora, and other engines to measure against
```

Production gates belong beside their implementation. Keep historical spikes
until a real-engine harness can reproduce what they measured. Do not copy their
test-only parsers or call their helpers from production code. Records, sqldb,
KV, blobs and jobs get no placeholder packages, and engines never import each
other.

## Modules

Five, and the split is the point.

```text
root            what a caller links: the engine and nothing else
tools/          golangci-lint, govulncheck, task
bench/          corpora, comparison harnesses, whatever a measurement drags in
server/         the server: requires the root and nothing else; server/wire only the standard library
cmd/tinystore/  the one executable, go tool tinystore: requires the root and server/, nothing else
```

`cmd/tinystore` is a module of its own, as a service is: what it links to
serve, and later to back up and inspect a store, stays out of the library's
graph. `TestTheToolRequiresOnlyTheStoreAndTheServer` holds it to the root and
`server/`, so an application's `go tool` directive selects a root at least as
new as its tool's, and its `migrate` commands only run the application's own
test, so the comparison comes from the sqldb the application's graph selects.
`server/` is one for the same reason, and `TestTheServerRequiresOnlyTheRoot`
holds it to the root; a Go client links `server/wire` without SQLite,
`TestWireImportsOnlyTheStandardLibrary`. Until a release both reach what they
require through a `replace`. `task` tests, lints, formats and tidies both
beside the root.

The root module's dependency list is a promise rather than an accident:
`klauspost/compress` for zstd and `modernc.org/sqlite` for the file. Anything a
measurement needs — a generator, another engine's client, a container library —
belongs in `bench/`, which lives inside this repository and may therefore reach
`internal/` while its dependencies stay out of everyone else's graph.
`TestTheModuleCarriesOnlyTheEngine` fails when that list grows.

`go` and `toolchain` are deliberately different versions. `go 1.27.0` is the
floor imposed on everyone importing this; raise it only for a language feature
actually in use. `toolchain go1.27.1` pins what we build with and imposes
nothing.

## Runtime

Breaking one of these is a design change. [docs/architecture.md](docs/architecture.md)
has the reasons and the API; the rules hold for the runtime as it is built.

**One directory, a file per engine.** `metrics.db`, `records.db`, `jobs.db`,
`kv.db`, `blobs/`, and `sql/<name>.db` for databases the application names. No
engine waits on another's writer, and no write is atomic across two engines.

**The store opens first, engines open against it.** `metrics.Open(ctx, store, …)`,
`sqldb.Open(ctx, store, "app", migrations, schema)`. The caller keeps the handles; the
store has no accessors. One `Close` closes every engine, the last opened first.

**The root package imports no engine.** A program links the engines it opens
and nothing else. Engines import the root and `internal/`, never each other.

**Only the store starts goroutines that outlive a call.** A blocking call that
runs work, jobs' `Work`, starts its workers inside itself and waits for them
before it returns. Engines register periodic work with
`Store.Every`; `Options.Manual` stops all of it. Metrics maintenance runs by
default.

**Engine logs do not loop.** Engines log through `Store.Logger(name)` into the
application's `*slog.Logger`, never per sample. Background engine work uses
`Store.EveryEngine` so its failures and recovery keep the same engine attribute;
the records handler refuses its own lines. The records handler and a writer
of `Lines` never wait for their queue: they drop and count when full. An
arbitrary application-supplied slog handler controls its own call latency.

**Records has one logical model for logs and events.** A producer's language
is not a storage format, and template mining is optional for a text body.
Shapes and shared contexts are encoding choices with bounded lifetimes, not
permanent streams for every session id. The engine takes `slog` lines through
its handler and records through `Append`; [docs/records.md](docs/records.md) is
the model.

**A record's order is its event time.** A segment stores its records sorted by
event time, equal times in arrival order; `Append` order is not otherwise
observable. A query merges segments by time; a consumer follows segments in
publication order through a `(segment, row)` cursor, and a late record appears
in a later segment.

**What a query may skip is its own row.** SQL reads a blob column whole, its
overflow chain included, so a byte range inside a larger blob is not a
selective read. A row larger than a page leaves its remainder on a leaf page:
choose page size with row size, and measure the file, not the payload.

**An error names what failed.** Engines wrap the root's shared sentinels, so
`errors.Is` means the same in every engine; an error about one series carries
its labels.

**A time is taken only inside its engine's window.** An engine that keeps an
observation by its time accepts `[now − Retention, now + ClockSkew]`, `now`
read once a call from `Store.Now()`; outside it is `ErrTooOld` or `ErrTooNew`
naming the item. Without the upper edge one wrong clock holds a records
segment past retention and a metrics watermark in the future for good.

## Architecture

The metrics engine's invariants.

Breaking one of these is a design change, not an implementation detail.

**TinyStore knows samples, series, labels and time, and nothing of whoever
stores them.** No frame, no dashboard, no HTTP, no product vocabulary — and no
import of a caller, in the code or in the tests. What makes a move between
repositories cheap is the boundary rather than the path.

**A sample is kept bit for bit.** `-0`, NaN payloads and infinities are values
and not edge cases to normalise away, and a codec that "looks equal" after a
round trip is a codec that has lost something. Every transform proves the
original bits come back, one value at a time, or the block goes to another
representation. A rounding that is right for a chart is wrong for a store:
whoever draws the chart can round, and nobody can unround.

**The caller is told what it was given.** No silent downsampling, no range
snapped to a coarser grid, no approximate answer dressed as an exact one. A
later coarse tier may trade exactness for space, but it exposes its effective
resolution; the alternative is a number nobody can check.

**A query is bounded before it starts, and the bound is not the answer's size.**
One decode is capped at 8 KiB, which bounds a decode and not a query: a caller
asking for a thousand series over a month can hold every payload in memory
while streaming twelve hundred points out. Decoded samples, payload bytes,
blocks and series are separate budgets, and exceeding one is a resource error
rather than a quietly smaller answer.

**A sealed frontier is kept, and the compactor seals a prefix rather than a
selection.** `sealed_before` is stored per series and only sealing moves it,
inside the transaction that wrote the block; ingest also checks retention. A
watermark recomputed from whatever the head happens to hold after a restart has
forgotten what was already promised. And what is packed is what is strictly
before the watermark, not what a `limit` returned — an out-of-order window that
exists in the document and not in the block builder is the usual shape of
this bug. Ingest never moves `sealed_before`: it refuses what is behind it,
keeps the rest, and moves `max_seen_ts` once at the end of a batch, because
moving it sample by sample makes a backlog refuse its own neighbours halfway
through.

**What may be sealed trails the series; silence need not seal anything.** A
counter sample that arrives after its neighbourhood was packed does not make
the summary slightly wrong: `100 → 110 → 120` is an increase of 20 and
`100 → 110 → 5 → 120` is 130, and two blocks overlapping in time cannot have
their increases added. So lateness is bounded by a watermark that follows the
newest sample that series has shown — an agent back from a day away is not late
because the server moved on — and a sample behind it is refused rather than
reopening a block. The refusal is counted and visible, or data disappears
quietly. A quiet tail stays in the durable, queryable head until it is worth
packing or its samples expire. There is no forced closure on a wall clock.

**Retention clips the read before a summary is chosen.** A query captures one
cutoff and reads `[max(from, cutoff), to)`. A whole-block summary may be used
only when all its samples belong to that range and one requested aggregation
bucket; otherwise raw is filtered first, including for counters. Ingest refuses
a sample below either the cutoff or `sealed_before`. Retention removes expired
head points directly and blocks whose last sample is before the cutoff. An
overlapping block can hold expired samples physically without making them
queryable; freeing its pages neither shrinks the file nor promises byte erasure.

**Raw lives as long as its block in the first version.** A summary for 10:00
to 11:00 cannot answer 10:17 to 10:43, and five-minute buckets only narrow the
same boundary error. Whole blocks may be answered from their columns; a
block cut by a range is decoded and filtered. A later coarse tier keeps raw
for a sparse block whose rollup would be larger — a rollup is not
automatically smaller than what it replaces.

**A counter's increase is computed while the raw samples still exist.** A
summary holding `first`, `last` and a reset count cannot answer what a counter
did: on `100 → 110 → 5 → 20` the increase is 30, and 30 needs the value it
reset from. The compactor stores the increase so a whole-block query need not
decode raw and a later coarse tier cannot change the answer; the reset count is
diagnosis. A reset is visible only in time order, and samples arrive in any
order, so what is packed is sorted first.

**What a query sees is blocks and the head from one snapshot.** The head is
durable, the compactor writes a block and removes what it packed in one
transaction, and a reader takes both in one read transaction — otherwise a
compaction running beside it shows a sample twice or not at all, depending on
which half was read first.

**A read transaction covers fetching the bytes, not the query.** What one
snapshot hands back stays consistent after it ends, so decoding, merging and
building the answer happen outside the transaction; otherwise a heavy query is
itself the long reader below. And it gets a deadline, because the log's size is
the age of the oldest reader: three readers behaving like panels leave the
write-ahead log at a few megabytes, and one reader holding a snapshot for
twenty seconds takes it to two hundred and twenty.

**SQLite is single-writer, and the pools say so rather than the reviewer.** The
writer is one connection through `SetMaxOpenConns(1)`, so contention becomes a
Go mutex wait instead of an intermittent `database is locked`; the reader pool
is opened with `_query_only=1`, so a read path cannot write by mistake.
`_txlock=immediate` takes the write lock at `BEGIN`, because a deferred
transaction that upgrades on its first write gets a busy that `busy_timeout`
cannot wait out. Every pragma is per connection, which is why nothing in the
pool is allowed to expire and be reopened.

**Read SQL is prepared on its owning connection, with a bounded cache.** A
prepared program is not a cached result: each read still starts a new snapshot.
Do not pass connection-owned statements through `Tx.StmtContext`, which prepares
them again. Bound read limits use `LIMIT CAST(? AS INTEGER)` because a bare
`LIMIT ?` lets SQLite expire the program on rebinding and compile it again at
`step`; every caller supplies a checked integer. Keep both protections when
changing the read path.

**A statement by key runs to its end; one that walks a range keeps its
context.** `database/sql` and the driver each start a goroutine to watch a
context that can end, which cost a point read at depth 27 to 35 % of its rate.
`sqlite.QueryRowByKey` checks the context and runs without its cancel; a count
or a sum over a range goes through `sqlite.QueryRow`, which the context
interrupts. A grouped write's statements run without their caller's context
at all, since SQLite rolls back the whole transaction of a write statement it
interrupts, every write of the group with it: only the application's SQL,
through `sqlite.UntilDeadline`, ends at its caller's deadline.

**Nothing scans every series.** Postings are in the first version rather than
in an optimisation after it, and finding due work is an index over one value
per series rather than one per sample. A million active series without a
redesign is what the format and the query model are built for; what an
installation actually permits is a configured limit, and the two are not the
same promise. Cardinality and ingest rate are promised separately.

**The format carries its own version, and a released one never moves.** Codec
bodies, group directories and schema histories have their own versions and
golden readers. A module version is not a substitute for any of them.

**Merging directories must not relocate payloads.** A preceding group is absorbed
only when it has no more live blocks than the new group and the combination fits
the existing group and clock bounds. Version-three directories retain explicit
payload ids; values and summaries are neither decoded nor recomputed. Clock
ownership, directory replacement and the newly sealed prefix publish atomically.
`SealedBlocks` counts only new blocks, never the old blocks carried into a merge.

**A measurement is a number with its environment, or it is an anecdote.** Every
figure in `docs/` carries what produced it — machine or container, versions,
fixture, sample count — and the command that reproduces it. A candidate is
compared against what it replaces on identical input, in the same run. Take the
economics from a measurement and not the explanation of the mechanism: the
number is evidence, the story about why is a hypothesis until a second
measurement separates it from the alternatives.

**A storage measurement is a division of the file, not a total.** `dbstat`
reports every b-tree's own pages, so a change that claims to save space says
which object it took the bytes from and which object it gave them to. The
failure it exists to prevent is moving bytes to the next pocket and calling it
a saving: an index deleted here and an index created there net to zero, and
only a per-object report shows it.

**A payload's size is not a file's size.** SQLite stores rows in fixed pages,
so a payload that shrinks by a tenth can leave the file exactly as large, and
one that shrinks by a fiftieth can shrink it sharply by fitting one more row on
a page. Every codec result is therefore reported twice: bytes a sample in the
payload, and bytes a sample in a real file.

## Where things are written down

This file holds what gets broken: rules, invariants and traps. Read it before
changing something, not to look something up.

|                                              |                                                                  |
|----------------------------------------------|------------------------------------------------------------------|
| [docs/architecture.md](docs/architecture.md) | the runtime, the engines, their files, logs, errors and weight    |
| [docs/rewrite.md](docs/rewrite.md)           | how the metrics code is rewritten for people, and in what order  |
| [docs/samples/](docs/samples/README.md)      | the reference rewrite of one metrics path                        |
| [docs/design.md](docs/design.md)             | how the metrics store is meant to work, and why that shape       |
| [docs/aggregate-contract.md](docs/aggregate-contract.md) | exact aggregate arithmetic, resets, boundaries        |
| [metrics/README.md](metrics/README.md)       | the implemented metrics API, invariants and a runnable example   |
| [docs/reports/implementation-2026-09-21.md](docs/reports/implementation-2026-09-21.md) | the first slice and its measured limits |
| [docs/server.md](docs/server.md)             | the server and sidecar: modes, discovery, capabilities, limits   |
| [docs/wire.md](docs/wire.md)                 | the wire protocol's bytes: frames, credit, MessagePack, errors   |
| [docs/format.md](docs/format.md)             | the bytes: the payload's layout, version by version              |
| [docs/measurements.md](docs/measurements.md) | every number, its environment and how to reproduce it            |
| [docs/research.md](docs/research.md)         | what is not built: the open questions and their acceptance gates |
| [docs/reports/](docs/reports/README.md)      | the dated rounds every number above came from                    |

A reference a contributor returns to belongs in `docs/`, and a dated measurement
round belongs in `docs/reports/` with the environment and command that reproduce
it. A rule they are about to violate belongs here.

How to do the recurring work is a skill in `.agents/skills/`, which
`.claude/skills/` points to: `sdk`, `wire-change`, `measure`, `verify` and
`platform-traps`. Read the one that fits before you start, and fix it where it
is wrong, as you would this file.

## Rules that are gates

Every rule worth keeping is worth the twenty lines that make it fail loudly.

| Promise                                             | What enforces it                                                                |
|-----------------------------------------------------|---------------------------------------------------------------------------------|
| no cgo                                              | `CGO_ENABLED=0` in the build, on all three CI platforms                         |
| the module carries only the engine                  | `TestTheModuleCarriesOnlyTheEngine`, over its own go.mod                        |
| importing this stays cheap                          | `task size` links a cgo-free linux/amd64 probe and reports what it cost         |
| a `//nolint` silences a named finding and says why  | `nolintlint`: no unused, unexplained or blanket directive                       |
| the root links no engine                            | `TestTheRootImportsNoEngine`                                                    |
| one store holds a directory                         | `TestASecondStoreOnTheSameDirectoryIsRefused`                                   |
| engines close last opened first, once               | `TestCloseClosesEnginesLastOpenedFirstAndOnlyOnce`                              |
| a repeated background failure is not a log flood    | `TestBackgroundFailuresAreLoggedOncePerQuietPeriod`                             |
| an engine's errors are the store's kinds            | `TestMetricsErrorsAreTheStoresKinds`                                            |
| a failed engine open gives its file back            | `TestMetricsOpensOncePerStoreAndAFailedOpenLetsGo`                              |
| one refused instrument does not keep the others out | `TestARefusedInstrumentDoesNotKeepTheOthersOut`                                 |
| an instrument's last value survives Close           | `TestClosingTheStoreFlushesTheLastValues`                                       |
| engines never import each other, nor the server     | `TestEnginesDoNotImportEachOther`, over every engine package                    |
| an application's read cannot write                  | `TestAReadCannotWriteAndSaysWhereToWrite`                                       |
| an application's writes share a commit, fail alone  | `TestExecsShareACommitAndFailAlone`, `TestAPanicInsideAWriteRollsBackItsStatementAlone` |
| an applied migration cannot change under the file   | `TestMigrationsApplyOnceAndAChangedOneRefuses`                                  |
| a migration rebuilding a parent keeps its children  | `TestARebuiltTableKeepsItsChildren`, `TestMigrationsRunWithoutForeignKeysAndCheckThemBeforeCommit` |
| a schema is the SQL it prints                       | `TestASchemaIsTheSQLItPrints`, golden; `TestANameSQLWouldMisreadIsQuoted`        |
| a declaration that cannot be a table fails at start | `TestADeclarationThatCannotBeATableFailsAtStart`                                |
| an sqldb value comes back as it went in             | `TestEveryValueComesBackAsItWentIn`, `TestArgumentsAreWrittenByTheirGoType`     |
| sixteen bytes of a type sqldb does not know are bytes | `TestOnlyAKnownUUIDTypeIsText`                                                |
| a value that does not decode names column and field | `TestAValueThatDoesNotDecodeNamesItsColumnAndField`                             |
| a value SQLite would change is refused              | `TestAValueSQLiteWouldChangeIsRefused`: NaN, `uint64` past `int64`, a day no calendar has |
| `Insert` writes every field but the generated ones  | `TestInsertWritesEveryFieldButTheGeneratedOnes`, `TestInsertReturnsWhatTheDatabaseGenerated` |
| `Open` checks the file and changes nothing          | `TestOpenChecksTheFileAgainstTheSchemaAndChangesNothing`, `TestOpenNamesEachDifferenceOfStructure` |
| an expression spelled otherwise never refuses a file | `TestOpenDoesNotRefuseAnExpressionSpelledOtherwise`                            |
| a long transaction fails no grouped write behind it | `TestALongTransactionFailsNoWriteBehindIt`, `TestALongTransactionFailsNoGroupedWriteBehindIt` |
| a group's leader whose caller left hands the lead on | `TestALeaderWhoseCallerLeavesHandsTheLeadOn`                                   |
| a call on the DB inside its own Tx ends with its context | `TestACallOnTheDBInsideItsOwnTxEndsWithItsContext`                         |
| an sqldb snapshot ends at its bound and says so     | `TestEachHoldsOneSnapshotAndOneRow`, `TestASnapshotHeldPastItsBoundSaysSo`      |
| a constraint says its kind                          | `TestAConstraintSaysItsKind`                                                    |
| a statement is compiled once a connection           | `TestAStatementIsCompiledOnceAConnection`, `TestAConnectionKeepsTheStatementsItsFileWasOpenedWith` |
| sqldb holds the store's memory before it decodes    | `TestStoreMemoryBoundsReadsAndWrites`, `TestAllPastItsBoundRefuses`             |
| a schema check writes a migration only when asked   | `TestCheckSchemaFindsWhatIsMissingAndWritesOnlyWhenAsked`, `TestTwoChecksOfOneNameFail` |
| an ambiguous change is a draft that does not run    | `TestAnAmbiguousChangeIsADraftThatDoesNotRun`                                   |
| the tool finds each database by its name            | `TestTheToolFindsEachDatabaseByItsName`, `TestTheToolWritesTheNextMigrationThroughTheCheck` in `cmd/tinystore` |
| a log line never waits for the file                 | `TestAFullBufferDropsAndCountsWithoutWaiting`                                   |
| writing a log does not log again                    | `TestTheEnginesOwnLinesAreRefused`                                              |
| a line one record holds loses no byte; a longer one is dropped and counted | `FuzzLinesLoseNoByte`, `TestLinesKeepEveryByte`, `TestALargeLineWriteKeepsOnlyOneBoundedPartial` |
| a writer of lines never waits, and closes with the store | `TestLinesNeverWaitAndCloseWithTheStore`                                   |
| a stack trace's lines make one record               | `TestLinesJoinWhatBelongsTogether`, `TestAStackTraceGoesOn`                     |
| a line's level is found where its program writes it | `TestLinesFindTheLevelWhereProgramsWriteIt`                                     |
| a level in colour counts as one in brackets         | `TestAColourMarksALevelAsBracketsDo`                                            |
| a logfmt line keeps its pairs when they spell it    | `TestLogfmtLinesKeepTheirPairsWhenTheySpellTheLineAgain`, `TestLinesKeepALogfmtLinesPairs` |
| a record survives the records format exactly        | `TestSegmentsWrittenBeforeStillRead`, `TestHeadRowsWrittenBeforeStillRead`      |
| a changed records byte is refused                   | `TestAChangedOrMissingByteIsRefused`, `TestAChangedHeadRowIsRefused`, fuzzers   |
| a records decode stays bounded                      | `TestExpandedTextIsBounded`; every copy is charged before it is made            |
| a time a line spells comes back as it was spelled   | `TestEveryStampLayoutSpellsItsTextBack`, `FuzzStamps`                           |
| what no calendar shows stays text                   | `TestATimeNoClockShowsStaysText`                                                |
| a time kept against its record needs that record    | `TestStampsWithoutTheirTimesAreRefused`                                         |
| equal times keep their arrival order                | `TestEqualTimesKeepTheirArrivalOrder`                                           |
| a read merges segments and the head by time         | `TestReadMergesSegmentsAndTheHeadInEventTimeOrder`                              |
| an appended record reads before it is sealed        | `TestAppendedRecordsAreReadBeforeTheyAreSealed`                                 |
| a reader finds each record once while sealing       | `TestReadersSeeEveryRecordOnceWhileSealing`                                     |
| a failed seal leaves the head as it was             | `TestAFailedSealLeavesTheHeadAsItWas`                                           |
| an abrupt exit loses no appended record             | `TestAnAbruptExitKeepsEveryAppendedRecord`, before, inside and after a seal     |
| a damaged head does not stop the others             | `TestADamagedHeadDoesNotStopTheOthers`                                          |
| a damaged row is logged once, its head still seals  | `TestADamagedHeadRowIsReportedOnceAndTheRestOfItsHeadSeals`                     |
| a damaged segment is dropped whole, followed past   | `TestDropRemovesADamagedSegmentAndFollowPassesIt`                               |
| a follower keeps its place across merges            | `TestAFollowerKeepsItsPlaceAcrossMerges`, the middle of a merged place included |
| a merge is one write, and readers see a record once | `TestAFailedMergeLeavesTheSegmentsAsTheyWere`, `TestReadersSeeEveryRecordOnceWhileMerging` |
| a merged segment goes whole, with its places        | `TestAMergedSegmentExpiresWithItsPlaces`, `TestDropRemovesAMergedSegmentWithItsPlaces` |
| a holder's places are found through an index        | `TestPlacesAreFoundThroughTheirHolder`, on the plan SQLite chooses              |
| a follower far behind fetches a merged block once   | `TestAFollowerFetchesAMergedBlockOnce`, `TestTheFollowCacheKeepsWhatIsUsedWithinItsBytes` |
| a block's id is never given twice                   | `TestABlockIDIsNeverGivenTwice`                                                 |
| a merge joins four of a size, in time order         | `TestSmallSegmentsMergeFourOfASize`, `TestAMergeLeavesOverlappingSegmentsOut`   |
| only what no longer reads can be dropped            | `TestDropRefusesWhatStillReads`                                                 |
| a late record seals from its own head               | `TestLateRecordsSealFromTheirOwnHead`                                           |
| a record appended alone can be late                 | `TestARecordAppendedAloneCanBeLate`                                             |
| a batch in time order makes none of its own late    | `TestARecordTenSecondsBehindItsStreamsNewestIsLate`                             |
| a reopened store knows what its heads hold          | `TestAReopenedStoreKnowsWhatItsHeadsHold`                                       |
| a producer ahead of the store makes no one late     | `TestAProducerAheadOfTheStoreDoesNotMakeItsNeighboursLate`                      |
| a time outside its engine's window is refused       | `TestARecordOutsideItsWindowIsRefused`, `TestASampleAheadOfTheClockIsRefused`   |
| a page never splits a timestamp nor loses one       | `TestAPageNeverSplitsATimestamp`, `TestPagesContinueWithoutLosingOrRepeating`   |
| a budget ends a page rather than failing it         | `TestABudgetEndsAPageEarly`                                                     |
| blooms and level masks skip blocks                  | `TestBloomsAndLevelMasksSkipBlocks`                                             |
| a read walks the time index near its range only     | `TestTheTimeIndexIsWalkedWithinEachSpan`, on the plan SQLite chooses            |
| a read finds records in blocks of every width       | `TestReadsFindRecordsInBlocksOfEveryWidth`                                      |
| retention removes whole segments, clips reads       | `TestRetentionRemovesWholeSegmentsAndClipsReads`                                |
| records work holds the store's memory               | `TestStoreMemoryBoundsAppendReadSealAndFollow`                                  |
| a follower is told what retention removed           | `TestFollowCountsWhatRetentionRemovedFirst`                                     |
| a snapshot does not stop the writer                 | `TestSnapshotCopiesWhileTheWriterWrites`                                        |
| a backup restores every engine                      | `TestABackupRestoresEveryEngine`                                                |
| a changed backup is refused and leaves nothing      | `TestAChangedByteIsRefusedAndLeavesNothing`                                     |
| a sample survives the codec exactly                 | `TestEveryValueRepresentationPreservesBits`, on bits and not on values          |
| a sample survives a file and restart                 | `TestHeadSealingReopenAndPartialRetention`, over the public metrics API         |
| publication is one write                            | `TestFailedPublicationRollsBackPayloadsHeadAndIdentifiers`                       |
| a reader sees one consistent state                   | `TestReadersSeeOneSnapshotWhilePackingAndIngesting`, including retention        |
| shared clocks live as long as their owners           | `TestClockSharingAndLastOwnerRetention`                                         |
| production stays independent of research             | `TestEngineDoesNotImportExperimentsOrFutureEngines`                             |
| the extremes survive too                            | `TestExactBitsAndTimestampExtremes`, on `-0`, NaN payloads and the ends of time |
| a corrupt payload is refused                        | `TestPayloadCorruptionIsRefused` and `FuzzDecode`                               |
| bytes written once still read                       | `TestPayloadsWrittenBeforeStillRead`, one vector per representation             |
| a head that does not fit its body is refused        | the four moved heads in `TestPayloadCorruptionIsRefused`                        |
| a decimal travels as the integer it was written as  | `TestDecimalsTravelAsTheIntegersTheyWereWrittenAs`                              |
| a value no scale reproduces is refused, not rounded | `TestAValueNoScaleReproducesIsRefusedRatherThanRounded`                         |
| a decode stays bounded                              | `TestSmallBlockCodecMemory`, against the 8 KiB ceiling; opt-in, CI skips it     |
| an iterator outlives its codec                      | `TestIteratorOwnsItsBytesAndOutlivesTheCodec`                                   |
| unordered or oversized input is refused             | `TestRejectsUnorderedAndOversizedInput`                                         |
| a counter's increase survives a reset               | `TestCounterSummaryIncludesResets`, `TestAggregateCounterIncludesBlockTransitionButNotBucketTransition` |
| only the safe prefix is sealed                      | `TestWatermarkIsStrictAndFollowsTheSeries`, on the strict edge                  |
| a late sample cannot enter a sealed block           | `TestHeadSealingReopenAndPartialRetention`, `ErrTooOld` behind the frontier     |
| a partial range is not answered from a summary      | `TestAggregateRoundsExactSumAcrossSealedBlocks`: aggregates decode raw          |
| retention clips before it summarises                | `TestAggregateClipsRetentionBeforeSummingSealedEdges`                           |
| a bucket retention cut says so                      | `TestAggregateMarksOnlyTheBucketRetentionCut`                                   |
| a quiet tail expires without becoming a block       | `TestHeadSealingReopenAndPartialRetention`, the one-sample head at its end      |
| one expired sample does not delete a block          | `TestHeadSealingReopenAndPartialRetention`, the partly expired second block     |
| one damaged series does not stop its neighbors      | `TestCorruptSeriesDoesNotStopOtherMaintenance`                                   |
| a raised limit can resume suspended maintenance     | `TestSuspendedLimitCanRecoverAfterReopen`                                        |
| an append need not churn due indexes                | `TestExistingSeriesIngestAvoidsUnchangedDueIndexes`                             |
| untouched head chunks keep their bytes             | `TestUnchangedHeadChunksKeepTheirEncodedBytes`                                   |
| a posting above the old cap still ranks exactly    | `TestPostingCountsRankAboveTheOldProbeCap`                                       |
| ready waits for a safe prefix                       | `TestReadyWaitsForASealableWatermarkPrefix`                                      |
| writer programs stay bounded and transactional     | `TestPreparedWriterUsesOneTransactionAndRetainsPrograms` and `TestPreparedWriterCacheStaysBounded` |
| one bad publication does not count its neighbors   | `TestPublicationBatchRollsBackOneConflictingSeries` and `TestPublicationBatchDoesNotCountRolledBackTransaction` |
| cancelled callers do not bypass active-work slots   | `TestActiveReadAndIngestAdmissionHonorsCancellation`, `TestSlotsHonourCancellation` |
| a closed gate drains before an engine closes        | `TestAClosedGateRefusesWorkAndDrainsWhenTheWorkLeaves`                          |
| records reads and appends wait for their slots      | `TestReadsAndAppendsWaitForTheirSlots`                                          |
| one Append carries at most a segment's input        | `TestAnAppendOfMoreThanASegmentIsRefused`                                       |
| expired series return a cardinality slot            | `TestExpiredSeriesReclaimsCardinalityAndAllowsNewLifecycle`                  |
| live siblings keep shared dictionary pairs          | `TestReclaimKeepsLabelsUsedByAnotherSeries`                                   |
| narrow reads pay only for selected packed chunks     | `TestNarrowPackedHeadChargesSelectedChunksAndChecksWholeChecksum` and `TestBatchedNarrowHeadsChargeSelectedChunks` |
| an engine's work honors the store's memory           | `TestStoreMemoryBoundsReadsIngestAndMaintenance`                         |
| a large reservation is not passed over by small ones | `TestMemoryGrantsInArrivalOrder` and `TestMemoryCancelledWaiterLetsTheNextOneIn` |
| a reservation shrinks to what its work holds         | `TestAReservationShrinksToWhatItHolds`                                    |
| work holding a writer never waits for memory         | `TestAReservationThatCannotWaitTakesOnlyWhatIsFree`                       |
| streaming owns each result and exposes partial failure | `TestStreamOwnsResultsAndReportsPartialFailure`                         |
| an error about one series carries its labels           | `TestIngestRefusalNamesItsSeries` and `TestIngestValidationNamesItsSeries` |
| a series that cannot be repaired can still be dropped  | `TestDropSeriesRemovesAnUnreadableSuspendedSeries` and `TestDropSeriesKeepsItsNeighbours` |
| long-head append preserves bits and frontier           | `TestLongPackedHeadAppendKeepsExactBitsAndFrontier`                   |
| exact aggregates cross blocks, resets and retention    | `TestAggregateRoundsExactSumAcrossSealedBlocks`, `TestAggregateCounterIncludesBlockTransitionButNotBucketTransition` and `TestAggregateClipsRetentionBeforeSummingSealedEdges` |
| writes queued for the writer share a commit, fail alone | `TestGroupedWritesShareACommitAndFailAlone`                                    |
| a write whose caller left before its turn writes nothing | `TestACallerCancelledBeforeItsTurnWritesNothing`                             |
| a grouped write that has started finishes with its group | `TestAWriteThatHasStartedFinishesWithItsGroup`, cancelled or past its deadline mid-statement |
| the application's grouped SQL ends at its deadline     | `TestAStatementUntilItsDeadlineEndsThere`, `TestADataStatementEndsAtItsDeadline` |
| a statement by key starts no goroutine, nor runs once its context ended | `TestAStatementByKeyStartsNoGoroutine`, `TestAStatementByKeyWhoseContextEndedDoesNotRun` |
| a statement that walks a range ends at its deadline    | `TestARangeReadEndsAtItsDeadline`                                               |
| a value SQLite would make past the store's memory is refused | `TestAValueSQLiteWouldMakePastTheStoresMemoryIsRefused`                   |
| a download holds the store's memory until its last DATA | `TestADownloadHoldsTheStoresMemoryUntilItsLastData`                           |
| a remote server is bounded unless told otherwise       | `TestARemoteServerIsBoundedUnlessToldOtherwise` in `cmd/tinystore`: 1 GiB with `--listen` |
| a point read is its statement's own snapshot           | `TestALookupReadsEachStatementFromItsOwnSnapshot`                               |
| a kv write that returned survives an abrupt exit       | `TestAWriteThatReturnedSurvivesAnAbruptExit`, from many goroutines at once      |
| an expired key is absent to every operation            | `TestAnExpiredKeyIsAbsentToEveryOperation`                                      |
| expiry past one pass's bound is taken in ten seconds   | `TestAMaintainPastItsBoundIsFollowedSoon`, expired and cleared rows             |
| a default TTL is given once, at creation               | `TestADefaultTTLIsGivenOnceAtCreation`                                          |
| an integer key is its decimal text                     | `TestAnIntegerKeyIsItsDecimalText`                                              |
| a kv version never repeats                             | `TestAVersionNeverRepeatsAfterDeleteExpiryOrReopen`                             |
| a stale claim cannot finish or delete the next         | `TestAStaleClaimCannotFinishOrDeleteTheNext`                                    |
| a kv Take whose value no longer decodes keeps it       | `TestAFailedTakeKeepsItsValue`, a codec's panic and inside Tx included          |
| a kv value comes back as it went in                    | `TestAValueComesBackAsItWentIn`, floats by their bits, a named one's NaN that signals too |
| a `kv.Raw` is what its row holds, for every type       | `TestARawValueIsWhatItsRowHolds`, an empty string as empty bytes and not nothing |
| an overflowing counter is refused, not rounded         | `TestAnOverflowingCounterIsRefusedRatherThanRounded`                            |
| `LoseAtMost` loses no more than its interval           | `TestLoseAtMostLosesNoMoreThanItsInterval`, an exit that closes nothing         |
| counters of one name keep their numbers one way        | `TestCountersOpenAgainOnlyAsTheyWereOpened`                                     |
| a `LoseAtMost` counter joins no transaction            | `TestALoseAtMostCounterRefusesATransaction`                                     |
| counters in memory stay within their bound            | `TestCountersInMemoryStayWithinTheirBound`, `TestFailedChangesStayWithinTheBound` |
| cold counters arriving together pass no bound          | `TestColdCountersArrivingTogetherStayWithinTheBound`                            |
| a failed flush refuses new counters, loses none held   | `TestAFailedFlushRefusesNewCountersRatherThanHoldThem`                          |
| a kv Clear empties a branch and those under it         | `TestClearEmptiesTheBranchAndThoseUnderIt`, over the bound and under it         |
| a cleared key is absent to every operation             | `TestAClearedKeyIsAbsentToEveryOperation`                                       |
| a mark hides what lies under it at every depth         | `TestAMarkHidesWhatLiesUnderItAtEveryDepth`, deeper than the lookups included   |
| a Clear never brings back counters waiting to flush    | `TestAClearDoesNotResurrectCountersWaitingForTheFlush`                          |
| a failed Clear keeps what counters wait to flush       | `TestAFailedClearKeepsTheCountersWaitingForTheFlush`                            |
| a Clear whose commit fails lets go as a crash would    | `TestAClearWhoseCommitFailsLetsGoAsACrashWould`                                 |
| Clears beside changes and flushes keep branches apart  | `TestClearsBesideChangesAndFlushesKeepTheirBranchesApart`                       |
| a Clear inside Tx deletes what it clears or refuses    | `TestAClearInATransactionOverTheBoundIsRefused`                                 |
| a call inside a kv Tx or View waits for no memory      | `TestTxAndViewWaitForNoMemoryTheCallsWaitingForThemHold`                        |
| kv holds the store's memory before it makes a value    | `TestStoreMemoryBoundsWritesReadsAndScans`, `TestAWriteWaitingForMemoryHasEncodedNothing` |
| a sliding read writes at most once per refresh         | `TestASlidingReadWritesAtMostOncePerRefresh`                                    |
| a renewal never extends a newer incarnation of its key | `TestARenewalDoesNotExtendANewerIncarnation`, bound to version and expiry       |
| a key read in its last minute is renewed at once       | `TestAReadNearItsExpiryRenewsAtOnce`                                            |
| kv's All holds no snapshot between its pages           | `TestAllWalksEveryKeyAPageAtATime`                                              |
| a kv page ends before the value that passes its bytes  | `TestAPageEndsBeforeTheValueThatPassesItsBytes`                                 |
| an Enqueue that returned survives an abrupt exit       | `TestAnEnqueuedJobSurvivesAnAbruptExit`, from many goroutines at once           |
| a job runs at its time and not before                  | `TestAJobRunsAtItsTimeAndNotBefore`                                             |
| a write during a Work loop's read is not lost, nor keeps it awake | `TestAWriteDuringAnAlarmReadCannotBeLost`, `TestALaterWriteDuringAnAlarmReadLetsTheLoopSleep` |
| a jobs Scan finds exactly the keys under its prefix    | `TestScanFindsOnlyTheKeysUnderAPrefixNoRuneEnds`                                |
| a key names one job, and enqueuing it only brings it forward | `TestAKeyNamesOneJobAndARepeatOnlyBringsItForward`                        |
| an enqueue while its job runs asks for one run more    | `TestAnEnqueueWhileItsJobRunsAsksForOneRunMore`                                 |
| `KeepDone` makes a key run once                        | `TestKeepDoneMakesAKeyRunOnce`                                                  |
| `Update` changes only a job that still waits           | `TestUpdateChangesOnlyAWaitingJob`                                              |
| `Cancel` says whether it came in time                  | `TestCancelSaysWhetherItCameInTime`                                             |
| a job whose lease ended runs again                     | `TestAJobWhoseLeaseEndedRunsAgain`                                              |
| a stale lease settles nothing                          | `TestAStaleLeaseSettlesNothing`                                                 |
| a job that kills its process fails after its attempts  | `TestAJobThatKillsItsProcessFailsAfterItsAttempts`                              |
| a retry waits longer each time, then fails for good    | `TestARetryWaitsLongerEachTimeThenFailsForGood`                                 |
| a snooze counts no attempt                             | `TestASnoozeCountsNoAttempt`                                                    |
| a repeating job neither overlaps nor piles up          | `TestARepeatingJobNeitherOverlapsNorPilesUp`                                    |
| a schedule keeps its zone across daylight saving       | `TestAScheduleKeepsItsZoneAcrossDaylightSaving`                                 |
| `Work` settles by what the handler returns             | `TestWorkSettlesByWhatTheHandlerReturns`, `UntilIdle` with one worker included  |
| jobs due together are claimed in batches               | `TestJobsDueTogetherAreClaimedInBatches`                                        |
| a Work loop lets go of a lease another claim took      | `TestWorkLetsGoOfALeaseAnotherClaimTook`, without writing again at once         |
| a handler stopped by `Close` gives its job back uncounted | `TestCloseGivesRunningJobsBackUncounted`                                     |
| a handler that returns as `Work` ends settles as it returned | `TestAHandlerThatReturnsAsWorkEndsSettlesAsItReturned`, done, failed, stopped |
| a job value comes back as the JSON it went in          | `TestAValueComesBackAsTheJSONItWentIn`                                          |
| a value that no longer reads fails its job, not its queue | `TestAValueThatNoLongerReadsFailsItsJob`, through Claim and Work             |
| a queue past `MaxWaiting` refuses the next job         | `TestAQueuePastMaxWaitingRefusesTheNextJob`                                     |
| concurrent enqueues cannot pass `MaxWaiting`           | `TestConcurrentEnqueuesCannotPassMaxWaiting`                                   |
| a failed job is kept, then removed                     | `TestAFailedJobIsKeptThenRemoved`                                               |
| a jobs Scan page holds at most its jobs and bytes      | `TestAScanPageHoldsAtMostItsBytes`, spilled values counted                      |
| a job's key left behind names nothing, then is dropped | `TestAKeyLeftBehindNamesNothingAndMaintenanceDropsIt`                           |
| a job that moves takes its key along                   | `TestAMovedJobTakesItsKeyAlong`                                                 |
| jobs hold values in the store's memory                 | `TestStoreMemoryBoundsEnqueuesReadsAndHandlers`                                 |
| an Enqueue waiting for memory has written nothing      | `TestAnEnqueueWaitingForMemoryHasWrittenNothing`                                |
| a jobs Tx takes only the memory that is free           | `TestATransactionTakesOnlyTheMemoryThatIsFree`                                  |
| a Put that returned survives an abrupt exit            | `TestAPutThatReturnedSurvivesAnAbruptExit`, inline and in files                 |
| an object appears whole at its commit or not at all    | `TestAnObjectAppearsWholeAtItsCommitOrNotAtAll`, readers racing its replacements |
| an exit at any step of an upload leaves nothing behind | `TestAnExitAtEveryStepOfAnUploadLeavesNothingBehind`, the next Open cleaning up  |
| Open walks only what a crash can have left             | `TestOpenRemovesWhatAbandonedUploadsLeft`, past the settled mark alone          |
| a commit whose outcome is unknown leaves no file       | `TestAnUnknownCommitLeavesNoFileBehind`                                         |
| an upload that does not commit leaves nothing          | `TestAnAbortedOrAbandonedUploadLeavesNothing`, `TestMaintenanceAbortsAnUploadItsContextLeft` |
| a reader keeps what it opened, on Windows too          | `TestAReaderKeepsWhatItOpened`, through a delete, a replace, an expiry, a Clear |
| an Open racing a replace opens the new object          | `TestAnOpenRacingAReplaceOpensTheNewObject`                                     |
| a whole read of a changed byte fails before its end    | `TestAWholeReadOfAChangedByteFailsBeforeItsEnd`; a range is not checked         |
| the scrub finds what changed and names its keys        | `TestTheScrubNamesTheKeysOfWhatChanged`, `TestTheScrubKeepsItsPlaceAcrossReopens` |
| memory does not follow an object's size                | `TestMemoryDoesNotGrowWithAnObjectsSize`, `TestStoreMemoryBoundsUploadsReadsAndScans` |
| a Put grows its buffer only when memory is free now    | `TestStreamingUsesOnlyTheBufferItsBudgetCanHold`, `TestAFailedStreamReleasesItsLargerBuffer` |
| a Windows scanner's hold is retried and cleaned up     | `TestARenameRetriesAfterAWindowsScannerLetsGo`, `TestAHeldRenameExhaustsRetriesAndRecovers` |
| a stream that disagrees with its Size is refused       | `TestAStreamThatDisagreesWithItsSizeIsRefused`                                  |
| an upload past MaxSize or KeepFree leaves nothing      | `TestAnUploadPastItsBoundsStopsAndLeavesNothing`                                |
| of two conditional replaces, one conflicts             | `TestOneOfTwoConditionalReplacesConflicts`, `TestIfNoneMatchCreatesOnce`        |
| a copy shares the bytes and outlives its source        | `TestACopySharesTheBytesAndOutlivesItsSource`                                   |
| a blobs Clear empties a folder and those under it      | `TestClearEmptiesAFolderAndThoseUnderIt`, over the bound and under it           |
| a key is its own bytes on every file system            | `TestAKeyIsItsOwnBytesOnEveryFileSystem`, `TestAPathThatIsNotOneIsRefused`      |
| a file another program holds is removed later          | `TestAFileHeldElsewhereIsRemovedLater`, on Windows                              |
| a snapshot links the files, and removal waits for it   | `TestASnapshotLinksFilesAndCopiesTheDatabase`, `TestCollectionWaitsForASnapshot` |
| a backup restores every object bit for bit             | `TestABackupRestoresEveryObject`; past 4 GiB when `TINYSTORE_LARGE_BACKUP` is set |
| the blobs engine links no net/http                     | `TestBlobsImportsNoHTTP`                                                        |
| a frame past its agreed size is refused unread         | `TestAFrameLargerThanAgreedIsRefusedUnread`, `FuzzFrames` in `server/wire`      |
| a body the profile does not allow is refused           | `FuzzMessages`, a refused vector for each rule                                  |
| the vectors are the bytes                              | `TestVectors`, `TestFrameVectors`, `TestTheExamplesAreWhatTheMessagesWrite`     |
| a client past its credit is cut off, the reader never waits | `TestAClientPastItsCreditIsCutOff`, `TestFramesThatBreakTheProtocolEndTheConnection` |
| every stream ends with one final frame                 | `TestEveryStreamEndsOnce`, answered, failed, panicked, cancelled, silent, down and up |
| answers queued during a write leave in the next        | `TestQueuedAnswersShareAWrite` in `server/internal/flow`                        |
| a stream's number is free when its final frame arrives | `TestAStreamNumberIsFreeWhenItsFinalFrameArrives`                               |
| a closing server lets the streams running finish       | `TestClosingTheServerLetsTheStreamsRunningFinish`, `TestARequestThatCrossesTheGoAwayIsAnsweredUnavailable` |
| a remote connection needs its token                    | `TestARemoteConnectionNeedsItsToken`                                            |
| a pipe's name has one owner                            | `TestAPipesNameHasOneOwner`, on Windows                                         |
| a kv batch is one transaction                          | `TestAKVBatchRollsBackWhenOneOfItsCallsFails`                                   |
| Go and a wire client read each other's kv buckets      | `TestAWireClientAndAGoProgramReadEachOthersBuckets`                             |
| a remote worker's outcomes settle its jobs             | `TestARemoteWorkerSettlesByItsOutcomes`, a retry counted                        |
| a lost connection aborts uploads, fails attempts in hand | `TestALostConnectionAbortsUploadsAndFailsAttemptsInHand`, `TestALostWorkerFailsTheAttemptsInItsHands` |
| a jobs enqueue of many is one transaction              | `TestAJobsEnqueueIsOneTransaction`                                              |
| a blobs put that does not commit leaves nothing        | `TestAnUploadThatDoesNotCommitLeavesNothing`                                    |
| a whole blobs get of a changed object ends corrupt     | `TestAWholeReadOfAChangedObjectEndsCorrupt`                                     |
| `ApplyNone` and `Migrated` apply nothing               | `TestApplyNoneAndMigratedApplyNothing`, `TestVerifyChecksTheHistoryAndRunsNothing` |
| a statement SQLite refuses is `ErrInvalid`             | `TestAStatementSQLiteRefusesIsInvalid`, a missing argument included             |
| sqldb reads rows without a struct                      | `TestQueryReadsRowsAsSQLiteReturnsThem`                                         |
| the server module requires only the root               | `TestTheServerRequiresOnlyTheRoot`                                              |
| `server/wire` imports only the standard library        | `TestWireImportsOnlyTheStandardLibrary`                                         |
| a data client cannot change a schema                   | `TestADataClientCannotChangeTheSchema`, `TestEachLineOfTheCheckRefusesOnItsOwn`, `FuzzDataSQL` |
| the check ends a statement where SQLite does           | `TestSQLiteEndsAStatementWhereTheCheckDoes`, `TestTheCheckReadsSQLitesTokens`   |
| a database opens once, and later opens check it        | `TestSQLOpenAppliesOnceAndChecksAfter`, `TestADatabaseTheProgramOpenedIsChecked` |
| an sql batch is one transaction, a read one snapshot   | `TestAnSQLBatchIsOneTransaction`                                                |
| a data connection's statement ends at its deadline     | `TestADataStatementEndsAtItsDeadline`                                           |
| an answer past the agreed body fails only its stream   | `TestAnAnswerPastTheBodyIsALimit`                                               |
| a record over the wire comes back as it went in        | `TestRecordsOverTheWire`, times to the nanosecond, bytes that are not UTF-8     |
| an append over the wire names the record it refused    | `TestARefusedRecordNamesItsPlace`, all or none                                  |
| a follower over the wire comes back where it stopped   | `TestFollowOverTheWire`                                                         |
| another program's lines over the wire become records   | `TestLinesOverTheWire`, `TestLinesHandOverWhatTheyHoldWhenTheUploadEnds`        |
| a records drop is an admin's repair                    | `TestADropIsAnAdminsRepair`, `TestADamagedRowIsNamedAsADropNamesIt`             |
| a sample over the wire comes back bit for bit          | `TestMetricsOverTheWire`, -0 and a NaN's payload included                      |
| a metrics ingest over the wire is all or none          | `TestAMetricsIngestIsAllOrNone`, the refused series named by its labels         |
| an aggregate over the wire counts resets in its bucket | `TestAggregateOverTheWire`                                                      |
| a series longer than a body comes in pieces            | `TestALongSeriesComesInPieces`                                                  |
| `SERVE` is written whole, under the lock, for its owner | `TestServeIsWrittenWholeUnderTheLock`, `TestADirectoryIsItsOwnersAlone`        |
| a `SERVE` left behind goes before its server listens   | `TestAServeLeftBehindGoesBeforeTheServerListens`                                |
| a client reading `SERVE` delays a change, fails none   | `TestAChangeHeldUpByAReaderIsTriedAgain`, `TestAServeHeldPastEveryTryIsAnError` |
| a server goes idle only after its last connection     | `TestAServerGoesIdleAfterItsLastConnection`                                     |
| a long store's socket moves to the user's own directory | `TestALongSocketPathMovesToTheUsersOwnDirectory`, off Windows                  |
| a server proves it read `SERVE` to a local challenge only | `TestAServerProvesItselfOnlyToALocalChallenge`, `TestProofVectors`          |
| an endpoint taken after its server left cannot prove itself | `TestAnEndpointTakenAfterItsServerLeftCannotProveItself`                    |
| of two sidecars started at once, one exits held        | `TestAStaleServeStartsOneSidecar`, `TestASecondServeOfADirectoryExitsHeld`      |
| a sidecar leaves once idle, with its `SERVE` and lock  | `TestTheSidecarIsFoundThroughServeAndLeavesWhenIdle`                            |
| a private child leaves when told, though its parent stays | `TestAPrivateChildLeavesWhenToldThoughItsParentStays`, `TestAPrivateChildServesItsParent` |
| what serve cannot serve opens nothing                  | `TestServeRefusesWhatItCannotServe`, a file it cannot read included             |
| the tool requires only the store and the server        | `TestTheToolRequiresOnlyTheStoreAndTheServer`                                   |

`task check` runs exactly what CI gates on. When those two drift, the local one
is the weaker of the pair and a failure arrives after a push instead of before
it.

## Weight

This exists because the alternatives cost more memory than what they watch.

**No cgo, ever.** `CGO_ENABLED=0` in the CI build on all three platforms, so a
dependency needing a C toolchain fails on the pull request that introduces it,
and in `task size`, whose report refuses a probe built with cgo. This is why
the file is `modernc.org/sqlite`; do not swap it for a faster cgo driver. The
race detector is the one exception — it builds test binaries, never a product.

**A dependency is a decision, and here it is somebody else's decision too.**
Whatever the root module requires, every program importing this links. Check
what a module drags in, prefer the standard library, and put anything a
measurement needs in `bench/`.

**Watch the weight.** `task size` links a probe that calls the public API, for
linux/amd64 on every host, and reports what the import cost. There is no
threshold to game; the point is that growth is visible and deliberate.

**Nothing in the module calls `reflect.Value.MethodByName` with a non-constant
name.** Doing so switches off the linker's method pruning for the whole program
that imported us, measured at 3.4 MB on a program of Dashbin's size — a cost we
would be charging to somebody else's binary.

## Editing rules

- **Write for someone who has never seen this code**, reading top to bottom,
  once, without opening another package to find out what a name means. Every
  identifier has to survive that pass. **The fix is never a comment** — rename
  it, delete it, split it, or collapse the two branches into one. A comment
  that explains a confusing name leaves the name confusing.
- A comment says what the code cannot: why this way, what broke last time,
  which trap is being avoided. Never restate the line below.
- **Comment prose** is ordinary prose, in Go and YAML alike. A doc comment on a
  declaration, and any comment of more than one sentence, has a capital and
  full stops. A short inline comment that reads as a label (`// fast path`) may
  omit the final stop. A comment already in the file keeps its form until it is
  edited for another reason; nothing is reflowed to match.
  - A simple why is **one line**; two lines when one would be too wide.
    Wrapped prose stays within 80 columns, tabs counted as four. A single-line
    comment may run to 100 when that keeps a small fact on one line.
  - Avoid three or more lines that are still one sentence. Tighten it to one or
    two lines, or write proper sentences in paragraphs.
  - A long explanation is paragraphs separated by a blank `//` line, one piece
    of reasoning each: what it guarantees, why the obvious way is wrong, the
    edge case or invariant. The first sentence stands alone as the summary.
  - A table, a small diagram or a worked example beats prose when it explains the
    behaviour more directly.
  - If a type's comment starts explaining individual fields, move each
    explanation onto its field, unless the relationship between the fields is
    itself the invariant (`[Start, End)` belongs on the type). A field whose
    meaning its name and type already give gets no comment.
  - When editing an old comment, the facts may change order for clarity; what
    must survive is their meaning and the guarantees they state.
  - A good short comment is not lengthened for uniformity.
  - Where durability, concurrency, security, SQLite or file-system behaviour,
    protocol state or a memory bound is at stake, say enough that a reviewer need
    not rebuild the invariant from the code.
- **A worked example is the exception, and it is welcome.** Where a function
  transforms data — an encoding, a boundary, a merge — show one input and its
  output in a small aligned block (`12.02 → 1202 → +2`). Every such example
  is also a test case, so it cannot drift.
- **A public API speaks a small, familiar vocabulary**, in Go and in every
  SDK: a type is given once, where a bucket or queue opens; the daily calls
  are plain verbs; what the engine chooses goes into open's options, never into
  a call. A proposal shows the call site first.
- **A public method reads as a list of steps**: blank lines between them, each
  a call named with a verb, the details in the functions it calls. A function
  over 40 lines needs a reason and over 60 is split; no line passes 120
  columns; SQL is a named constant beside its function; more than four
  parameters become a named value. `docs/rewrite.md` shows it on real code.
- **One byte layout, one parser.** Decoding, reuse and partial reads all start
  from what it returns.
- **No file-level `//nolint`.** One line with its reason, or a helper that owns
  the conversion once.
- A test file is named after the file it tests: `head.go`, `head_test.go`.
- **A comment that restates its declaration is worse than none.** It costs a
  line, it ages on its own, and it teaches the reader that comments here can be
  skipped. When the name and the signature say it, write nothing. revive's
  `exported` rule is off for exactly this reason.
- Doc comments follow the same prose rules and the same restraint. They earn
  their place by saying what a caller cannot see — that the slice is not
  copied, that the iterator owns its buffer, that the codec may be closed while
  an iterator lives.
- Errors wrap with `%w` and carry what failed. A decoding error names the
  invariant that broke rather than the offset it was standing at.
- `context.Context` is the first parameter on anything touching the file or the
  scheduler.
- Do not weaken or delete a test to make a change pass. When behaviour changes
  on purpose, update the test and state the new contract.
- Do not commit machine-specific paths or a corpus; see **Research rounds**.

## Commits

One Conventional Commits subject line, English, imperative, lower case after
the type, scoped to the area touched.

```
feat(codec): pack exact decimals as scaled integers
fix(block): stop sealing past the watermark
chore(tools): bump golangci-lint to v2.14.0
```

No body, no footers, no trailers of any kind — including co-author and
generated-by trailers. The log is read as a changelog, so the subject carries
the whole message. `feat:` and `fix:` become changelog sections, so pick the
type for how the change reads to a user. A `fix:` says something a user had was
broken; repairing what the same unpushed work introduced is not that.

**Commit where the work is coherent, not every time it compiles.** A change
that exists only because an earlier one in the same push was wrong belongs
inside that commit, not beside it.

Do not commit or push unless you were asked to.

## Branches and releases

Work lands on `main` directly. Versions are `v0.x.y` until the API stops
moving, which in Go is not modesty but the compatibility statement the module
system reads. A release is a tag; tag nothing before there is something to run.

The payload format's version and the schema's version are not the module's. A
release may leave both where they are, and either may move without a release.

## Research rounds

A round is prototype code in `spike/` and one dated report in `docs/reports/`.
Both land on `main`. Neither is a product, and neither may be quoted as one.

**A round may be developed on a branch; it is not archived on one.** A report
names the commit it measured, so a round whose base lives only on a branch
somebody deleted is an anecdote with a number in it. The finding lands on `main`
with its report, and the commit it names is never rewritten afterwards — check
what the reports cite before touching history, because a rewrite that moves a
cited base silently unmakes every measurement standing on it.

**A report is dated in its filename and listed in the index.**
`docs/reports/<topic>-<date>.md`, one line in `docs/reports/README.md`, and the
machine, the versions, the corpus and the command inside the report itself. A
later round supersedes an earlier one by saying so in the earlier one, rather
than by editing the number it replaces.

**A reproduction command carries no path from the machine that ran it.**
`<repo>` for the repository, `<corpus>` for a prepared corpus. A real path is
useless to the reader, and a home directory is a username published for as long
as the history lasts.

**A corpus is fetched, never committed.** The runners in `bench/` download and
normalise one, `/bench/corpus/` is ignored, and a hash file is how a corpus is
pinned.

**A round is `test(spike):` and its report is `docs:`.** What the round proved
is worth building becomes its own commit with its own type: the evidence and the
feature are read by different people.

## Build and checks

```sh
task test             # go test -shuffle=on, which also runs the default vet analyzers
task lint             # golangci-lint
task fmt              # gofumpt + goimports
task fix              # go fix modernizers
task vuln             # govulncheck
task size             # what importing this costs a binary
task check            # everything CI gates on
task tidy             # both module files
```

Install Task with `go -C tools install github.com/go-task/task/v3/cmd/task`;
`task setup` then builds the rest into `./bin`.

Measurements are skipped unless `TINYSTORE_SPIKE=1`, so `task check` and CI do
not run them. Heavy ones belong in a linux container, or their numbers cannot
sit beside the others in `docs/measurements.md`:

```sh
docker run --rm -v <repo>:/src -w /src -e TINYSTORE_SPIKE=1 golang:1.27 \
  go test ./spike -run <name> -v -count=1
```

Property tests are `testing.F`, not a generator library: an in-package target
reaches unexported code and costs nothing in the graph. `task test` runs the
seed corpora; a real fuzz run is manual.

Run `task check` before pushing. Say which checks you ran and which you could
not.

## Reporting your work

State what changed, which checks ran, and which could not run and why. Point at
exact files and commands. Do not claim a platform was tested from a different
operating system. A number without its environment is not a result. Leave
unrelated working-tree changes alone.
