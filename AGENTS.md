# TinyStore

An embedded time-series store for Go, on SQLite: exact samples, bounded memory,
one file, no daemon. It is a library, so the only thing a consumer sees is a
handle and the promises this file makes about it. `tinyshed/dashbin` is the
first caller and not the owner.

This file is the contract for anyone — human or agent — changing the repository.
Keep it short and factual, and update it when a decision moves. Nothing here
should be a fact a ten-second grep would answer.

## Status

`codec/` is built, tested and measured: 1..240 ordered samples in, a head and a
bounded body out, every IEEE-754 bit preserved, five value representations, a
checksum over both halves, an iterator that owns its bytes and a fuzzed decoder
that refuses corruption. It has no opinion about when a block is sealed or how
long one is kept.

`metrics/` has the first durable slice: a registry with postings, an exact packed
head, atomic bounded ingestion, raw range reads from one snapshot, version-checked
sealing, bounded retention and reopen. `internal/sqlite/` owns file mechanics.
`metrics/README.md` documents the API and limits. Groups now share clocks and
use constant/change/grid candidates, compact exact summaries and inline or
separate payloads. Aggregate queries, sealed-group merging and steady-state
performance remain unfinished. Prototype density figures are not engine guarantees.
`spike/` preserves the experiments behind those decisions.

The mutable **head** is a bounded packed tail in `series_state`, rewritten once
per touched series inside a transaction. Each encoded chunk has at most 240
samples; the whole head has separate byte and sample budgets. Legacy head rows
remain readable and convert on mutation. Packing is physical compression only:
it does not move the sealed frontier or reject a previously admissible timestamp.
Identity indexes store a digest; the original canonical labels are checked in
full before a digest match may resolve a series. A series row holds its labels
as gap-coded dictionary ids rather than text, so that check compares ids.

Do not describe unbuilt behaviour as though it works.

## Shape

| Path                 | What it is                                                                    |
|----------------------|-------------------------------------------------------------------------------|
| `codec/`             | the block codec and the payload format. Knows samples and bytes, nothing else |
| `metrics/`           | the metrics API and its registry, head, groups, query and retention           |
| `internal/sqlite/`   | file handles, read/write transactions and checked migrations                  |
| `spike/`             | prototypes and measurements, skipped unless `TINYSTORE_SPIKE=1`               |
| `tools/`             | a second module pinning developer tools. Two files, never hand-edited         |
| `docs/`              | the design, the format, the numbers, the open questions; `reports/` the rounds |
| `.github/workflows/` | the authoritative clean builds                                                |

The first engine keeps its implementation in one package; split it only when
a dependency boundary needs a package, not to mirror the execution steps:

```text
metrics/            public API and private implementation files
internal/sqlite/    mechanics shared by future engines; no metric vocabulary
bench/              a module of its own: corpora, and other engines to measure against
```

Production gates belong beside their implementation. Keep historical spikes
until a real-engine harness can reproduce what they measured. Do not copy their
test-only parsers or call their helpers from production code. Records, KV and a
SQL mapper get no placeholder packages and are not Metrics dependencies.

## Modules

Three, and the split is the point.

```text
root     what a caller links: the engine and nothing else
tools/   golangci-lint, govulncheck, task
bench/   corpora, comparison harnesses, whatever a measurement drags in
```

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

## Architecture

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

**Nothing scans every series.** Postings are in the first version rather than
in an optimisation after it, and finding due work is an index over one value
per series rather than one per sample. A million active series without a
redesign is what the format and the query model are built for; what an
installation actually permits is a configured limit, and the two are not the
same promise. Cardinality and ingest rate are promised separately.

**The format carries its own version, and a released one never moves.** Codec
bodies, group directories and schema histories have their own versions and
golden readers. A module version is not a substitute for any of them.

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
| [docs/design.md](docs/design.md)             | how the store is meant to work, and why that shape               |
| [metrics/README.md](metrics/README.md)       | the implemented metrics API, invariants and a runnable example   |
| [docs/reports/implementation-2026-09-21.md](docs/reports/implementation-2026-09-21.md) | the first slice and its measured limits |
| [docs/format.md](docs/format.md)             | the bytes: the payload's layout, version by version              |
| [docs/measurements.md](docs/measurements.md) | every number, its environment and how to reproduce it            |
| [docs/research.md](docs/research.md)         | what is not built: the open questions and their acceptance gates |
| [docs/reports/](docs/reports/README.md)      | the dated rounds every number above came from                    |

A reference a contributor returns to belongs in `docs/`, and a dated measurement
round belongs in `docs/reports/` with the environment and command that reproduce
it. A rule they are about to violate belongs here.

## Rules that are gates

Every rule worth keeping is worth the twenty lines that make it fail loudly.

| Promise                                             | What enforces it                                                                |
|-----------------------------------------------------|---------------------------------------------------------------------------------|
| no cgo                                              | `CGO_ENABLED=0` in the build, on all three CI platforms                         |
| the module carries only the engine                  | `TestTheModuleCarriesOnlyTheEngine`, over its own go.mod                        |
| importing this stays cheap                          | `task size` links a probe and reports what it cost                              |
| a sample survives the codec exactly                 | `TestSamplesSurviveTheCodecExactly`, on bits and not on values                  |
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
| a decode stays bounded                              | `TestSmallBlockCodecMemory`, against the 8 KiB ceiling                          |
| an iterator outlives its codec                      | `TestIteratorOwnsItsBytesAndOutlivesTheCodec`                                   |
| unordered or oversized input is refused             | `TestRejectsUnorderedAndOversizedInput`                                         |
| a counter's increase survives a reset               | `TestACounterKeepsItsIncreaseAcrossAReset`                                      |
| only the safe prefix is sealed                      | `TestOnlyTheSafePrefixIsSealed`, on the strict edge                             |
| a late sample cannot enter a sealed block           | `TestALateCounterSampleWouldRewriteASealedBlock`                                |
| a partial range is not answered from a summary      | `TestAPartialRangeNeedsTheRawEdges`                                             |
| retention clips before it summarises                | `TestRetentionClipsAPersistedBlockBeforeSummarising`                            |
| a quiet tail expires without becoming a block       | `TestASilentTailExpiresWithoutBecomingABlock`                                   |
| one expired sample does not delete a block          | `TestWholeBlockRetentionOvershoot`                                              |

`task check` runs exactly what CI gates on. When those two drift, the local one
is the weaker of the pair and a failure arrives after a push instead of before
it.

## Weight

This exists because the alternatives cost more memory than what they watch.

**No cgo, ever.** `CGO_ENABLED=0` in `task build`, `task size` and the CI build
on all three platforms, so a dependency needing a C toolchain fails on the pull
request that introduces it. This is why the file is `modernc.org/sqlite`; do
not swap it for a faster cgo driver. The race detector is the one exception —
it builds test binaries, never a product.

**A dependency is a decision, and here it is somebody else's decision too.**
Whatever the root module requires, every program importing this links. Check
what a module drags in, prefer the standard library, and put anything a
measurement needs in `bench/`.

**Watch the weight.** `task size` links a probe that calls the public API and
reports what the import cost. There is no threshold to game; the point is that
growth is visible and deliberate.

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
  which trap is being avoided. **One line.** Two is an exception you should be
  able to defend — when a comment runs onto a second line it is usually a
  `so that ...` clause, and that clause is rationale, which belongs in this
  file. Lower case, no closing full stop, in Go and YAML alike. Never restate
  the line below it.
- **A comment that restates its declaration is worse than none.** It costs a
  line, it ages on its own, and it teaches the reader that comments here can be
  skipped. When the name and the signature say it, write nothing. revive's
  `exported` rule is off for exactly this reason.
- Doc comments are exempt from the lower-case sentence form, **not from the
  length and not from the rule above**. They earn their place by saying what a
  caller cannot see — that the slice is not copied, that the iterator owns its
  buffer, that the codec may be closed while an iterator lives.
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
