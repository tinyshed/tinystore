# TinyStore

An embedded data runtime for Go, on SQLite: exact metrics first, then records,
SQL databases the application owns, KV, blobs and jobs, in one directory with a
file per engine, bounded memory, no daemon and no cgo. It is a library, so the
only thing a consumer sees is a handle and the promises this file makes about
it. `tinyshed/dashbin` is the first caller and not the owner.

This file is the contract for anyone — human or agent — changing the repository.
Keep it short and factual, and update it when a decision moves. Nothing here
should be a fact a ten-second grep would answer. The guides in
[docs/](docs/README.md) say what TinyStore does for the people using it, and
each package's README what it promises. Why each engine has its shape is in
the design documents, kept in research's [tinystore/design][design] as they
were when the guides replaced them, beside the measurements, prototypes and
dated reports behind the decisions in
[tinyshed/research](https://github.com/tinyshed/research/tree/main/tinystore).

## Status

Built, each to its README, which states its contracts and limits, and its
design document:

| | What it is | Contract | Design |
|---|---|---|---|
| root | the directory and its lifecycle: `Open` under the lock, `Close`, `Claim`, `Attach`, `Logger`, `Now`, `Every`, the memory budget, `Snapshot`, `Dir` | [doc.go](doc.go) | [architecture.md][design-architecture] |
| `codec/` | 1..240 ordered samples to a checked body, every bit kept | [format.md][design-format] | [metrics.md][design-metrics] |
| `metrics/` | samples, exact reads, streams and aggregates, sealing, retention, instruments | [README](metrics/README.md) | [metrics.md][design-metrics] |
| `records/` | logs and events, a logger of the console and the store, another program's lines, paged reads, a follow cursor | [README](records/README.md) | [records.md][design-records] |
| `sqldb/` | the application's SQL databases, tables from structs, checked migrations | [README](sqldb/README.md) | [sqldb.md][design-sqldb] |
| `kv/` | buckets, counters, branches, expiry, versions, configs hot in every process, a GCRA limiter, quotas of several windows, answers kept once a key | [README](kv/README.md) | [kv.md][design-kv] |
| `jobs/` | queues ordered by time, leases, retries, repeats, a Work loop, where a job is and a watch to its end, its last run, a bound on those running, steps a run keeps across its attempts | [README](jobs/README.md) | [jobs.md][design-jobs] |
| `blobs/` | objects by path, inline or a file each, checked whole reads, a scrub | [README](blobs/README.md) | [blobs.md][design-blobs] |
| `backup/` | a snapshot as one checked zip, restored before `Open` | [backup.go](backup/backup.go) | [architecture.md][design-architecture] |
| `server/`, `cmd/tinystore` | every engine over one protocol: a sidecar, a private child, a remote server with TLS and tokens, a Go program's own store shared in one call; a person's `status`, `logs` and `serve`, and an agent's MCP tools | [wire.md](docs/wire.md) | [server.md][design-server] |
| `sdk/js`, `sdk/python` | the Bun and Node client and the Python one, tested against every vector and a real `tinystore serve` (`task sdk`) | [Bun and Node](sdk/js/README.md), [Python](sdk/python/README.md) | [sdk.md][design-sdk], [server.md][design-server] |
| `web/` | the docs as a site: every page prerendered from `docs/`, served by Bun into a TinyStore of its own, its views and readers' events kept there (`task web`) | [web/AGENTS.md](web/AGENTS.md) | the `docs` skill |

[design]: https://github.com/tinyshed/research/tree/main/tinystore/design
[design-architecture]: https://github.com/tinyshed/research/blob/main/tinystore/design/architecture.md
[design-format]: https://github.com/tinyshed/research/blob/main/tinystore/design/format.md
[design-metrics]: https://github.com/tinyshed/research/blob/main/tinystore/design/metrics.md
[design-aggregates]: https://github.com/tinyshed/research/blob/main/tinystore/design/aggregate-contract.md
[design-records]: https://github.com/tinyshed/research/blob/main/tinystore/design/records.md
[design-sqldb]: https://github.com/tinyshed/research/blob/main/tinystore/design/sqldb.md
[design-kv]: https://github.com/tinyshed/research/blob/main/tinystore/design/kv.md
[design-jobs]: https://github.com/tinyshed/research/blob/main/tinystore/design/jobs.md
[design-blobs]: https://github.com/tinyshed/research/blob/main/tinystore/design/blobs.md
[design-server]: https://github.com/tinyshed/research/blob/main/tinystore/design/server.md
[design-sdk]: https://github.com/tinyshed/research/blob/main/tinystore/design/sdk.md

Where the building differs from the design:

- `records` has no per-segment text sample, since one zstd frame a segment
  bounds the text at 0.48 bytes a record on the production corpus, and no text
  templates beyond a line's own time, which cost more than zstd there.
- In metrics steady-state performance is unfinished, with the gaps listed in
  research's `rewrite.md`. Prototype density figures are not engine guarantees.

Self-metrics are opt-in, `Options.SelfMetrics`: the store's memory budget and
the metrics and records engines' counters, written to its metrics engine as
ordinary series; kv, jobs, blobs and sqldb report nothing yet. An aggregate
answers a whole block inside one bucket from its exact summary; a cut block,
the head and a block without exact sums decode raw.

Not built: the SDKs' examples and published packages; a release; a host and a
domain for the site; the guides `docs/README.md` lists without a link.

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
| `internal/term/`     | whether a file is a terminal that shows colours, for a logger's console lines |
| `internal/dbstat/`   | a closed file's pages divided among its tables and indexes, for measurements  |
| `internal/release/`  | what a release makes: its tags, binaries, archives, npm packages, wheels, notes |
| `tools/`             | a second module pinning developer tools. Two files, never hand-edited         |
| `server/`            | a module of its own: the store served to other processes, sessions, listeners, handlers |
| `server/wire/`       | the protocol's bytes: frames, the MessagePack profile, messages, codes, vectors |
| `server/reach/`      | the Go client of a directory's server, found through `SERVE` and proven, for the tool |
| `cmd/tinystore/`     | the one executable, a module of its own: `serve`, `stop`, `status`, `logs`, `mcp`, `backup`, `restore`, and `migrate` and `schema` for sqldb |
| `sdk/js/`, `sdk/python/` | the clients of `tinystore serve` for Bun and Node, and Python; `sdk/go.mod` keeps them out of the Go module |
| `docs/`              | the guides, one page per feature in Go, Bun and Python, and the wire protocol; `README.md` is their table of contents and the site's sidebar |
| `web/`               | the docs site: SvelteKit prerendering `docs/`, and its Bun server under `server/`; `web/go.mod` keeps it out of the Go module |
| `examples/`          | programs using the public API, built and tested with the module               |
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
```

Production gates belong beside their implementation. Do not copy a
prototype's test-only parsers from research or call its helpers from
production code. Records, sqldb,
KV, blobs and jobs get no placeholder packages, and engines never import each
other.

## Modules

Four, and the split is the point.

```text
root            what a caller links: the engine and nothing else
tools/          golangci-lint, govulncheck, task
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
`klauspost/compress` for zstd and `ncruces/go-sqlite3` for the file. Anything a
measurement needs — a generator, another engine's client, a container library —
belongs in a module of its own, whose dependencies stay out of everyone else's
graph.
`TestTheModuleCarriesOnlyTheEngine` fails when that list grows.

`go` and `toolchain` are deliberately different versions. `go 1.27.0` is the
floor imposed on everyone importing this; raise it only for a language feature
actually in use. `toolchain go1.27.1` pins what we build with and imposes
nothing.

## Runtime

Breaking one of these is a design change. [architecture.md][design-architecture]
has the reasons as they were designed; the rules hold for the runtime as it is
built.

**One directory, a file per engine, unless the application joins two.**
`metrics.db`, `records.db`, `jobs.db`, `kv.db`, `blobs/`, and `sql/<name>.db`
for databases the application names. No engine waits on another's writer, and
no write is atomic across two files. A jobs store opened `In` a database keeps
its queues in that database's file, so that a `Batch` commits a job with the
rows it is about: what shares a file shares its writer, by the application's
choice and never by default. A guest engine's tables are `_tinystore_<engine>…`
with a migration history of their own.

**Writes known before they run share a commit; a Tx does not.** A `Batch` is
one savepoint of a grouped commit, built before it takes the writer, so the
program's code never runs while a group waits. A `Tx` holds the writer alone
and pays its own fsync, for reads that decide what to write.

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
the records handler keeps its own lines out of the store, though they reach its
console. The records handler and a writer
of `Lines` never wait for their queue: they drop and count when full. An
arbitrary application-supplied slog handler controls its own call latency.

**Records has one logical model for logs and events.** A producer's language
is not a storage format, and template mining is optional for a text body.
Shapes and shared contexts are encoding choices with bounded lifetimes, not
permanent streams for every session id. The engine takes `slog` lines through
its handler and records through `Append`; [records/README.md](records/README.md)
is the model.

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
the existing group and clock bounds. A directory keeps explicit payload ids;
values and summaries are neither decoded nor recomputed. Clock
ownership, directory replacement and the newly sealed prefix publish atomically.
`SealedBlocks` counts only new blocks, never the old blocks carried into a merge.

**A measurement is a number with its environment, or it is an anecdote.** Every
figure in `docs/` and in research carries what produced it — machine or
container, versions, fixture, sample count — and the command that reproduces it.
A candidate is compared against what it replaces on identical input, in the same
run. Take the economics from a measurement and not the explanation of the
mechanism: the number is evidence, the story about why is a hypothesis until a
second measurement separates it from the alternatives.

**A storage measurement is a division of the file, not a total.**
`internal/dbstat` reports every b-tree's own pages, as SQLite's `dbstat` does
where it is built, so a change that claims to save space says
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
| [docs/README.md](docs/README.md)             | the guides: what each engine does for its user, in Go, Bun and Python |
| each package's README                        | what it promises as built: its contracts, bounds and errors      |
| [metrics/README.md](metrics/README.md)       | the implemented metrics API, invariants and a runnable example   |
| [docs/wire.md](docs/wire.md)                 | the wire protocol's bytes: frames, credit, MessagePack, errors   |
| [web/AGENTS.md](web/AGENTS.md)               | the docs site: how a file becomes a page, the server, its analytics |
| research's [tinystore/design][design]        | why each engine has its shape, as designed: the runtime, metrics and [its exact aggregates][design-aggregates], the format, every engine, the server, the SDKs' vocabulary |
| [tinyshed/research](https://github.com/tinyshed/research/tree/main/tinystore) | the rounds, every number, the prototypes and the open questions |

What a user of TinyStore reads belongs in a guide in `docs/`, what a package
promises in its README, and a dated measurement round in tinyshed/research
with the environment and command that reproduce it. A rule a contributor is
about to violate belongs here.

How to do the recurring work is a skill in `.agents/skills/`, which
`.claude/skills/` points to with the same frontmatter: `api-change`, `sdk`,
`wire-change`, `verify`, `release`, `platform-traps` and `docs`. Read the one that fits
before you start, and fix it where it is wrong, as you would this file.
Measuring is research's `measure` skill.

## Rules that are gates

Every rule worth keeping is worth the twenty lines that make it fail loudly.

| Promise                                             | What enforces it                                                                |
|-----------------------------------------------------|---------------------------------------------------------------------------------|
| no cgo                                              | `CGO_ENABLED=0` in the build, on all three CI platforms                         |
| the module carries only the engine                  | `TestTheModuleCarriesOnlyTheEngine`, over its own go.mod                        |
| importing this stays cheap                          | `task size` links a cgo-free linux/amd64 probe and reports what it cost         |
| a program may link another SQLite driver beside it  | `TestTheExampleRegistersNoSQLDriver` in `examples/notes`: no name registered    |
| a `//nolint` silences a named finding and says why  | `nolintlint`: no unused, unexplained or blanket directive                       |
| a gate in this table names a test that exists       | `TestEveryGateNamesATestThatExists`, over every module's tests                  |
| a skill's pointer says what its skill says          | `TestEverySkillHasAPointerThatMatchesIt`                                        |
| a release's version is one every registry spells, after every release before it | `TestOnlyAPreReleaseEveryRegistrySpellsIsPublished`, `TestVersionsOrderAsSemanticVersioningSays` |
| what a release publishes says its version, stamped as it is built | `TestTheSDKPackageCarriesTheReleasesVersion`, `TestAPyprojectIsStampedWithTheReleasesVersion` |
| `feat` and `fix` subjects are a release's notes     | `TestNotesKeepFeaturesAndFixesUnderTheirSections`, `TestNotesBeginAfterThePreviousReleaseOfTheirKind` |
| the workflows parse before a release needs them     | `task lint:actions`, in CI's quality job                                        |
| the root links no engine                            | `TestTheRootImportsNoEngine`                                                    |
| one store holds a directory                         | `TestASecondStoreOnTheSameDirectoryIsRefused`                                   |
| engines close last opened first, once               | `TestCloseClosesEnginesLastOpenedFirstAndOnlyOnce`                              |
| a repeated background failure is not a log flood    | `TestBackgroundFailuresAreLoggedOncePerQuietPeriod`; `a repeated failure is said once a quiet period, and its recovery once` in `sdk/js/test/background.test.ts`, `test_a_repeated_failure_is_said_once_a_quiet_period_and_its_recovery_once` in Python's |
| an engine's errors are the store's kinds            | `TestMetricsErrorsAreTheStoresKinds`                                            |
| a failed engine open gives its file back            | `TestMetricsOpensOncePerStoreAndAFailedOpenLetsGo`                              |
| one refused instrument does not keep the others out | `TestARefusedInstrumentDoesNotKeepTheOthersOut`; `a refused instrument keeps no other out, and says so once` in `sdk/js/test/records.test.ts`, `test_a_refused_instrument_keeps_no_other_out_and_says_so_once` in Python's |
| an instrument's last value survives Close           | `TestClosingTheStoreFlushesTheLastValues`                                       |
| a timer writes its count, sum and longest at each flush | `TestATimerWritesItsCountSumAndLongestAtEachFlush`, `TestAFailedFlushKeepsATimersLongestForTheNext` |
| a timer's three series go together and have one writer | `TestATimersSeriesHaveOneWriter`, `TestARefusedTimerLeavesItsSeriesOutTogether`, `TestATimerTheSeriesLimitCutsWritesNoneOfItsSeries` |
| a timer's measure answers, rethrows, and records either way | `a timer writes its count, sum and longest at each flush; measure answers and rethrows` in `sdk/js/test/records.test.ts`, `test_a_timer_writes_its_count_sum_and_longest_at_each_flush` in Python's |
| self-metrics are opt-in, the last report before the engines close | `TestSelfMetricsAreOptInAndCollectBeforeClose`                    |
| a self-report never counts itself, nor rounds a value | `TestSelfSamplesDoNotCountThemselvesAndSurviveClose`, `TestSelfMetricsRefuseAmbiguousOrInexactReports` |
| engines never import each other, nor the server     | `TestEnginesDoNotImportEachOther`, over every engine package                    |
| an application's read cannot write                  | `TestAReadCannotWriteAndSaysWhereToWrite`                                       |
| an application's writes share a commit, fail alone  | `TestExecsShareACommitAndFailAlone`, `TestAPanicInsideAWriteRollsBackItsStatementAlone` |
| a batch commits whole in its group, and fails alone | `TestABatchCommitsTogetherAndFailsAlone`, `TestABatchThatDoesNotBuildWritesNothing` |
| a guest engine keeps its history in its owner's file | `TestAGuestKeepsItsOwnHistoryInItsOwnersFile`                                  |
| a job in a batch commits with its rows or not at all | `TestAJobInABatchCommitsWithItsRows`, `TestAJobGoesOnlyInTheDatabaseItsQueueLivesIn` |
| a job a Tx adds commits with its rows, and lets go of its turn either way | `TestAJobInATxCommitsWithItsRows` |
| a job in an SQL batch over the wire commits with its rows, on the program's own store when it passed one | `TestAJobInAnSQLBatchCommitsWithItsRows`, `TestAQueueInTheProgramsDatabaseIsTheProgramsOwn`; `a job a batch enqueues commits with the rows or not at all` in `sdk/js/test/sql.test.ts`, `test_a_job_a_batch_enqueues_commits_with_the_rows_or_not_at_all` in Python's |
| a Bun batch or view gives back what its function returns, answered | `a view and a batch give back what their function returns, its promises answered` in `sdk/js/test/kv.test.ts`, `a batch and a view give back what their function returns, its promises answered` in `sdk/js/test/sql.test.ts` |
| an applied migration cannot change under the file   | `TestMigrationsApplyOnceAndAChangedOneRefuses`                                  |
| a migration rebuilding a parent keeps its children  | `TestARebuiltTableKeepsItsChildren`, `TestMigrationsRunWithoutForeignKeysAndCheckThemBeforeCommit` |
| a schema is the SQL it prints                       | `TestASchemaIsTheSQLItPrints`, golden; `TestANameSQLWouldMisreadIsQuoted`        |
| a declaration that cannot be a table fails at start | `TestADeclarationThatCannotBeATableFailsAtStart`                                |
| an sqldb value comes back as it went in             | `TestEveryValueComesBackAsItWentIn`, `TestArgumentsAreWrittenByTheirGoType`     |
| a value comes back as SQLite keeps it, no time read into its text | `TestAValueComesBackAsSQLiteKeepsIt`, `TestAValueTravelsAsItsRowKeepsIt` in `server` |
| bytes a caller scanned are its own                  | `TestABlobScannedAgainLeavesTheLastOnesBytes`                                   |
| a call prepares one statement and refuses a second  | `TestACallPreparesOneStatement`, `TestSQLiteEndsAStatementWhereTheCheckDoes`    |
| sixteen bytes of a type sqldb does not know are bytes | `TestOnlyAKnownUUIDTypeIsText`                                                |
| FTS5 and R*Tree work in an application's file and its snapshot | `TestFullTextAndRTreeTablesWorkInTheFileAndItsSnapshot`                  |
| a virtual table's shadow tables are not the application's | `TestAVirtualTablesShadowsAreNotTheSchemas`                                 |
| a file's pages are divided with none counted twice or left over | `TestEveryPageIsCountedOnce`, `TestAPageCountedTwiceOrNeverIsRefused`    |
| a value that does not decode names column and field | `TestAValueThatDoesNotDecodeNamesItsColumnAndField`                             |
| a numbered SQL parameter without its argument is refused | `TestAStatementWithNumberedParametersNeedsEveryArgument`                    |
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
| what a full buffer dropped is said, once a quiet period | `TestDroppedLinesAreSaidOncePerQuietPeriod`; `dropped lines are said at a flush, once a quiet period, counted since the last time` in `sdk/js/test/background.test.ts`, `test_dropped_lines_are_said_at_a_flush_once_a_quiet_period_counted_since_the_last_time` in Python's |
| a Bun logger's line never waits for the server     | `a full logger drops and counts rather than wait` in `sdk/js/test/records.test.ts` |
| a Bun logger writes what filled its buffer during a write as that write ends | `what filled the buffer while a write ran goes as the write ends, not at the next second` in `sdk/js/test/logger.test.ts` |
| a Bun call under an aborted signal is refused      | `every call under an aborted signal is refused` in `sdk/js/test/records.test.ts` |
| a Bun duration or rate misspelled does not compile | `a duration misspelled does not compile, and text from elsewhere is checked at its call` in `sdk/js/test/kv.test.ts`, `refuses a rate it cannot read, its type before its call` in `sdk/js/test/config.test.ts` |
| a Bun store's close frees its directory            | `close returns once the child has exited` in `sdk/js/test/kv.test.ts`            |
| the JS SDK runs under Node: its sidecar, a private child, TCP and TLS checked | `a remote server is reached over TLS, its certificate checked, under Node` and the rest of `sdk/js/test/under-node.ts`, run by `node --test` |
| a half-full log buffer is written before its interval | `TestAHalfFullBufferFlushesBeforeItsInterval`; `test_a_half_full_buffer_is_written_before_its_interval` in Python's |
| writing a log does not log again                    | `TestTheEnginesOwnLinesAreRefused`                                              |
| a logger's console line is the same bytes in Go, Bun and Python | `TestConsoleLinesAreTheVectors`, over `records/testdata/console.json`, which both SDKs' suites read |
| a line reaches its console as it is logged, whole   | `TestEachLineReachesTheConsoleAsItIsLogged`, `TestConsoleLinesFromManyGoroutinesDoNotInterleave` |
| the engine's own lines reach the console from Info up, never the store | `TestTheEnginesOwnLinesReachTheConsoleAndNotTheStore`         |
| a redacted field is kept nowhere                    | `TestARedactedFieldIsHiddenInTheStoreAndOnTheConsole`, and in both SDKs' suites |
| a logger keeps its lines from its level up          | `TestALevelKeepsALoggersLinesFromItUp`                                          |
| a Python line takes the fields of the context it was logged in | `test_lines_take_the_fields_of_the_context_they_were_logged_in` in Python's suite |
| a logger without a store writes the console alone   | `TestAHandlerWithoutAStoreWritesTheConsoleAlone`                                |
| colours need a terminal, and not NO_COLOR           | `TestAFileIsNoTerminal`, `TestNoColorAndADumbTerminalTurnColoursOff`            |
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
| a records scan since a span pages on in that range  | `TestAScanSinceStartsThatLongBeforeNowAndPagesOn`, the clock moved between pages |
| `All` walks every record a page at a time           | `TestAllWalksEveryRecordAPageAtATime`, a walk stopped early reads no further     |
| a search finds a record by its text, the case ignored | `TestASearchFindsARecordByItsTextItsCaseIgnored`, sealed and in its head        |
| a record takes the trace of its context              | `TestARecordTakesTheTraceOfItsContext`, the caller's records left unchanged     |
| blooms and level masks skip blocks                  | `TestBloomsAndLevelMasksSkipBlocks`                                             |
| a read walks the time index near its range only     | `TestTheTimeIndexIsWalkedWithinEachSpan`, on the plan SQLite chooses            |
| a read finds records in blocks of every width       | `TestReadsFindRecordsInBlocksOfEveryWidth`                                      |
| retention removes whole segments, clips reads       | `TestRetentionRemovesWholeSegmentsAndClipsReads`                                |
| records work holds the store's memory               | `TestStoreMemoryBoundsAppendReadSealAndFollow`                                  |
| a follower is told what retention removed           | `TestFollowCountsWhatRetentionRemovedFirst`                                     |
| a snapshot does not stop the writer                 | `TestSnapshotCopiesWhileTheWriterWrites`                                        |
| a backup restores every engine                      | `TestABackupRestoresEveryEngine`                                                |
| a changed backup is refused and leaves nothing      | `TestAChangedByteIsRefusedAndLeavesNothing`                                     |
| a database no one opened is copied without opening it | `TestCopyTakesADatabaseNoOneOpened` in `sqldb`                                |
| a backup over the wire holds every engine on disk, and makes none that is not | `TestABackupOverTheWireHoldsEveryEngineOnDisk`; `TestBackupWritesAZipThatRestoreTakesBack` in `cmd/tinystore`; `a backup is one zip of every engine, which restore takes back into an empty directory` in `sdk/js/test/backup.test.ts`, `test_a_backup_is_one_zip_of_every_engine_which_restore_takes_back` in Python's |
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
| a decode stays bounded                              | `TestADecodeStaysWithinTheBodysCeiling`, against the 8 KiB ceiling              |
| an iterator outlives its codec                      | `TestIteratorOwnsItsBytesAndOutlivesTheCodec`                                   |
| unordered or oversized input is refused             | `TestRejectsUnorderedAndOversizedInput`                                         |
| a counter's increase survives a reset               | `TestCounterSummaryIncludesResets`, `TestAggregateCounterIncludesBlockTransitionButNotBucketTransition` |
| only the safe prefix is sealed                      | `TestWatermarkIsStrictAndFollowsTheSeries`, on the strict edge                  |
| a late sample cannot enter a sealed block           | `TestHeadSealingReopenAndPartialRetention`, `ErrTooOld` behind the frontier     |
| a partial range is not answered from a summary      | `TestWholeSummarySkipsPayloadButPartialBlocksCheckIt`: a cut block decodes raw  |
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
| a limit names which, what the call wanted and the bound | `TestALimitErrorNamesItsBoundAndIsItsKind`, `TestWholeExactBlocksNeedNoDecodedSampleBudget` |
| a metrics plan is what the call then spends            | `TestAPlanSaysWhatTheCallThenSpends`, no payload fetched                         |
| a series is its name and its labels, `__` the store's  | `TestASeriesIsItsNameAndItsLabels`, `TestIngestIsAtomicAndLastMutableValueWins` |
| a range over the last Since starts that long before now | `TestARangeSinceStartsThatLongBeforeNow`, a To of zero the open end            |
| a condition finds series beyond equality, NoneOf alone refused | `TestConditionsFindSeriesBeyondEquality`, `TestAConditionThatFindsNothingOrCannotBeTakenIsRefused`, `TestConditionsOverTheWire` |
| a series that cannot be repaired can still be dropped  | `TestDropSeriesRemovesAnUnreadableSuspendedSeries` and `TestDropSeriesKeepsItsNeighbours` |
| long-head append preserves bits and frontier           | `TestLongPackedHeadAppendKeepsExactBitsAndFrontier`                   |
| exact aggregates cross blocks, resets and retention    | `TestAggregateRoundsExactSumAcrossSealedBlocks`, `TestAggregateCounterIncludesBlockTransitionButNotBucketTransition` and `TestAggregateClipsRetentionBeforeSummingSealedEdges` |
| a whole block's summary answers as its samples do      | `TestSummaryAndRawAggregatesAgreeAtEveryBoundary`, every operation, kind and boundary |
| a group joins its series exactly and rounds once       | `TestAGroupJoinsItsSeriesExactlyAndRoundsOnce`, `TestRateAndDeltaAreExactPerSeriesThenJoined` |
| a whole-block summary spends no decoded-sample budget  | `TestWholeExactBlocksNeedNoDecodedSampleBudget`                                 |
| a malformed exact summary is refused                   | `TestExactSummaryEncodingRefusesNoncanonicalOrUnboundedFields`, `FuzzExactSummary` |
| exact summaries keep a directory within its bound      | `TestLargeExactSummariesStayWithinDirectoryBounds`                              |
| a directory written once still reads                   | `TestDirectoriesWrittenBeforeStillRead`, inline and external                    |
| writes queued for the writer share a commit, fail alone | `TestGroupedWritesShareACommitAndFailAlone`                                    |
| a commit gathers the writers its last one answered     | `TestAGroupGathersTheWritesItsLastBatchAnswered`                               |
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
| a config is its defaults, its environment, then what was kept, across a restart | `TestAConfigIsItsDefaultsThenItsEnvironmentThenWhatWasKept`, in both SDKs' suites too |
| a config change reaches every handle and watcher at once | `TestAChangeIsSeenByEveryHandleAtOnce`, `TestAConfigChangeReachesEveryWatcher` over the wire |
| a config change that fails its check, or sets a secret, keeps nothing | `TestAChangeThatFailsItsCheckOrSetsASecretKeepsNothing`                    |
| a kept value that no longer fits its field is left out and named | `TestAKeptValueThatNoLongerFitsIsLeftOutAndNamed`                       |
| a variable is read by its field's type, or refused naming it | `TestAVariableIsReadByItsFieldsType`                                       |
| a config keeps only JSON within its bounds             | `TestARawConfigKeepsOnlyJSONWithinItsBounds`                                    |
| variables and `.env` files read alike in every language | `TestVariablesAndDotenvFilesAreTheVectors`, over `kv/testdata/config.json`, which both SDKs' suites read |
| a watch ends with its client's side                    | `TestAWatchEndsWithItsClientsSide`                                              |
| a limiter lets its burst through, then its rate        | `TestALimiterLetsABurstThroughThenItsRate`, `TestAllowNTakesAllOrNoneAndNeverPastTheBurst`, `TestALimiterOverTheWire` |
| a limiter's times outlive a reopen, a quiet key is forgotten | `TestALimiterKeepsItsTimesAcrossAReopen`, `TestAQuietKeyIsForgottenOnceItsTimeHasCome` |
| requests racing for a key pass no more than the burst  | `TestRequestsRacingForAKeyPassNoMoreThanTheBurst`                               |
| a quota counts a use in every window or in none, racing uses included | `TestAQuotaCountsInEveryWindowOrInNone`, `TestUsesRacingForAKeyPassNoMoreThanItsLimit`, `TestAQuotaOverTheWire`, `a use counts in every window or in none, and a refund gives it back` in `sdk/js/test/kv.test.ts`, `test_a_quota_counts_a_use_in_every_window_or_in_none` in Python's |
| a quota's window starts at a key's first use after the last ended | `TestAWindowStartsAtTheFirstUseAfterTheLastEnded` |
| a quota's `Get` counts nothing, a refund never goes below nothing, its windows outlive a reopen | `TestGetRefundAndDeleteChangeWhatTheySay` |
| a quota that cannot count is refused at open | `TestAQuotaThatCannotCountIsRefused` |
| a once key's function runs once and its answer is kept | `TestARunKeepsItsAnswerAndRunsAKeyOnce`, `TestAnErrorKeepsNothingAndTheNextRunRunsAgain` |
| a run of a key waits for the one running it, every client's | `TestARunWaitsForTheRunOfItsKey`, `TestAWaitingRunEndsWithItsContextAndAnAnswerOutlivesIt`, `TestAOnceRunsAKeyOnceOverTheWire`, `a key runs once: a call meanwhile waits for its answer, and a throw keeps nothing` in `sdk/js/test/kv.test.ts`, `test_a_once_key_runs_once_and_a_call_meanwhile_waits_for_its_answer` in Python's |
| an Enqueue that returned survives an abrupt exit       | `TestAnEnqueuedJobSurvivesAnAbruptExit`, from many goroutines at once           |
| a job runs at its time and not before                  | `TestAJobRunsAtItsTimeAndNotBefore`                                             |
| a write during a Work loop's read is not lost, nor keeps it awake | `TestAWriteDuringAnAlarmReadCannotBeLost`, `TestALaterWriteDuringAnAlarmReadLetsTheLoopSleep` |
| a jobs Scan finds exactly the keys under its prefix    | `TestScanFindsOnlyTheKeysUnderAPrefixNoRuneEnds`                                |
| a key names one job, and enqueuing it only brings it forward | `TestAKeyNamesOneJobAndARepeatOnlyBringsItForward`                        |
| an enqueue while its job runs asks for one run more    | `TestAnEnqueueWhileItsJobRunsAsksForOneRunMore`                                 |
| `KeepDone` makes a key run once                        | `TestKeepDoneMakesAKeyRunOnce`                                                  |
| `Update` changes only a job that still waits           | `TestUpdateChangesOnlyAWaitingJob`                                              |
| `Cancel` says whether there was a job, and stops a running one's handler | `TestCancelSaysWhetherItCameInTime`, `TestCancelStopsTheHandlerOfARunningJob` |
| a job claimed ahead for a busy worker waits, and a cancel keeps it from starting | `TestAJobHeldForABusyWorkerWaitsAndCancelKeepsItFromStarting` |
| `Get` says where a job is and how many jobs run before it | `TestGetSaysWhereAJobIsAndHowManyRunBeforeIt`, `TestAJobWhoseLeaseEndedWaitsAgain` |
| a watch follows its job to its end                     | `TestAWatchFollowsItsJobToItsEnd`                                               |
| a progress past 4 KiB is dropped in Go, refused by the SDKs | `TestAProgressPastItsBoundIsDropped`, `a progress JSON cannot write, or past 4 KiB, is refused` in `sdk/js/test/jobs.test.ts` |
| `MaxRunning` holds a queue to its places across loops and claims | `TestMaxRunningHoldsAQueueToItsPlaces`                                |
| a job keeps its last run, over the wire too, and a run given back records none | `TestAJobKeepsItsLastRun`, `TestAJobsLastRunOverTheWire`, `a job keeps its last run: when it began and how long it took` in `sdk/js/test/jobs.test.ts`, `test_a_job_keeps_its_last_run` in Python's |
| a remote worker's job is watched, and its handler told of a cancel | `TestAJobWatchFollowsARemoteWorkersJob`, `a watch follows a job up its queue, through its progress, to a cancel its handler sees` in `sdk/js/test/jobs.test.ts`, `test_a_watch_follows_a_job_through_its_progress_to_a_cancel_its_handler_sees` in Python's |
| a job whose lease ended runs again                     | `TestAJobWhoseLeaseEndedRunsAgain`                                              |
| a step runs once in a run, across its attempts and a worker that died, and a lost lease keeps none | `TestAStepRunsOnceAcrossTheAttemptsOfARun`, `TestAStepOfALostLeaseKeepsNothing` |
| a run that ends takes its steps along, and a repeat's next run starts without them | `TestStepsGoWithTheirRun`, `TestAStepsNameAndAnswerAreBounded` |
| a remote worker's steps are kept across a run's attempts, claimed or on a work stream | `TestAJobsStepsOverTheWire`, `a step runs once in a run: the attempt after a failure gets its kept answer` in `sdk/js/test/jobs.test.ts`, `test_a_step_runs_once_in_a_run_the_attempt_after_a_failure_gets_its_kept_answer` in Python's |
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
| a request is understood whole or refused, naming the field and the server's version | `TestARequestWithAFieldTheServerDoesNotKnowIsRefused`, `TestAMessageReadsWhatItKnowsAndNamesWhatItDoesNot` |
| a client newer than its server speaks the server's protocol | `TestAClientOfANewerProtocolIsWelcomedInTheServers`                    |
| the vectors are the bytes                              | `TestVectors`, `TestFrameVectors`, `TestTheExamplesAreWhatTheMessagesWrite`     |
| a byte the Bun encoder writes as its buffer grows is kept | `a byte written as the buffer grows is kept` in `sdk/js/test/wire.test.ts`    |
| every message is a vector, every field by its name     | `TestMessageVectors`: `messages.json` is what the Go types write, each schema field in one |
| the largest kv or jobs value travels in one body       | `TestTheLargestValueTravelsInOneBody`                                           |
| a work stream asked to end when idle ends              | `TestAWorkStreamUntilIdleEndsOnceNoJobIsDue`                                    |
| an extend on a work stream is refused, not an ack     | `TestAnExtendOnAWorkStreamIsRefused`                                            |
| a sidecar started in the background says why it ended | `TestServeLogsToTheFileItIsGivenWhyItEnded` in `cmd/tinystore`                  |
| a client past its credit is cut off, the reader never waits | `TestAClientPastItsCreditIsCutOff`, `TestFramesThatBreakTheProtocolEndTheConnection` |
| every stream ends with one final frame                 | `TestEveryStreamEndsOnce`, answered, failed, panicked, cancelled, silent, down and up |
| answers queued during a write leave in the next        | `TestQueuedAnswersShareAWrite` in `server/internal/flow`                        |
| a connection grant wakes every sender whose body fits | `TestAGrantWakesEverySenderWhoseBodyFits` in `server/internal/flow`             |
| a Go test client's upload stops with its stream or connection | `TestAFinalResponseStopsAnUploadWaitingForCredit`, `TestALostConnectionStopsAnUploadWaitingForCredit` in `server/internal/client` |
| a point read cancelled while it waits lets its connection go on | `TestACancelledPointReadLetsTheConnectionGoOn`                           |
| a stream's number is free when its final frame arrives | `TestAStreamNumberIsFreeWhenItsFinalFrameArrives`                               |
| a closing server lets the streams running finish       | `TestClosingTheServerLetsTheStreamsRunningFinish`, `TestARequestThatCrossesTheGoAwayIsAnsweredUnavailable` |
| a remote connection needs its token                    | `TestARemoteConnectionNeedsItsToken`                                            |
| a pipe's name has one owner                            | `TestAPipesNameHasOneOwner`, on Windows                                         |
| a kv batch is one transaction                          | `TestAKVBatchRollsBackWhenOneOfItsCallsFails`                                   |
| a read that names what it read fails once its key changed, in a batch too | `TestAReadThatNamesWhatItReadFailsOnceTheKeyChanged`                    |
| an SDK's tx reads, decides and writes, running again when a key it read changed, five times at most | `transactions` in `sdk/js/test/kv.test.ts`, `test_a_tx_reads_decides_and_writes_and_runs_again_when_a_key_it_read_changed` and `test_a_tx_reads_its_own_writes_and_gives_up_on_a_key_that_keeps_changing` in Python's |
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
| a program shares its own store in one call, until stop | `TestShareServesTheProgramsStoreUntilItStops`                                  |
| an engine a program opened and did not pass says to pass it | `TestAnEngineTheProgramOpenedAndDidNotPassSaysToPassIt`                   |
| a long store's socket moves to the user's own directory | `TestALongSocketPathMovesToTheUsersOwnDirectory`, off Windows                  |
| a server proves it read `SERVE` to a local challenge only | `TestAServerProvesItselfOnlyToALocalChallenge`, `TestProofVectors`          |
| an endpoint taken after its server left cannot prove itself | `TestAnEndpointTakenAfterItsServerLeftCannotProveItself`                    |
| of two sidecars started at once, one exits held        | `TestAStaleServeStartsOneSidecar`, `TestASecondServeOfADirectoryExitsHeld`      |
| a sidecar leaves once idle, with its `SERVE` and lock  | `TestTheSidecarIsFoundThroughServeAndLeavesWhenIdle`                            |
| a private child leaves when told, though its parent stays | `TestAPrivateChildLeavesWhenToldThoughItsParentStays`, `TestAPrivateChildServesItsParent` |
| a test's clock moves only forward, an admin's alone, and the store runs on it | `TestATestsClockMovesOnlyForwardAndTheStoreRunsOnIt`, `TestAPrivateChildRunsOnTheClockItIsGiven` in `cmd/tinystore`; `a private store runs on the clock it is given, which moves only forward` in `sdk/js/test/clock.test.ts`, `test_a_private_store_runs_on_the_clock_it_is_given_which_moves_only_forward` in Python's |
| what serve cannot serve opens nothing                  | `TestServeRefusesWhatItCannotServe`, a file it cannot read included             |
| the tool requires only the store and the server        | `TestTheToolRequiresOnlyTheStoreAndTheServer`                                   |
| status reads a directory beside its server, for a person and with `--json`, never printing SERVE's secret | `TestStatusReadsADirectoryAndKeepsTheSecret` in `cmd/tinystore`  |
| the command alone lists its commands, a directory before or after the flags | `TestHelpListsTheCommandsAndIsNoError`, `TestADirectoryComesBeforeOrAfterTheFlags` in `cmd/tinystore` |
| `serve <dir>` serves the directory until Ctrl+C, never idle | `TestServeOfADirectoryServesItUntilCtrlC` |
| a server stops at an admin's request, and only when its program said how | `TestAServerStopsAtAnAdminsRequestOnly`, `TestStopEndsTheServerOfADirectory` in `cmd/tinystore` |
| an SDK replaces a sidecar of an older release than its own, its clients moving to the new one; another server it only tells of | `a sidecar of an older release is stopped, and every client moves to the one started in its place` and `a server of an older release than its SDK is told apart, and only one` in `sdk/js/test/connection.test.ts`, `test_a_sidecar_of_an_older_release_is_stopped_and_every_client_moves_to_the_one_started_in_its_place` in Python's |
| SERVE calls a sidecar one, and neither a person's server nor a program's | `TestTheSidecarIsFoundThroughServeAndLeavesWhenIdle`, `TestServeOfADirectoryServesItUntilCtrlC` in `cmd/tinystore`, `TestServeIsWrittenWholeUnderTheLock` |
| a read through the tool makes no store of a directory | `TestLogsOfADirectoryWithoutAStoreMakeNone` |
| `logs -f` prints a record sent a little late, each once and two alike twice | `TestLogsPrintTheLastRecordsAndFollowTheNext` |
| the tool prints a record as a logger's console does | `TestAPrinterWritesARecordAsTheConsoleDoes` in `records` |
| an agent over MCP writes nothing, nor makes an engine's file | `TestAnAgentReadsTheStoreOverMCP`, `TestAnAgentMakesNoEnginesFile` |
| the SDK packages install the `tinystore` command, the binary's output and exit code its own | `the tinystore command runs the binary, with its output and its exit code` in `sdk/js/test/command.test.ts`, under Bun and Node, `test_the_tinystore_command_runs_the_binary_its_output_and_exit_code_the_binarys` in Python's |
| a link in the docs that leads nowhere fails the site's build | `that leads nowhere is a problem` in `web/src/lib/content/links.test.ts`, and the prerender `task web` runs |
| a heading keeps the anchor GitHub gives it | `gives headings the ids GitHub gives them, a repeated one numbered` in `web/src/lib/content/outline.test.ts` |
| fences in three languages are one block, untitled ones of a language two | `a run in three languages is one block`, `two untitled fences of one language stay two blocks` in `web/src/lib/content/code.test.ts` |
| the landing page's numbers are the README's SVG cards, read back | `read back from every SVG the README shows, in its order` in `web/src/lib/content/landing.test.ts` |
| the landing page's words are `web/landing.md`'s, a part missing or a link nowhere failing the build | `takes every word from web/landing.md, its links resolved as a page's`, `fails the build when a part is missing or a link leads nowhere` in `web/src/lib/content/landing.test.ts` |
| the site's views and events live in its own TinyStore, through a restart | `keeps a view and an event as records and counts both, through a restart` in `web/server/analytics.test.ts` |
| a page, its data and its markdown are views; an asset, a HEAD and a 404 are not | `is served and counted, as are its data and its markdown; an asset is not counted` in `web/server/http.test.ts` |

`task check` runs exactly what CI gates on. When those two drift, the local one
is the weaker of the pair and a failure arrives after a push instead of before
it.

## Weight

This exists because the alternatives cost more memory than what they watch.

**No cgo, ever.** `CGO_ENABLED=0` in the CI build on all three platforms, so a
dependency needing a C toolchain fails on the pull request that introduces it,
and in `task size`, whose report refuses a probe built with cgo. This is why
the file is `ncruces/go-sqlite3`, SQLite translated to Go; do not swap it for a
faster cgo driver. The store reaches it through a database/sql driver of its
own in `internal/sqlite`, never ncruces' `driver` package, which registers
`sqlite3` as it loads, as mattn's does, so that a program linking both panics.
The race detector is the one exception to no cgo — it builds test binaries,
never a product.

**A dependency is a decision, and here it is somebody else's decision too.**
Whatever the root module requires, every program importing this links. Check
what a module drags in, prefer the standard library, and put anything a
measurement needs in a module of its own.

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
  parameters become a named value. Research's `rewrite.md` shows it on real
  code.
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
One version covers the Go modules, the binary, npm, PyPI and the server image.
A release is one run of the Release workflow, which tags and publishes and
never writes `main`; nothing is tagged by hand. The `release` skill has how,
and why each step is safe.

The payload format's version and the schema's version are not the module's. A
release may leave both where they are, and either may move without a release.

## Research rounds

A round is prototype code and one dated report in
[tinyshed/research](https://github.com/tinyshed/research), under
`tinystore/`, whose own AGENTS.md says how a round is run and written.
Neither is a product, and neither may be quoted as one.

**A round measures a commit of this repository, which is never rewritten.**
Research keeps this repository as a submodule pinned to the commit a round
measured, and its prototypes are modules under `github.com/tinyshed/tinystore/`
so that Go lets them import `internal/`. A report names that commit, so a
rewrite of `main` that moves it silently unmakes every measurement standing
on it: check what the reports cite before touching history. A round measured
on a branch lands its finding on `main` before its report does.

**A corpus is fetched, never committed**, here or in research.

**What a round proved is worth building is its own commit here**, with its
own type: the evidence and the feature are read by different people.

## Build and checks

```sh
task test             # go test -shuffle=on, which also runs the default vet analyzers
task lint             # golangci-lint
task fmt              # gofumpt + goimports
task fix              # go fix modernizers
task vuln             # govulncheck
task size             # what importing this costs a binary
task check            # everything CI gates on
task tidy             # every module file
task lint:platforms   # golangci-lint as Linux, macOS and Windows build the code
task sdk              # both SDKs' checks and suites, against a tinystore built here
task web              # the docs site's checks, tests and build
task web:dev          # the docs site, reloading as docs/ changes
task race:linux       # the race detector in a Linux container, for a host without cgo
task sdk:linux        # both SDKs' suites in a Linux container
```

Install Task with `go -C tools install github.com/go-task/task/v3/cmd/task`;
`task setup` then builds the rest into `./bin`.

Measurements beside the engines are skipped unless `TINYSTORE_SPIKE=1`, so
`task check` and CI do not run them. Heavy ones belong in a linux container, or
their numbers cannot sit beside the others in research's `measurements.md`:

```sh
docker run --rm -v <repo>:/src -w /src -e TINYSTORE_SPIKE=1 golang:1.27 \
  go test ./records -run <name> -v -count=1
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
