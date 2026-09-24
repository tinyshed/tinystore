# The first metrics slice

This package now opens a real file, registers series, commits samples, reads
exact ranges, seals eligible history, reopens it, and expires old data.
It has no dependency on Records, KV, a SQL mapper or the experiment harness.

## Follow one series

The runnable [example](example_test.go) writes 241 samples of `cpu{host="web-1"}`,
seals the first 240, closes the store and reads across the sealed/head boundary
after reopening. No binary-format knowledge is needed to use or follow it.

1. **Open** (`store.go`) validates limits, opens `internal/sqlite` and applies
   checked SQL migrations. A metrics file has its own application identity.
2. **Ingest** (`ingest.go`) validates and sorts a bounded batch, resolves its
   series through `series.go`, merges incoming points into its packed head,
   then advances its state. `head.go` owns reading and replacing that tail.
   Everything commits together. Last input wins for duplicate mutable timestamps.
   Ordered unique input skips the timestamp map and sort; a duplicate or late
   point uses the same last-input-wins fallback. The writer reuses up to 32
   prepared SQL programs on its one connection. On a long packed head, a
   strictly newer batch with zero lateness and no retention overlap validates
   the whole head but decodes only the changing suffix; completed chunks keep
   their encoded bytes.
3. **Read** (`read.go`, `snapshot.go`) resolves exact label matches through
   postings (`match.go`) and copies
   the required directories, payloads and encoded heads from one snapshot. It ends
   that snapshot before decoding. An error returns no partial answer.
4. **Maintain** (`maintain.go`, `seal.go`, `publish.go`) first expires due work, then reads a safe head
   prefix, encodes it outside the writer, and publishes only if its version is
   unchanged. Publication writes payloads, removes those exact head samples and
   moves the sealed frontier in the same transaction.
   It stages at most eight series and 1 MiB for a publication transaction;
   savepoints isolate conflicts, and counters advance after the outer commit.
   A local corrupt or over-limit series is suspended and recorded so later series
   can continue; `RetryFailedMaintenance` re-enables a bounded group after repair
   or a limit change. File and I/O failures still stop the pass.
5. **Expire** (`retention.go`) removes expired head points and whole expired
   microblocks. A partly expired group keeps its original payload addresses.
   Once a series has no head or groups, retention removes its registration,
   postings and unreferenced dictionary pairs in the same transaction.

The value encoder in `values.go` compares the ordinary codec with constant,
change-event (`values_changes.go`) and decimal-grid (`values_grid.go`)
candidates. `clock.go` stores time separately
and shares identical clock groups, with ownership maintained in the publication
transaction. None of these choices appear in the public request types.

For example, 241 timestamps `0..240` with zero lateness give a watermark of 240.
Only timestamps strictly below it may seal. The first 240 form one block; sample
240 remains in the head. A quiet series does not seal merely because time passes.
The ready queue is entered only when 240 retained samples are strictly before
the watermark; a full but unsafe head does not cause repeated empty passes.

## Public use

```go
store, err := metrics.Open(ctx, "metrics.db", metrics.Options{
    Retention: 30 * 24 * time.Hour,
    Lateness:  5 * time.Minute,
})
if err != nil {
    return err
}
defer store.Close(context.Background())

series := metrics.Series{
    Kind: metrics.Gauge,
    Labels: []metrics.Label{
        {Name: "__name__", Value: "cpu"},
        {Name: "host", Value: "web-1"},
    },
}
err = store.Ingest(ctx, []metrics.Batch{{
    Series: series,
    Samples: []metrics.Sample{{At: time.Now().UnixMilli(), Value: 42}},
}})
if err != nil {
    return err
}

result, err := store.Read(ctx, metrics.Range{
    Matchers: series.Labels,
    From:     from.UnixMilli(),
    To:       to.UnixMilli(), // exclusive
})
```

Call `Maintain(ctx)` from the embedding application's maintenance loop. Each
call considers at most `MaintenanceSeries` due series for expiry and that many
ready series for packing. There is no hidden maintenance timer yet.
`Maintenance.QuarantinedSeries` counts newly suspended series in that call;
`Stats.QuarantinedSeries` reports the persisted current count, including after
reopen. `ListMaintenanceFailures(ctx, afterID)` pages through stored reasons,
at most `MaintenanceSeries` rows per call; start with `afterID=0` and continue
from the last returned `SeriesID`. Those IDs are diagnostic cursors, not handles.
`RetryFailedMaintenance(ctx)` clears at most `MaintenanceSeries` failures per
call. A still damaged series will be suspended again on the next pass, and
ingestion into a suspended series returns `ErrSuspended`.

`Stream(ctx, range, yield)` calls `yield` once per nonempty series, in the same
series order and with the same exact owned samples as `Read`. It fetches one
snapshot and closes the read transaction before the first callback. A callback
can stop early by returning an error, which `Stream` wraps; cancellation is
checked during decode and before later callbacks. Earlier callbacks may have
received results when a later decode, output limit, callback or cancellation
fails. `Read` retains its all-or-error behavior. Streaming reduces output
materialization to one series at a time while the call is active; the encoded
bytes fetched from the snapshot remain held until it returns. The caller may
retain callback results, so this is not a total RSS bound. Read admission and
an optional shared work reservation remain held through every callback. A
callback must not call `Close` on the same Store or synchronously re-enter it
when the configured active-read limit is exhausted; both waits would depend on
the callback returning.

`Aggregate(ctx, metrics.AggregateRequest{Range: r, Width: time.Hour,
Op: metrics.AggregateSum})` returns one result per matched series and one value
per nonempty bucket. Buckets start at `r.From`; retention clips contributing
samples without shifting them. `AggregateCount`, `AggregateMin`,
`AggregateMax` and `AggregateIncrease` are also available; increase requires
a counter series. `Count` and `Resets` remain integers, `Value` is rounded once
from exact finite arithmetic, and `Overflow` distinguishes a finite sum that
rounded to infinity. An error returns no results. `OutputSamples` limits
buckets, while `DecodedSamples` limits raw work. The current engine decodes raw
for every aggregate; [the numerical contract](../docs/aggregate-contract.md)
specifies nonfinite, reset and boundary behavior.

## Contracts and defaults

- Timestamps are signed Unix milliseconds; MaxInt64 is reserved as an exclusive
  upper bound. Values, including signed zero and NaN payloads, are stored as
  exact bits. An invalid counter value remains readable; its summary is marked
  unusable. `Aggregate` computes exact `count`, correctly rounded `sum`,
  signed-zero-aware `min`/`max`, and counter `increase` from raw samples in one
  snapshot. It does not use the existing float64 directory summaries.
- `__name__` is required on ingestion. Labels are case-sensitive and unique by
  name, and are bounded by four budgets rather than one count: at most 128
  pairs, 16 KiB of combined raw name/value bytes, 256 bytes for one name and
  4 KiB for one value. Stock container telemetry carries 36 to 37 labels, which
  the earlier cap of 32 refused outright. Matchers are ANDed exact equality
  predicates; absence is different from an empty value.
- Retention defaults to 30 days. Lateness defaults to zero and follows the
  newest timestamp of the series. Both must be whole milliseconds. Ingest
  rejects expired samples and samples behind the persisted frontier.
  `MaxBlockSpan` defaults to one day and caps blocks while packing; it does not
  force a quiet sparse head to produce small blocks on a timer.
- An entire admitted `Ingest` call is atomic, including new series and postings.
  No input is silently dropped. Errors are returned and rejected batches counted.
  A refusal that belongs to one series, such as an invalid kind, a sample behind
  the cutoff or the frontier, or a suspended or conflicting series, is a
  `*SeriesError` carrying that series' labels; `errors.Is` still matches its
  cause, so the caller can send the call again without that series. A cancelled
  call or a failing file keeps its own error.
- Defaults are 100000 currently registered series, 4096 head samples per series, 10000
  input samples / 4 MiB accounted batch data, and 64 maintenance series per pass.
  Replacements do not consume another head slot. A full head returns `ErrLimit`;
  callers can run maintenance and retry if a safe prefix exists.
  `MaxHeadBytes` defaults to 256 KiB and bounds the entire encoded tail. The
  configurable ceiling is 16 MiB and one million head samples; these are separate
  limits. Raising one does not raise the other. Compression never seals unsafe
  points just to fit a head budget.
- Query defaults: 1000 matched series, 4096 inspected groups/decoded blocks,
  16 MiB fetched data, 1048576 decoded samples, 100000 output samples, and a
  five-second snapshot timeout. Byte accounting includes labels, directories,
  external bodies and encoded head bytes; it is not total process RSS or disk I/O.
  Store options can change capacities; a request can only lower its limits.
- `MaxReaders` sizes the reader connection pool and defaults to 2. Queries beyond
  that queue rather than fail, so raising it widens read concurrency; it also
  lengthens the write-ahead log, because a checkpoint cannot advance past the
  oldest open snapshot. One writer connection is not configurable.
- `MaxConcurrentReads` defaults to `MaxReaders` and holds a slot through decode,
  after the snapshot has ended. `MaxConcurrentIngest` defaults to one and holds
  a slot from before batch preparation through commit. The slots bound active
  work per Store; callers waiting for one can cancel through their context.
  `NewWorkBudget(bytes)` creates an optional shared active-work reservation;
  pass the same pointer as `Options.SharedBudget` to every Store that should
  participate. Read weight comes from effective query limits; ingest and
  maintenance use conservative input and staging estimates. An operation too
  large for the budget returns `ErrLimit`, and a queued operation honors its
  context. Queued operations are granted in arrival order, so a large one is
  not passed over by smaller ones that arrive after it.
  `WorkBudget.Usage()` exposes current and peak reservations. The
  budget is not a process RSS ceiling: it excludes runtime, SQLite caches,
  caller-owned inputs and retained returned results.
- A narrow query can spend 240 decoded samples on one returned point. Resource
  errors never silently truncate the result or choose another resolution.
  A packed head charges the whole compressed tail to `PayloadBytes`, checks
  its CRC and chunk metadata, then decodes and charges only chunks overlapping
  the requested range to `DecodedSamples`.
- `Close(ctx)` stops admission and waits for admitted work. Canceling that wait
  does not cancel cleanup; another Close can wait for its completion.
- Statistics are per opened handle. External series deletion, regex matchers
  and a network API are not implemented. Retention can
  reclaim an empty series and free a cardinality slot; `Maintenance.ReclaimedSeries`
  reports that pass and `Stats.ReclaimedSeries` counts this handle's committed
  reclamations. Re-registering the same labels starts a new lifecycle: kind and
  sealed frontier belong to the new registration. Samples below the current
  retention cutoff remain inadmissible.

## Binary details are behind one boundary

The label index uses a dictionary: each exact `(name,value)` pair is stored
once, and postings contain only its id and a series id. A series row stores the same dictionary
ids as a gap-coded blob rather than its canonical label text, and a query
rebuilds the labels from one dictionary read for the whole match. Matching,
identity checks still compare the full ids behind a digest. Retention decrements
posting counts and removes dictionary pairs after their last series disappears.
Each dictionary pair also stores an exact posting count, advanced in the
registration transaction. Multi-label reads use those
counts to choose the shortest posting list without scanning a capped prefix.

The packed head uses independently decodable existing-codec chunks of at most
240 samples, a version and a checksum bound to the series. A batch rewrites one
tail per series, never one SQL row per sample. The head remains mutable across
restart. Its bounded encode/merge runs inside the writer transaction; sealed
group encoding still runs outside it. This trades CPU for fewer SQL mutations.
Queries copy packed bytes in the snapshot and decode after ending it.

The schema is one script, `migrations/0001_schema.sql`; nothing written by an
earlier revision is read. A series' identity is `@` and the base64 SHA-256 of its
canonical labels, unique in the index; a digest match resolves a series only
after its stored label ids equal the batch's.

`group.go` owns slot addressing; `directory.go` owns the directory format,
versions two and three. Think of a directory as a small list of block descriptions:
first value, statistics and where the body lives. The shared clock owns its
time bounds and sample counts. `binary.go` contains checked binary reads and
the bounded metadata compressor, so parsing checks are not scattered through SQL.

A group has at most 32 blocks, each with at most 240 samples. Constant values
have no body. Bodies up to 16 bytes are inline; others are payload rows. The allocation mask records
which slots originally received rows. Retention changes only the live mask,
so deleting slot 0 never shifts the address of slot 1. Payload identifiers are
reserved durably and never recycled by this writer.

The checksum binds a directory to its series, time bounds, clock id and payload
addressing. Each nonempty encoded body also verifies its head and clock bytes.
First values and summaries use exact storage, not SQLite REAL. Summary values
may be derived from the first value only when their IEEE bits agree. Directory
versions two and three have golden readers and a fuzz target.

Clock deduplication checks both SHA256 and actual bytes. Retention releases one
clock owner per deleted group, not per deleted microblock. Query-local caching
holds at most eight decoded clock descriptors and never outlives its snapshot;
there is no unbounded global object cache.

Grid scales range from 0 to 15. The first nonconstant sealed block searches;
later groups inherit a persisted hint and keep the ordinary codec if smaller.
Residual encodings include zero, sign bitmap, packed bitmap, fixed width,
sparse positions and optional zstd varints. The hint is write optimization only;
each block says everything its decoder needs. Automatic retraining is not built.

## Scope of this implementation

This is the first durable path, reusing `codec/` for ordinary values and adding
the measured representations around it. The mutable head is now packed;
shared payload objects and clock-pattern-only deduplication are not implemented.
It must not be advertised with the prototype's
0.3608 B/sample density without a real-engine run under a stated workload.

Packing starts when at least 240 safe head points are present, and publishes up
to 32 microblocks. The time-span cap can split them sooner; clock objects are
bounded to 64 KiB. Publication merges preceding groups with no more live blocks
than the incoming group, up to those same bounds. Version-three directories
keep explicit payload addresses, so merging rewrites metadata without relocating
values. Expired slots are omitted; the new clock ownership, directory and sealed
prefix commit together. `SealedBlocks` counts newly sealed blocks only. Existing
groups merge when later samples seal; there is no background sweep over quiet
series. Byte-aware page-packing policies remain future work.

Tests cover reopen, bitwise values, label matching, atomic rejection, budgets,
watermark boundaries, stale compaction, rollback after payload/head writes,
concurrent ingestion/packing/expiry/read snapshots, partial retention, series
reactivation, shared clock ownership and close. A subprocess also commits and
exits without Close before reopening. Packed heads add a fixed reader vector,
fuzz target and byte-capacity gates. An opt-in 1000-round
incremental test covers lateness, replacements, retention and raw reads and
reports WAL size. Broad WAL/RSS/p99 work, mid-commit process termination and
power-loss testing remain acceptance work.

`TestCorpusThroughPublicStore`, enabled by `TINYSTORE_JSONL`, ingests a normalized
corpus through the public API, runs maintenance, closes the database, reopens it
and checks every returned sample bit. That is the replacement path for prototype
benchmarks; corpus files and competitor dependencies remain outside the engine.
