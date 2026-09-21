# TinyStore

An embedded time-series store for Go, on SQLite: exact samples, bounded memory,
one file, no daemon. It is a library, so the only thing a consumer sees is a
handle and the promises this file makes about it. `tinyshed/dashbin` is the
first caller and not the owner.

This file is the contract for anyone — human or agent — changing the repository.
Keep it short and factual, and update it when a decision moves. Nothing here
should be a fact a ten-second grep would answer.

## Status

`codec/` is built, tested and measured: 1..240 ordered samples in, one bounded
payload out, every IEEE-754 bit preserved, a versioned header, a checksum, an
iterator that owns its bytes and a fuzzed decoder that refuses corruption. It
has no opinion about when a block is sealed or how long one is kept.

Nothing else is built. There is no store, no registry, no compactor, no query
and no schema. `spike/` holds prototypes that measured the shape the store will
have, and a handful of gates that pin arithmetic which is easy to get wrong.
[docs/design.md](docs/design.md) is a design with measurements behind it, not a
description of running code.

The rules below are written in the vocabulary that was measured: a durable
**head** of recent rows and immutable **blocks** behind it. The physical shape
of that head is an open question — a packed mutable tail inside the series row
would remove a table and most of the write amplification, and it has no
measurement behind it yet. [docs/research.md](docs/research.md) holds the
candidate and the numbers it has to beat. What does not change either way is
the frontier, the retention arithmetic, the snapshot and the exactness.

Do not describe unbuilt behaviour as though it works.

## Shape

| Path | What it is |
|------|------------|
| `codec/` | the block codec and the payload format. Knows samples and bytes, nothing else |
| `spike/` | prototypes and measurements, skipped unless `TINYSTORE_SPIKE=1` |
| `tools/` | a second module pinning developer tools. Two files, never hand-edited |
| `docs/` | the design, the format, the numbers, the open questions |
| `.github/workflows/` | the authoritative clean builds |

Planned, and not yet written:

```text
tinystore.go        Open, Close — the whole surface a caller sees
ingest.go           a batch in
query.go            a range out, and the budget it may spend
series.go           Sample, SeriesID, Labels, Matcher
internal/
  sqlite/           the handle, two pools, the pragmas, the migrations
  series/           the registry, its postings and its bounded cache
  block/            the head, the sealing, the immutable blocks
  query/            one snapshot, the merge, the aggregation
bench/              a module of its own: corpora, and other engines to measure against
```

`spike/` is deleted when that exists. Its gates move onto the real
implementation, because a harness measuring a prototype nobody runs any more is
worse than no harness at all.

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
head rows directly and blocks whose last sample is before the cutoff. An
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

**What a query sees is blocks and the head from one snapshot.** The head is a
durable table, the compactor writes a block and deletes what it packed in one
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

**Nothing scans every series.** Postings are in the first version rather than
in an optimisation after it, and finding due work is an index over one value
per series rather than one per sample. A million active series without a
redesign is what the format and the query model are built for; what an
installation actually permits is a configured limit, and the two are not the
same promise. Cardinality and ingest rate are promised separately.

**The format carries its own version, and a released one never moves.** The
payload names its version in its third byte and a reader keeps every version it
has ever written. The module's version, the payload format's version and the
schema's version are three numbers; conflating them is how a `v0.4.0` ends up
meaning something about bytes on somebody's disk.

**A measurement is a number with its environment, or it is an anecdote.** Every
figure in `docs/` carries what produced it — machine or container, versions,
fixture, sample count — and the command that reproduces it. A candidate is
compared against what it replaces on identical input, in the same run. Take the
economics from a measurement and not the explanation of the mechanism: the
number is evidence, the story about why is a hypothesis until a second
measurement separates it from the alternatives.

**A payload's size is not a file's size.** SQLite stores rows in fixed pages,
so a payload that shrinks by a tenth can leave the file exactly as large, and
one that shrinks by a fiftieth can shrink it sharply by fitting one more row on
a page. Every codec result is therefore reported twice: bytes a sample in the
payload, and bytes a sample in a real file.

## Where things are written down

This file holds what gets broken: rules, invariants and traps. Read it before
changing something, not to look something up.

| | |
|---|---|
| [docs/design.md](docs/design.md) | how the store is meant to work, and why that shape |
| [docs/format.md](docs/format.md) | the bytes: the payload's layout, version by version |
| [docs/measurements.md](docs/measurements.md) | every number, its environment and how to reproduce it |
| [docs/research.md](docs/research.md) | what is not built: the open questions and their acceptance gates |

A reference a contributor returns to belongs in `docs/`. A rule they are about
to violate belongs here.

## Rules that are gates

Every rule worth keeping is worth the twenty lines that make it fail loudly.

| Promise | What enforces it |
|---------|------------------|
| no cgo | `CGO_ENABLED=0` in the build, on all three CI platforms |
| the module carries only the engine | `TestTheModuleCarriesOnlyTheEngine`, over its own go.mod |
| importing this stays cheap | `task size` links a probe and reports what it cost |
| a sample survives the codec exactly | `TestSamplesSurviveTheCodecExactly`, on bits and not on values |
| the extremes survive too | `TestExactBitsAndTimestampExtremes`, on `-0`, NaN payloads and the ends of time |
| a corrupt payload is refused | `TestPayloadCorruptionIsRefused` and `FuzzDecode` |
| a decode stays bounded | `TestSmallBlockCodecMemory`, against the 8 KiB ceiling |
| an iterator outlives its codec | `TestIteratorOwnsItsBytesAndOutlivesTheCodec` |
| unordered or oversized input is refused | `TestRejectsUnorderedAndOversizedInput` |
| a counter's increase survives a reset | `TestACounterKeepsItsIncreaseAcrossAReset` |
| only the safe prefix is sealed | `TestOnlyTheSafePrefixIsSealed`, on the strict edge |
| a late sample cannot enter a sealed block | `TestALateCounterSampleWouldRewriteASealedBlock` |
| a partial range is not answered from a summary | `TestAPartialRangeNeedsTheRawEdges` |
| retention clips before it summarises | `TestRetentionClipsAPersistedBlockBeforeSummarising` |
| a quiet tail expires without becoming a block | `TestASilentTailExpiresWithoutBecomingABlock` |
| one expired sample does not delete a block | `TestWholeBlockRetentionOvershoot` |

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
- Do not commit machine-specific paths, and do not commit a corpus.

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
