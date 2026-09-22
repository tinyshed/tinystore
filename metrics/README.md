# The first metrics slice

This package now opens a real file, registers series, commits samples, reads
exact ranges, seals eligible history, reopens it, and expires old data.
It has no dependency on Records, KV, a SQL mapper or the experiment harness.

## Follow one series

The runnable [example](example_test.go) writes 241 samples of `cpu{host="web-1"}`,
seals the first 240, closes the store and reads across the sealed/head boundary
after reopening. No binary-format knowledge is needed to use or follow it.

1. **Open** (`open.go`) validates limits, opens `internal/sqlite` and applies
   checked SQL migrations. A metrics file has its own application identity.
2. **Ingest** (`ingest.go`) validates and sorts a bounded batch, resolves its
   series through `registry.go`, merges incoming points into its packed head,
   then advances its state. `head.go` owns reading and replacing that tail.
   Everything commits together. Last input wins for duplicate mutable timestamps.
3. **Read** (`query.go`) resolves exact label matches through postings and copies
   the required directories, payloads and encoded heads from one snapshot. It ends
   that snapshot before decoding. An error returns no partial answer.
4. **Maintain** (`packing.go`) first expires due work, then reads a safe head
   prefix, encodes it outside the writer, and publishes only if its version is
   unchanged. Publication writes payloads, removes those exact head samples and
   moves the sealed frontier in the same transaction.
5. **Expire** (`retention.go`) removes expired head points and whole expired
   microblocks. A partly expired group keeps its original payload addresses.
   Empty series keep their registration and count toward the cardinality limit.

The value encoder in `values.go` compares the ordinary codec with constant,
change-event and decimal-grid candidates. `clocks.go` stores time separately
and shares identical clock groups, with ownership maintained in the publication
transaction. None of these choices appear in the public request types.

For example, 241 timestamps `0..240` with zero lateness give a watermark of 240.
Only timestamps strictly below it may seal. The first 240 form one block; sample
240 remains in the head. A quiet series does not seal merely because time passes.

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

## Contracts and defaults

- Timestamps are signed Unix milliseconds; MaxInt64 is reserved as an exclusive
  upper bound. Values, including signed zero and NaN payloads, are stored as
  exact bits. An invalid counter value remains readable; its summary is marked
  unusable. No aggregate API is exposed yet.
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
- Defaults are 100000 registered series, 4096 head samples per series, 10000
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
- A narrow query can spend 240 decoded samples on one returned point. Resource
  errors never silently truncate the result or choose another resolution.
  A query touching a packed head currently decodes that whole bounded head;
  every decoded point is charged even when only one falls in the requested range.
- `Close(ctx)` stops admission and waits for admitted work. Canceling that wait
  does not cancel cleanup; another Close can wait for its completion.
- Statistics are per opened handle. Aggregate query semantics, external series
  deletion, regex matchers and a network API are not implemented.

## Binary details are behind one boundary

The label index uses a dictionary: each exact `(name,value)` pair is stored
once, and postings contain only its id and a series id. Migration 0005 converts
existing postings transactionally. A series row then stores the same dictionary
ids as a gap-coded blob rather than its canonical label text, and a query
rebuilds the labels from one dictionary read for the whole match. Matching,
identity and cardinality semantics do not change: the ids are the labels, so
comparing them is the full equality check a digest match still requires.

The packed head uses independently decodable existing-codec chunks of at most
240 samples, a version and a checksum bound to the series. A batch rewrites one
tail per series, never one SQL row per sample. The head remains mutable across
restart. Its bounded encode/merge runs inside the writer transaction; sealed
group encoding still runs outside it. This trades CPU for fewer SQL mutations.
Queries copy packed bytes in the snapshot and decode after ending it.

Schema migrations add packed heads without discarding legacy rows. A legacy head
is read as before and replaced atomically on its next mutation. Canonical labels
now use JSON pairs, with a short SHA256-based identity in the unique index. Full
label equality is checked on lookup, including when upgrading an older identity.

`groups.go` owns slot addressing and the legacy reader; `directory.go` owns the
compact directory. Think of a directory as a small list of block descriptions:
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
may be derived from the first value only when their IEEE bits agree. Both old
and compact directories have golden readers. New parsers have a fuzz target.

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
bounded to 64 KiB. A live workload may produce one-block groups. Merging already
sealed groups and byte-aware page-packing policies remain future work. Payload
formats and schema migrations preserve the old directory reader.

Tests cover reopen, bitwise values, label matching, atomic rejection, budgets,
watermark boundaries, stale compaction, rollback after payload/head writes,
concurrent ingestion/packing/expiry/read snapshots, partial retention, series
reactivation, shared clock ownership and close. A subprocess also commits and
exits without Close before reopening. Packed heads add a fixed reader vector,
fuzz target, legacy conversion and byte-capacity gates. An opt-in 1000-round
incremental test covers lateness, replacements, retention and raw reads and
reports WAL size. Broad WAL/RSS/p99 work, mid-commit process termination and
power-loss testing remain acceptance work.

`TestCorpusThroughPublicStore`, enabled by `TINYSTORE_JSONL`, ingests a normalized
corpus through the public API, runs maintenance, closes the database, reopens it
and checks every returned sample bit. That is the replacement path for prototype
benchmarks; corpus files and competitor dependencies remain outside the engine.
