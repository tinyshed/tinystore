# Rewriting metrics for people

The metrics engine works and is measured, and it is hard to read: functions of
100+ lines, lines of 300 columns, three parsers for one byte layout. This is
the plan to make it read top to bottom without changing what it does, what it
writes or how fast it is.

## The method: move functions, not behaviour

Every step moves or splits existing code under the existing tests. A step never
changes behaviour and structure at once. The code already holds every edge
case that was found the hard way (CRCs, bounds, frontier checks, budgets before
materialisation); writing it again from memory loses some of them silently.

The reference is branch `claude/tender-cannon-6z3mxv`, commit `407e728`: the
ingest path from `Ingest` to the packed head, rebuilt this way.

| before | after |
|---|---|
| `Ingest`, 60 lines, admission copied inline | `Ingest`, 31 lines: `admit` → `reserve` → `prepareIngest` → `commitIngest` |
| `prepareIngest`, 78 lines | `ingestInput.add`, `seriesFor`, `pendingSeries.add/sorted`, `seriesKind` |
| `writeHead` and `head_append.go` | `accepts` → `storedChunks` → `nextHead` → `appendToHead` or `mergeIntoHead` |
| `decodeHead`, `inspectHead`, `reusableHeadPrefix`: three parsers | one `parseHead`, then `decodeChunks` and `reusableChunks` |
| `encodeHeadPrefix` → `encodeHeadSuffix` | `encodeHeadAfter` and `appendHeadChunk` |
| enter, slot and budget copied into four methods | `admit` and `reserve` in `admission.go` |

It passed the whole suite with `-shuffle=on` and `-race`, golangci-lint with no
issue, the packed-head golden vector byte for byte, and the new
`BenchmarkIngest*` against the previous code, interleaved on one machine
(4 vCPU container, Go 1.27.1, 2000 iterations, six rounds): append 449 → 472
µs, replace inside the head 1026 → 1028 µs, a hundred-series scrape 6.60 →
6.26 ms with 20% fewer bytes allocated; the append medians swapped order in an
earlier run, so all three are within noise. Functions over 40 lines went from
7 to 1. Two tests went with the row-per-sample head they tested.

## Reading path

```text
tinystore/doc.go     the runtime: engines, directory, lifecycle   (after step 5)
metrics/doc.go       the pipeline and a map of the files
metrics/store.go     every public method, each a list of steps
metrics/<step>.go    one step per file
```

Target files of `metrics/`, one package; tests mirror them one to one:

| file | holds |
|---|---|
| `doc.go`, `store.go`, `types.go`, `options.go` | map, public methods, public model, defaults and limits |
| `admission.go` | open/closed gate, slots, shared budget |
| `ingest.go`, `ingest_input.go` | `Ingest`; one call's batches checked and grouped |
| `series.go`, `match.go` | identity, dictionary, postings; matchers to series |
| `head.go`, `head_state.go` | the packed head format; its row in `series_state` |
| `read.go`, `snapshot.go`, `budget.go`, `aggregate.go` | `Read`/`Stream`; fetching from one snapshot; query budget; exact buckets |
| `maintain.go`, `seal.go`, `publish.go`, `merge.go` | the pass; the safe prefix and its blocks; publication; merging groups |
| `retention.go`, `quarantine.go` | expiry and reclamation; suspended series |
| `group.go`, `directory.go`, `clock.go`, `summary.go` | sealed format |
| `values.go`, `values_changes.go`, `values_grid.go`, `residuals.go`, `binary.go` | value representations and bounded binary reads |
| `schema.sql` | the one schema |

## Rules

The editing rules in `AGENTS.md` are the law; this is how they look in code.

1. A public method is a list of steps separated by blank lines, each a call
   named with a verb (`admit`, `prepareIngest`, `commitIngest`). Details live
   in the functions it calls.
2. A function over 40 lines needs a reason, over 60 is split. No line over 120
   columns. More than four parameters become a named value (`headWrite`).
3. SQL is a named, formatted constant beside the function that runs it.
4. Every transform shows one worked example, and the same example is a test
   case:

   ```go
   // reusableChunks returns the leading chunks a write at `before` leaves as they
   // are: full, and ending before it.
   //
   //	chunks      [0 … 239] [240 … 479] [480 … 520]
   //	write at 300               ↑
   //	reusable    [0 … 239]                            copied byte for byte
   ```

5. One byte layout, one parser. Decoding, reuse and partial reads start from
   what it returns.
6. No file-level `//nolint`. One line with its reason, or a helper that owns the
   conversion once (`distance`, `advance`, `appendCount` in `head.go`).
7. A test is named after the unit it tests; a deleted test goes with the code it
   tested, and the commit says which.

## Gates for every step

- `go test -shuffle=on ./...` and `go test -race ./...`, pinned golangci-lint,
  formatting, `go mod tidy -diff`: what `task check` runs.
- Golden vectors unchanged, unless the step says it changes a format.
- `BenchmarkIngest*` and the relevant `bench/perf` stage before and after,
  interleaved on one machine, within noise.
- No test weakened.

## Order of work

1. Done: `codex/architecture-measurements` fast-forwarded into `main` at
   `cad0ea8`; every cited commit kept its hash.
2. Done: `docs/architecture.md`, this file, `AGENTS.md`.
3. **Delete what only reads old files.** No database written by an earlier
   revision exists, and nothing is tagged.
   - row-per-sample head: the `head` table, the legacy branches of `fetchHead`,
     `decodeHead`, `loadIngestState`, `writeHead`, `saveHead` and `head_batch.go`,
     `TestLegacyHeadConvertsOnFirstMutation`,
     `TestSchemaOneFileUpgradesWithoutChangingSamples` (the reference commit
     already did all but the table);
   - version-one directories: `directoryHeader`, `directoryBlock`,
     `encodeDirectory`, `decodeDirectory`, the `data[0] == 1` branch of
     `readDirectory`, `format < 2` in `writeDirectory`, codec-body blocks
     (`block.format < 2`) in `decodeBlock`, clock id 0,
     `TestDirectoryVersionOneStillReads`, and their uses in `publication_test.go`;
     version two is still written for new groups and keeps its golden reader;
   - text identities: the `labels` column, JSON identities in
     `resolveSeries` (`identity in (?,?,?)`), the legacy branches of
     `storedIdentity.confirms`, `decodeLabels` and `coalesce(s.labels,
     s.identity)` in `matchShape`, `TestLegacyLabelIdentityIsReusedAndCompacted`;
   - migrations `0001`…`0009` become one `0001_schema.sql` holding the final
     tables and indexes, without `head` and `series.labels`.
4. **Skeleton.** `doc.go` with the map, `store.go` with the public methods as
   step lists, files moved to the table above, tests renamed to mirror them.
   Add `lll` (120), `funlen` (60 lines) and `gocognit` to `.golangci.yml` with
   the exceptions that remain listed. Mechanical moves only.
5. **By area, in parallel** on disjoint files, each merged green: A codec,
   B head, C seal/publish/merge/group/directory/clock, D read/snapshot/aggregate,
   E series/match, F maintain/retention/quarantine, G `internal/sqlite`. Steps 3
   and 4 go first and in one session: they set the seams the areas share.
6. The runtime: `tinystore.Open` and friends from `samples/runtime`, then
   `metrics.Open(ctx, store, …)`, `records`, `sqldb`.
7. New engines, one at a time.

## Gaps to close during the rewrite

| area | gap |
|---|---|
| E, F | one suspended or corrupt series fails the whole atomic `Ingest`, and the error does not name it: wrap it in an error carrying the labels; add `DropSeries` |
| F | a suspended series is skipped by retention, so its expired samples stay on disk |
| D | a bucket clipped by retention reports its nominal `From`; the caller cannot tell it is partial |
| G | `WorkBudget` reserves the worst case (about 34 MiB for a default `Read`) and has no queue order |
| E | `rankMatchers` has three paths; `lookupLabelIDs` and `increasePostingCounts` send SQL of varying arity through the 32-program writer cache |
| A, C | `values.go` `readOrdinary` rebuilds the codec's private envelope; give the codec a checked value-stream entry point |
| G | `transactReusable` returns `(error, bool)`; the error goes last |
| C | file-level `//nolint:gosec` in `clocks.go`, `directory.go`, `values.go`, `residuals.go` |
| C, D | block summaries are written and never read: the versioned exact summary shortcut, or fewer summary bytes |
| docs | `docs/research.md` "Order of work" still lists shipped items |
