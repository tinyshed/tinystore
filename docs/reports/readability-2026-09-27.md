# Reading the engines and auditing diagnostics — 2026-09-27

The diagnostic gaps below describe the code before
[the logging round](logging-2026-09-27.md), which addresses the background
engine scope, metrics suspension events, Debug summaries and KV error phase.
This audit remains the record of what the readers found.

This is a small readability pass before the sqldb round, plus an audit of
shared mechanics and diagnostic logging. It is not a performance rewrite
or an implementation of the proposed diagnostic events below.

The engine passes use `gpt-6-luna` with `max` reasoning. Each reads its whole
engine, including tests, README and migrations, before editing. The agent
interface exposes no separate fast-mode switch. Three engine agents run
concurrently; records follows when a slot becomes available.

## Scope and verification

| engine | files read | Go files | edited Go files |
|---|---:|---:|---:|
| KV | 31 | 29 | 10 |
| metrics | 67 | 65 | 10 |
| jobs | 22 | 20 | 4 |
| records | 70 | 66 | 2 |

All **190 engine files** were read, including four READMEs, four migrations
and records' two JSON golden fixtures. Changes touch 26 Go files. A lexical
check against the starting revision found only two intentional identifier
renames beyond comments: `names` to `seriesCount` in the matcher code/test,
and `expiredBlocks` to `expiredSamples` in retention. All other Go tokens,
compiler directives and example-test expected output are unchanged. The
changed files have 39 fewer comment lines overall, with several surviving
comments clarified rather than deleted. Non-Go engine files are unchanged.

`task check` passed on Windows. Linux passed
`go test -race -shuffle=on -count=1 ./kv ./metrics ./jobs ./records`.
Each reviewer also ran its engine's shuffled tests. A formatter-only
adjustment removed a trailing empty comment line beside the records head
layout example; the example itself remains.

The metrics identifier changes also passed an interleaved smoke run of
`BenchmarkIngest*` and `BenchmarkRankThirtyTwoMatchers`: before/after/after/before,
100 iterations each, Windows/amd64, Ryzen 7 7700, Go 1.27.1, 16 logical
processors, default temporary files on NTFS. Fixtures are defined by
`metrics/ingest_bench_test.go` and `metrics/match_test.go`. This short run
is a sanity check, not a performance claim; [raw output](data/readability-2026-09-27-metrics-bench.txt)
keeps both runs and allocation counts. Reproduce after compiling the same
test package on each side with `go test -c -o <binary> ./metrics`:

```powershell
foreach ($variant in @('before', 'after', 'after', 'before')) {
  & "<binaries>/metrics-$variant.exe" '-test.run=^$' `
    '-test.bench=^(BenchmarkIngest.*|BenchmarkRankThirtyTwoMatchers)$' `
    '-test.benchtime=100x' '-test.benchmem'
}
```

The starting production revision is `15bdb3bf0465dac657953a6d8d32ac49ff440817`.
The before benchmark binary included comment cleanup only; its executable
tokens were still those of that revision. No performance-sensitive algorithm
or SQL was changed in this pass.

The pass removes comments that merely repeat a declaration or narrate an
obvious next statement. It keeps API lifetime and ownership contracts,
durability and snapshot boundaries, cancellation rules, checked-arithmetic
explanations and worked examples. A multiline explanation is not noise just
because it occupies several lines. Migrations and golden fixtures remain
byte-for-byte unchanged.

Earlier uncommitted blobs implementation, measurements and reports are
separate work and are not part of the four-engine readability diff. The
unrelated `docs/report/` directory is left alone. No commit or push is made
by this pass.

## Where reuse already exists

Every engine uses the root lifecycle, directory claims, store clock and
memory reservations. `internal/sqlite` owns connections, prepared programs,
migrations, transaction mechanics, grouped commits and file snapshots.
`internal/admission` owns the close gate and bounded concurrent slots.
Those are real dependency boundaries and already share the expensive and
subtle mechanics without making engines import one another.

A shared package inside `internal/` does not require another Go module.
The root/tools/bench module split remains unchanged. A generic engine base
or a miscellaneous `internal/utils` package would obscure the remaining
engine policies rather than remove them.

| apparent duplication | decision | why |
|---|---|---|
| jobs and blobs `quietLog` | strongest candidate for one small internal counted-warning helper | both count events and emit once per quiet window; queue identity belongs at the caller |
| root `failureLog` versus those counters | keep its policy distinct | a changed error logs immediately and success emits recovery; quietLog counts a category and has neither behavior |
| `holdMaintenance` in five engines | reuse admission machinery in a separate, tested change | existing receive-token selects and `Slots.Take` differ for an already-cancelled context; replacing one with the other changes observable behavior |
| engine admission/reservation wrappers | keep local around existing shared primitives | errors, weighting, disabled budgets, transaction ownership and `ReserveNow` differ; the wrappers expose those choices |
| KV/blob key spelling and prefix ends | keep separate | KV admits byte keys and uses escaped binary branches; blobs validates UTF-8 path segments; jobs also handles prefixes with no finite upper bound |
| records cursor and metrics binary reader | keep format-specific checks | records additionally accounts expansion and aliases strings, metrics handles its scalar representation; shared varint calls alone do not justify another parser layer |
| transaction wrappers/savepoints | keep domain outcomes local | jobs runs post-commit effects and reserves ids, KV has read-only View and memory rules, metrics quarantines one series; sqlite already shares the transaction mechanism |
| short Snapshot and revision wrappers | leave them | the file mechanics are shared already; engine file names, failure context, metadata and blob hard links remain engine-owned |

No extraction is performed on the strength of matching function names.
A logging helper extraction should follow the logging-policy decisions below,
so it preserves the suppression key, counts, field types and tail behavior.

## Readability findings to carry forward

Two metrics names were corrected in this pass: `matcherPosting.names`
counted series, and the local `expiredBlocks` counted samples. The new
`seriesCount` and `expiredSamples` state what the numbers mean. KV's
`deleteRange` comment incorrectly promised a count although the function
returns only an error; that claim was removed. The descriptor comment in
metrics now states why lengths are fetched before tail bytes are charged.

Remaining candidates, deliberately not expanded into larger rewrites:

- `kv/branch.go`: `branch.hidden` and `call.hidden` contain packed ancestor
  prefix lengths, unlike `cell.hidden`, a boolean. A name such as
  `clearPrefixLengths` would remove the collision. `kv/bucket.go:settled`
  means resolved bucket settings and codec.
- `jobs/queue.go:queueState.waiting` counts active rows including leased jobs.
  `jobs/work.go:workLoop.hold` is the total held-job cap.
  `jobs/read.go:found.time` is either next due time or failure time. Those
  meanings deserve distinct names when the surrounding work-loop/scan code
  is next changed.
- `metrics/aggregate.go:aggregateAccumulator` holds several exact-arithmetic
  roles under short field names. Clarify them as a focused arithmetic review,
  preserving the aggregate contract and tests.
- Records' integer codec and timestamp templates remain intrinsically dense.
  Their worked examples, expansion bounds and shared parsing path are useful
  explanations, not comment noise; no format rewrite is justified by this pass.

## The reviewers' overall verdicts

The following are summaries of four separate engine readers' judgments,
not release or performance certifications:

- **KV:** a well-built embedded store with clear promises. The reviewer
  singled out bit-preserving value representations, revision-based immediate
  Clear, grouped writes with isolated rollback, reservations before encoding,
  and the explicit crash-loss contract for relaxed counters. The main
  reservations were diagnostic attribution and a few overloaded names.
- **Metrics:** careful storage engineering around several difficult
  invariants. One head parser, byte-preserving chunk reuse, atomic ingest,
  independently bounded snapshot work, and version-checked prefix publication
  were the strongest decisions. Format complexity and unfinished aggregate
  summary acceleration remain real maintenance costs.
- **Jobs:** a strong core, especially attempt-bound leases that reject stale
  workers, settling and claiming in one write, bounded worker prefetch,
  memory acquired before value materialization, and zone-aware repeat tests.
  The work-loop protocol is complex and logs reveal less than the execution
  state actually knows.
- **Records:** a designed storage engine rather than a collection of probes.
  The reviewer highlighted separate event and publication order, bounded
  checked decoding, short SQLite snapshots, stable Follow positions across
  merging, and the bounded follow cache. Codec maintenance and visibility of
  dropped records are the main reservations.

These are substantive strengths: the code addresses storage failures,
concurrency and memory limits with explicit contracts and corresponding
tests. Sparse diagnostics do not erase those achievements.

## What logging currently provides

`Options.Logger` supplies the application's `*slog.Logger`; nil uses a
discard handler. `Store.Logger(name)` attaches `engine=name`. Events are
ordinary structured slog records, not a Go execution trace. There is no
automatic trace capture or diagnostic-bundle export in the current runtime.

| source | present events and fallback information | important gap |
|---|---|---|
| runtime | store opened/closed; background failure and recovery with work/error/failures; same error suppressed for ten minutes | Every uses the root logger without an engine field; closed is logged even when the returned Close error is non-nil |
| KV | engine opened/closed; Every reports expiry, relaxed-counter flush and renewal failures; ordinary errors return to the caller | a combined Maintain error can omit the failed stage and counter bucket; no Debug count/duration summary |
| metrics | opened with suspended count, closed, instrument refusal and gauge-read warnings; Stats and ListMaintenanceFailures are pull diagnostics | a newly quarantined series can make maintenance return success without a log event; no Debug maintenance summary |
| jobs | counted warnings for terminal failures, exhausted leases, capacity and panics; malformed repeats log queue/key/error | most warnings omit job identity/attempt; the panic log omits its captured stack; a transient failed attempt may later disappear after success |
| records | lifecycle, close totals, once-per-handle damaged-row Error with stream/row/time/reason; Stats.Dropped and Damaged provide pull diagnostics | dropped records have no reason breakdown; no flush/maintenance Debug summaries; own background warnings lack the attribute used by the self-log guard |
| blobs, root review only | lifecycle; counted removal warnings; scrub damage with content and keys; snapshot missing-file warning | successful maintenance/scrub progress has no Debug summary; ordinary caller errors are intentionally not duplicated |

The architecture logging table is a target, not a complete inventory of built
events: migration-applied Info and maintenance/flush Debug summaries are not
currently emitted. Metrics suspension logging is another concrete gap.

### Highest-priority diagnostic issues

1. **Metrics can suspend a series without an event.**
   `metrics/quarantine.go:handleMaintenanceFailure` persists isolated
   corruption/resource failure and returns success after counting the new
   quarantine. `metrics/publish.go:publishIsolated` similarly converts a
   corrupt publication into a count. The background adapter discards the
   counts, so Every sees no failure. Emit one event after a new suspension
   commits, with series id, phase and reason; labels need an explicit policy.
   Keep `ListMaintenanceFailures` as the bounded persisted source of detail.
2. **Background failures lack the engine routing attribute.**
   `every.go` emits through the store logger. The records handler's own-line
   guard in `records/handler.go:isOwn` recognizes `engine=records`, not a
   work-name string. A records flush/maintenance failure therefore lacks the
   attribute that would let this guard recognize its origin. An engine-aware
   background registration must preserve that attribute without teaching the
   root package a list of engine names. An outside-repository probe confirmed
   that a synthetic records-maintenance failure was persisted through the
   handler with no engine field. This demonstrates routing, not infinite
   recursion. One API-neutral direction is an internal typed origin wrapper
   on background errors, preserving Error/Unwrap, with Every retaining that
   origin through the recovery event as well.
3. **The nonblocking promise has a boundary.**
   `Options.Logger` is called synchronously; an arbitrary application handler
   can block. The built-in records handler has a bounded nonblocking enqueue,
   but this is not an asynchronous wrapper around all supplied loggers.
   Decide and document the caller's handler obligation or provide a bounded
   runtime-owned adapter with explicit dropped-log accounting. Never add a
   goroutine per message.
4. **Aggregate background errors need their phase.**
   KV Maintain joins flush/renew/clear/expiry errors, and its counter flush
   loop may omit the bucket name. Wrap stage context before joining. A
   maintenance summary should include the partial counts even on failure.
5. **Jobs panic evidence is incomplete in the log.**
   `jobs/work.go:call` captures a stack in the attempt error but passes only
   the panic value to its counted warning. Queue plus a bounded job identifier,
   attempt and stack reference would let a support report locate that failure.
   This does not imply logging every successful or retried job.

## A useful diagnostic policy

This is a proposal, not newly implemented instrumentation:

- Info for lifecycle and applied migrations, with outcome and schema version
  where known. A failed Close must not look like unqualified success.
- Warn/Error for meaningful transitions: new quarantine/damage, lost lease,
  background failure and recovery. Decide which key defines a repeated
  incident before sharing a suppression helper.
- Debug for one completed maintenance/flush pass: engine, operation, elapsed
  time, counts, partial outcome and error phase. Check `Enabled` before
  building expensive attributes. No normal per-sample, per-key or per-query
  success log.
- Return ordinary request errors to the caller that has request context;
  log inside the engine when work has no such caller or the engine absorbs
  a failure and changes state. Avoid automatically logging the same error
  at every layer.
- Keep payloads and arbitrary application values out of new default events.
  Existing paths, labels, keys and raw error text can already contain
  application-specific data; the diagnostic format must state that boundary.

A support packet should identify the application/TinyStore build, Go version,
OS/architecture, relevant options and time window, then include structured
engine events and existing Stats/failure lists. Scheduling, lock and I/O
investigations additionally need a bounded Go execution trace or profile
captured during the problem. Records' TraceID/SpanID data fields do not by
themselves instrument TinyStore's internal operations. A full database or
application payload dump is not a prerequisite for that packet.

Implement diagnostics separately from this readability pass, with gates for
disabled-level allocations, repeated-error counting, records self-log
rejection, failing Close outcomes and state-change events only after commit.
