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

The reference is commit `407e728`, merged into `main`: the ingest path from
`Ingest` to the packed head, rebuilt this way.

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
tinystore/doc.go     the runtime: engines, directory, lifecycle   (step 6)
metrics/doc.go       the pipeline and a map of the files
metrics/store.go     the handle: Open, Close, Stats
metrics/<step>.go    one step per file; a public method opens the file of its step
```

The files of `metrics/` since step 4, one package; a test file is named after
the file it tests, and `store_test.go`, `corpus_test.go`, `recovery_test.go`,
`physical_test.go` and `example_test.go` cross every step:

| file | holds | area |
|---|---|---|
| `doc.go`, `store.go`, `types.go`, `options.go` | map; handle and lifecycle; public model; options, limits, defaults | — |
| `admission.go` | open/closed gate, slots, the shared `WorkBudget` | G |
| `ingest.go`, `ingest_input.go` | `Ingest`; one call's batches checked and grouped | B |
| `series.go`, `match.go` | identity, dictionary, postings; matchers to series | E |
| `head.go`, `head_state.go` | the packed head format; its row in `series_state` | B |
| `read.go`, `snapshot.go`, `snapshot_heads.go`, `snapshot_groups.go`, `snapshot_payloads.go` | `Read`/`Stream` and the query budget; one snapshot and its batched fetches | D |
| `aggregate.go` | `Aggregate`, exact buckets | D |
| `maintain.go`, `seal.go`, `publish.go`, `merge.go` | the pass; the safe prefix encoded; publication; merging groups | F, C |
| `retention.go`, `quarantine.go` | expiry and reclamation; suspended series | F |
| `group.go`, `directory.go`, `clock.go`, `summary.go` | sealed format | C |
| `values.go`, `values_changes.go`, `values_grid.go`, `residuals.go`, `binary.go` | value representations and bounded binary reads | A |
| `migrations/0001_schema.sql` | the one schema; a later change is `0002_…` | — |

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
3. Done: what only read old files is gone: the row-per-sample head, version-one
   directories, codec-body blocks, clock id 0, text identities and the
   `labels` column, with the four tests that exercised them
   (`TestLegacyHeadConvertsOnFirstMutation`,
   `TestSchemaOneFileUpgradesWithoutChangingSamples`,
   `TestDirectoryVersionOneStillReads`,
   `TestLegacyLabelIdentityIsReusedAndCompacted`) and `FuzzDirectory`.
   Migrations `0001`…`0009` are one `0001_schema.sql`.
4. Done: the skeleton. Files moved to the table above by whole declarations,
   tests renamed or merged to mirror them, `doc.go` holds the map. `lll` (120),
   `funlen` (60 lines, comments excluded) and `gocognit` (30) gate new code;
   `.golangci.yml` lists by name what does not pass yet, and an area deletes
   its entries as it clears them.
5. **By area**, one or two at a time on disjoint files, each merged green:
   A codec, B head, C seal/publish/merge/group/directory/clock,
   D read/snapshot/aggregate, E series/match, F maintain/retention/quarantine,
   G admission and `internal/sqlite`. The file table above names each file's
   area. The order is G, E and F, D, then A and C, the format code, last; B is
   the reference and only checked.
   - Done, G: `internal/sqlite` reads as steps (`Open`, `Migrate`, `view`,
     `update`), `transactReusable` returns its error last, each test file is
     named after the file it tests, and the connection URLs and the migration
     history carry worked examples that are also tests. Against `e316a95`,
     interleaved on one 4-vCPU linux/amd64 container with Go 1.27.1:
     `BenchmarkIngest*` medians moved −0.5% to +3.3% (the long-head append
     over 16 runs a side, permutation p = 0.24, inside the base's own 9%
     interquartile range), `BenchmarkPreparedReadAfterApplicationError` −8%,
     allocations identical to the count; the four `bench/perf/repro` stages
     with the parameters of the saved-revision round, in ABBA order, stayed
     inside the spread of the base's own runs, and append wrote the same
     663 552 bytes. `task size` grew by 4 KiB, to 6 664. Later the shared
     `WorkBudget` grants queued reservations in arrival order; it still
     reserves the worst case, about 34 MiB for a default `Read`, because that
     is the one bound it can promise.
   - Done, E and F: registration, matching, `Maintain`, expiry and
     reclamation read as steps; a `maintenancePass` carries one call's cutoff,
     staged publications and counts, `dueSeries(packing bool)` is
     `expiryDue` and `readyToSeal`, and every statement is a named constant
     beside its function. The three matcher lookups stay, as measured in
     `reports/matcher-batching-2026-09-23.md`, and so does the SQL of varying
     arity, as measured in WP3 of `reports/architecture-hypotheses-2026-09-23.md`.
     Against `cca752f` on the same container: `BenchmarkIngest*` medians moved
     −3.3% to −0.2% and `BenchmarkRankThirtyTwoMatchers` +4.2% (8 runs a
     side, p ≥ 0.4), allocations identical; the four `bench/perf/repro` stages
     moved −4.1% to +7.6% (seal over 16 runs a side, p = 0.28), each inside
     the spread of the base's own runs, and each wrote the same file bytes.
     `task size` grew by 12 KiB, to 6 676.
   - Done, D: `Read`, `Stream` and `Aggregate` are admitted, checked, reserved
     and fetched by the same steps (`admit`, `checkRange`, `reserve`,
     `fetchSnapshot`); a `snapshotRead` carries one read transaction's
     connection, range and budget; one `eachSample` walks blocks and head for
     reads and aggregates alike; `sqlite.EachRow` replaces six hand-written row
     loops that closed their rows early. `Aggregate` now also checks
     cancellation once admitted, as `Read` already did. The fetch trace in
     `TestBatchedHeadsPreserveSnapshotAndReserveBytesFirst` recognises the tail
     and directory queries by their constants rather than by a fragment of
     their text, which the new formatting would have silently defeated.
     Against `64bbc08`, four runs a side in ABBA order on a 2 000-series
     `bench/perf` file: a narrow hour −1.6%, a 500-series selector ±0%, a
     region aggregate +0.5%, and the `repro` read stage +2.9%, each p ≥ 0.37;
     allocations per wide query fell about 4%, from 56.2k to 53.9k. `task size`
     grew by 4 KiB, to 6 684.
6. The runtime: `tinystore.Open` and friends from `samples/runtime`, then
   `metrics.Open(ctx, store, …)`, `records`, `sqldb`.
7. New engines, one at a time.

## Gaps to close during the rewrite

| area | gap |
|---|---|
| E, F | a series that cannot be repaired cannot be removed: add `DropSeries`, and decide what happens to payloads a corrupt directory no longer names |
| F | a suspended series is skipped by retention, so its expired samples stay on disk |
| D | a bucket clipped by retention reports its nominal `From`; the caller cannot tell it is partial |
| A, C | `values.go` `readOrdinary` rebuilds the codec's private envelope; give the codec a checked value-stream entry point |
| A, C | file-level `//nolint:gosec` in `clock.go`, `directory.go`, `values.go`, `values_changes.go`, `values_grid.go`, `residuals.go` |
| all | the debt entries in `.golangci.yml`: 8 functions over `funlen` or `gocognit`, 18 files with lines over 120 columns |
| C, D | block summaries are written and never read: the versioned exact summary shortcut, or fewer summary bytes |
| docs | `docs/research.md` "Order of work" still lists shipped items |
