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

|                            | What it is                                                                                                                                                                                                      | Contract                                                                  | Design                                           |
|----------------------------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|---------------------------------------------------------------------------|--------------------------------------------------|
| root                       | the directory and its lifecycle: `Open` under the lock, `Close`, `Claim`, `Attach`, `Logger`, `Now`, `Every`, the memory budget, `Snapshot`, `Dir`                                                              | [doc.go](doc.go)                                                          | [architecture.md][design-architecture]           |
| `codec/`                   | 1..240 ordered samples to a checked body, every bit kept                                                                                                                                                        | [format.md][design-format]                                                | [metrics.md][design-metrics]                     |
| `metrics/`                 | samples, exact reads, streams and aggregates, sealing, retention, instruments                                                                                                                                   | [README](metrics/README.md)                                               | [metrics.md][design-metrics]                     |
| `records/`                 | logs and events, a logger of the console and the store, another program's lines, paged reads, a follow cursor                                                                                                   | [README](records/README.md)                                               | [records.md][design-records]                     |
| `sqldb/`                   | the application's SQL databases, tables from structs, checked migrations                                                                                                                                        | [README](sqldb/README.md)                                                 | [sqldb.md][design-sqldb]                         |
| `kv/`                      | buckets, counters, branches, expiry, versions, configs hot in every process, a GCRA limiter, quotas of several windows, answers kept once a key                                                                 | [README](kv/README.md)                                                    | [kv.md][design-kv]                               |
| `jobs/`                    | queues ordered by time, leases, retries, repeats, a Work loop, where a job is and a watch to its end, its last run, a bound on those running, steps a run keeps across its attempts                             | [README](jobs/README.md)                                                  | [jobs.md][design-jobs]                           |
| `blobs/`                   | objects by path, inline or a file each, checked whole reads, a scrub                                                                                                                                            | [README](blobs/README.md)                                                 | [blobs.md][design-blobs]                         |
| `backup/`                  | a snapshot as one checked zip, restored before `Open`                                                                                                                                                           | [backup.go](backup/backup.go)                                             | [architecture.md][design-architecture]           |
| `server/`, `cmd/tinystore` | every engine over one protocol: a sidecar, a private child, a remote server with TLS and tokens, a Go program's own store shared in one call; a person's `status`, `logs` and `serve`, and an agent's MCP tools | [wire.md](docs/wire.md)                                                   | [server.md][design-server]                       |
| `sdk/js`, `sdk/python`     | the Bun and Node client and the Python one, tested against every vector and a real `tinystore serve` (`task sdk`)                                                                                               | [Bun and Node](docs/reference/bun.md), [Python](docs/reference/python.md) | [sdk.md][design-sdk], [server.md][design-server] |
| `web/`                     | the docs as a site: every page prerendered from `docs/`, served by Bun into a TinyStore of its own, its views and readers' events kept there (`task web`)                                                       | [web/AGENTS.md](web/AGENTS.md)                                            | the `docs` skill                                 |

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

Not built: the SDKs' examples; `v0.1.0` itself, after its release
candidates; the guides `docs/README.md`
lists without a link.

Only release candidates are released, `v0.1.0-rc.N`, and they promise nothing
about files: until `v0.1.0`, no database written by an earlier revision has to
be read. Readers for earlier formats are deleted, not kept, and an engine's
schema changes inside its `0001` migration: a second one is added only after
`v0.1.0`.

Do not describe unbuilt behaviour as though it works.

## Shape

| Path                          | What it is                                                                                                                                   |
|-------------------------------|----------------------------------------------------------------------------------------------------------------------------------------------|
| `codec/`                      | the block codec and the payload format. Knows samples and bytes, nothing else                                                                |
| `metrics/`                    | the metrics API and its registry, head, groups, query and retention                                                                          |
| `sqldb/`                      | the application's SQL databases: tables from structs, checked migrations, typed reads and writes                                             |
| `records/`                    | logs and events: a head, event-time segments, paged reads, a follow cursor                                                                   |
| `records/console/`            | the logger without the store: a slog handler, its console lines, redaction and environment; links no SQLite                                  |
| `kv/`                         | the application's current state: typed buckets, branches, expiry, versions                                                                   |
| `jobs/`                       | work that runs at its time: queues ordered by time, leases, retries, repeats                                                                 |
| `blobs/`                      | the application's files: objects by path, inline or a file each, checked reads                                                               |
| `backup/`                     | every engine's file in one checked zip, and its restore before `Open`                                                                        |
| `internal/sqlite/`            | file handles, read/write transactions and checked migrations                                                                                 |
| `internal/admission/`         | an engine's open gate and the slots that bound its concurrent work                                                                           |
| `internal/dirlock/`           | the directory's `LOCK`, one store a directory, per platform                                                                                  |
| `internal/term/`              | whether a file is a terminal that shows colours, for a logger's console lines                                                                |
| `internal/linkaudit/`         | the canary `task size` looks for: methods the linker drops unless method pruning is off                                                      |
| `sqldb/fts5/`, `sqldb/rtree/` | SQLite's virtual table modules, each linked by its import                                                                                    |
| `internal/dbstat/`            | a closed file's pages divided among its tables and indexes, for measurements                                                                 |
| `internal/release/`           | what a release makes: its tags, binaries, archives, npm packages, wheels, notes                                                              |
| `tools/`                      | a second module pinning developer tools. Two files, never hand-edited                                                                        |
| `server/`                     | a module of its own: the store served to other processes, sessions, listeners, handlers                                                      |
| `server/wire/`                | the protocol's bytes: frames, the MessagePack profile, messages, codes, vectors                                                              |
| `server/reach/`               | the Go client of a directory's server, found through `SERVE` and proven, for the tool                                                        |
| `cmd/tinystore/`              | the one executable, a module of its own: `serve`, `stop`, `status`, `logs`, `mcp`, `backup`, `restore`, and `migrate` and `schema` for sqldb |
| `sdk/js/`, `sdk/python/`      | the clients of `tinystore serve` for Bun and Node, and Python; `sdk/go.mod` keeps them out of the Go module                                  |
| `docs/`                       | the guides, one page per feature in Go, Bun and Python, and the wire protocol; `README.md` is their table of contents and the site's sidebar |
| `web/`                        | the docs site: SvelteKit prerendering `docs/`, and its Bun server under `server/`; `web/go.mod` keeps it out of the Go module                |
| `examples/`                   | programs using the public API, built and tested with the module                                                                              |
| `.github/workflows/`          | the authoritative clean builds                                                                                                               |
| `.agents/skills/`             | how the recurring work is done; `.claude/skills/` points to it                                                                               |

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
`TestWireImportsOnlyTheStandardLibrary`. On main both reach what they require
through a `replace`, which a release drops in the commits only its tags reach. `task` tests, lints, formats and tidies both
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

**A guest opens beside the owner, and only SQL.** `Options.Guest` opens a
directory another process holds, without its `LOCK`, as a recovery command
beside a running server does: nothing runs in the background, `Claim`
refuses every engine's file but `sql/`, jobs refuse to open `In` a guest's
database, and a guest's database applies no migration and its connections'
authorizer refuses DDL and writes to `_tinystore_…`, since the owner checked
the schema and keeps those tables' state in memory. SQLite's own locks share
the writer between the two processes. A guest's backup copies the files it did
not open by a read snapshot, and refuses a store with blobs.

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
cannot wait out. Every pragma is per connection, so each travels in the
connection's URL and each module registers in `Connected`: a reader opened
again is the same reader. That is what lets a reader beyond one close after a
minute unused, on a timer `Close` stops and waits for, since a burst of reads
otherwise keeps half a MiB a reader for good. database/sql's own pool keeps
no idle connection, so nothing else expires one.

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

|                                                                               |                                                                                                                                                                            |
|-------------------------------------------------------------------------------|----------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| [docs/README.md](docs/README.md)                                              | the guides: what each engine does for its user, in Go, Bun and Python                                                                                                      |
| [GATES.md](GATES.md)                                                          | every promise and the test that fails when it breaks, by engine                                                                                                            |
| each package's README                                                         | what it promises as built: its contracts, bounds and errors                                                                                                                |
| [metrics/README.md](metrics/README.md)                                        | the implemented metrics API, invariants and a runnable example                                                                                                             |
| [docs/wire.md](docs/wire.md)                                                  | the wire protocol's bytes: frames, credit, MessagePack, errors                                                                                                             |
| [web/AGENTS.md](web/AGENTS.md)                                                | the docs site: how a file becomes a page, the server, its analytics                                                                                                        |
| research's [tinystore/design][design]                                         | why each engine has its shape, as designed: the runtime, metrics and [its exact aggregates][design-aggregates], the format, every engine, the server, the SDKs' vocabulary |
| [tinyshed/research](https://github.com/tinyshed/research/tree/main/tinystore) | the rounds, every number, the prototypes and the open questions                                                                                                            |

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
[GATES.md](GATES.md) lists each promise, by engine, beside the test that fails
when it breaks; a change that makes a promise adds its line there.

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
would be charging to somebody else's binary. `internal/linkaudit` is the
canary: `task size` fails when the probe still holds a method nothing calls.

**A program pays only for what it links.** SQLite's FTS5 and R*Tree are
imports of their own, `sqldb/fts5` and `sqldb/rtree`, and the logger without
the store is `records/console`, which links neither SQLite nor zstd. `task
size` reports each beside the whole probe.

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
- **A markdown table is aligned**, its cells padded so that the pipes line up
  as an IDE formats one. `task tables` aligns them, and `task web` fails on
  one that is not.
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
task readme           # the README's headline, pitch, sample and engines, from web/landing.md
task tables           # every markdown table aligned, as an IDE formats one
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
