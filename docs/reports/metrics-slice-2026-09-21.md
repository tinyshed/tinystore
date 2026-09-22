# Metrics first, with room for Records and application state

Status: architecture proposal; the first implementation now exists and its
actual scope is recorded in [implementation-2026-09-21.md](implementation-2026-09-21.md).
This document is not an API compatibility commitment. It answers an external
architecture proposal for Metrics, Records, KV and a SQL mapper, which is not
part of this repository; the passages it argues with are quoted where they
matter, and section numbers below refer to it. The repository's current status
at the time of this proposal was codec plus prototypes; use the implementation
report and metrics/README.md for current behavior.

## Keep the four surfaces; narrow what is shared

Metrics, Records, KV and ordinary application SQL are a reasonable product
direction. The implementation order in section 35 of that architecture
should change: a mapper, builder and joins are not prerequisites for metrics.

| Boundary | Responsibility | Excludes |
|---|---|---|
| SQLite file handle | pools, connection setup, transactions, migrations, snapshot-bound BLOB reads, checkpoint/backup mechanics | series, clocks, dictionaries, retention meaning |
| Binary primitives | bounded varints, bit packing, checksums, byte compression | object ids, SQL, ownership, metric summaries |
| Metrics | registry, head, frontier, groups, clock ownership, compaction, range planning, retention | record schemas and application tables |
| Records, later | identity, event time/order, typed fields, dictionaries, text bodies, record retention | metric-series lateness and counter rules |
| KV, later | byte keys/values, versions, expiry, scans | telemetry compression |
| SQL helpers, optional | application queries and mapping | engine migrations and hot-path representation |

Do not start with `PutObject(kind, body)` and a universal descriptor/refcount
manager. Clocks and dictionaries resemble each other physically, but their
deduplication rules, lifetime and access paths have not both been built. Keep
clock ownership in Metrics. Extract a reusable transaction-local primitive
when Records supplies a second concrete implementation.

Separate **encoding** from **location**. `Constant`, `Changes`, `Scaled`, `XOR`
and `Raw` are value encodings; `None`, `Inline` and `External` are locations.
A clock or dictionary reference is an additional dependency, not another value
encoding. Metrics and Records should have their own descriptors.

## Package shape and file ownership

```text
codec/                   existing public sample codec; retain compatibility
internal/sqlite/         file handle, transactions, migrations, BLOB access
metrics/                 public API; private implementation in ordinary files
  types.go               input, requests, limits and results
  open.go                composition and ordered shutdown
  registry.go            canonical labels, postings and cardinality admission
  head.go                durable recent samples, duplicate policy and versions
  packing.go             safe prefix, encoder choice and group publication
  groups.go              descriptors, payload allocation and clock ownership
  query.go               snapshots, budgets and exact range filtering
  retention.go           logical cutoff and bounded physical cleanup
internal/encoding/       only genuinely reused byte primitives, when needed

records/  kv/  query/     future surfaces; no empty scaffolding now
```

Keeping initial Metrics implementation files in one package avoids a public
facade/internal-model import cycle and premature interfaces. Split packages
when a concrete dependency boundary emerges. Metrics must not import Records,
KV or the optional SQL mapper. Preserve the root dependency budget.

Start with an independently opened metrics handle, conceptually
`metrics.Open(path, Options)`. A future `tinystore.Open(directory)` facade may
compose explicit handles. It must not create/open every database or make a
broken records file prevent metrics from opening.

Files remain `metrics.db`, `records.db` and an application-owned `app.db`.
Each file has its own writer, migration history and failure boundary. Limits
must also account for the total resources of the embedding process; opening
three engines does not grant each an unlimited independent codec/cache budget.

Raw application SQL must not expose Metrics/Records internal tables as a
supported mutation API. SQL and KV may share app.db and a transaction type
bound to that exact file. Cross-file atomic transactions and an atomic backup
of all files are not promised. Migrations for TinyStore-owned tables and
application tables need distinct namespaces and ownership.

## The first working slice

The slice is **Open → Ingest → Range → seal → reopen → expire**, on one real
metrics file. It is not a SQL wrapper, and not just another codec benchmark.

Its public surface needs options/limits, exact sample ingestion, raw range
queries by label matchers, local statistics and close. Maintenance must be
deterministically runnable in tests; a timer is only a trigger for the same
bounded work. Server mode and automatic resolution selection are later work.

Choose and test these contracts before publishing the API:

- Timestamps are explicit signed millisecond ticks for Metrics. Raw values are
  IEEE float bits in every durable path, including the head and descriptors.
  SQLite REAL is not the authority for reconstructing a sample's first value.
- Labels have deterministic canonical bytes, reject duplicate names and have
  size/count/cardinality limits. Implement postings in this slice. The benchmark
  registry's JSON string plus unique index does not implement label matching.
- A metric series has at most one live sample per timestamp. Proposed ingest
  policy: last occurrence in an input batch wins in the mutable head; a later
  committed batch may replace it until sealing. State this explicitly rather
  than inheriting a corpus converter's deduplication policy accidentally.
- An admitted bounded batch is atomic. Invalid, expired or sealed input returns
  a defined error without silent partial writes. If callers later need partial
  acceptance, introduce explicit per-item results rather than changing this rule.
- Classify a batch against the pre-batch series state; update max_seen_ts at the
  end. Otherwise a backlog can reject its own earlier samples. Every head
  mutation, including replacement and expiry, changes its version.
- Queries return exactly the requested unexpired raw samples in `[from,to)`.
  `MaxPoints` alone is a resource ceiling, not permission to downsample.
  Separate budgets cover matched series, fetched bytes, descriptors, decoded
  samples, output and snapshot duration.
- Retention clips reads immediately at a captured cutoff. Physical deletion and
  returning pages to the OS are separate operations. Quiet head samples expire
  without forced singleton block creation.

Use an exact row head first, with its persisted frontier/version and due index.
The packed mutable tail remains behind that private boundary: its steady-state
WAL and dense/lateness behavior have not yet been measured. This choice does not
change the immutable format and must not leak through the public API.

## Immutable format: keep the measured structure, define its guarantees

Use the combined prototype as the target layout: shared clocks owning temporal
descriptors, small independent value microblocks, grouped metadata, constant
and inline representations, separate external payloads, and allocation/live
masks. For an initial format, 240 samples, 32 descriptor slots and a 16-byte
inline threshold are measured starting choices, not universal optima. Also
bound group bytes and event-time span. Never make the benchmark size a promise
for the durable head, arbitrary workloads or steady-state file size.

Retain the existing codec reader. Give the new directory/clock/envelope formats
explicit versions and golden vectors before release; keep schema and package
versions independent. Implement the proven ordinary codec plus constant/change
representations first within that structure. Grid mode can remain an optional
candidate; the file and public API must not depend on its search heuristics.

Raw values and timestamps round-trip bitwise. Aggregation is a separate contract:
floating addition is not associative, and preserving samples does not establish
that grouped sums equal sequential raw sums bit for bit. Define NaN, infinities,
signed zero, reduction order and empty buckets before exposing aggregates.
Keep gauge and counter summaries distinct. Counter increase and resets must
come from sorted raw samples; a gauge-only prototype does not prove counters.

The first query path may expose only raw ranges while writing summaries needed
by later aggregation. When summaries are used, a block must be entirely live,
inside the effective range and inside one requested bucket. Every cut bucket
boundary can require decoding; this is not limited to the two ends of the query.

## Atomic publication, snapshots and reclamation

There must be one visible Metrics write path for publication:

```text
read bounded eligible head prefix and its version
encode outside the writer
BEGIN IMMEDIATE
  verify version; otherwise bounded retry
  acquire/increment exact clock references
  reserve and insert consecutive external payload ids
  write sealed group and protected directory
  remove exactly the selected head keys
  advance sealed_before and version
  update next due work
COMMIT
```

Select a safe prefix using a watermark following that series' event time. The
prefix ends strictly before the watermark, and only publication moves the
persisted frontier. Do not seal unsafe points when a buffer or group fills.
Do not wait for several series to share a clock: sharing is an optimization
of independently publishable immutable objects. Hash matches require complete
equality checks; digest index and ownership updates belong to the same file/tx.

Payload address remains `first_id + rank(allocation_mask, ordinal)`. Allocation
is immutable. Physical expiry changes live bits, removes covered payloads,
updates/invalidates affected parent summaries and reseals metadata atomically.
Clock ownership belongs to the group, not to the number of its live slots;
release the reference when the last slot/group disappears. Reactivating a series
with NULL next_gc_ts must restore its due-index entry.

The example Kernel's unscoped `ReadBlobRange(ctx, ref, dst)` is insufficient.
All query discovery and fetches must use one explicit read transaction:

```text
snapshot: registry/postings → groups/head → clocks → selected payload bytes
end snapshot
decode/filter/merge outside transaction
```

Directory inspection necessary to select payload ranges may happen in that
snapshot, under bounded bytes/time. Do not fetch descriptors in one snapshot
and payloads in another: GC and id reuse can invalidate that combination.
Do not retain a snapshot for the caller's arbitrary iterator lifetime. The
initial bounded request fetches a bounded plan or fails with a resource error;
it does not silently split the operation into inconsistent snapshots.

Reserve contiguous ids within the writer transaction, using durable allocation
state that cannot alias live or cached objects. Clock caches are bounded and
keyed by immutable identity/generation, not by a recyclable bare rowid.
Any future id reuse needs a proof covering outstanding readers and caches.

## Changes needed in the Records/KV/SQL draft

**Records timestamps may repeat.** Define record identity and tie-breaking order
separately from event time, and distinguish event time from observed/ingest time.
Do not inherit Metrics' duplicate replacement or sealed-series rejection rule.
Old records can be appended in another segment and merged at query time if that
is the chosen Records contract. Reuse low-level delta tools, not the current
strictly-increasing metrics block format wholesale.

**Records exactness needs a definition.** Absent, null and empty differ; typed
integers must not pass through float64. Preserve original body bytes when
template extraction cannot reproduce them exactly, including spacing/escaping.
These concerns stay in Records, not in a universal scalar/schema layer created
before Records exists.

**KV CAS has two missing rules.** Expired entries are absent to CAS and
PutIfAbsent, not only Get. Use one defined time observation for each operation
or transaction. A version must not repeat after delete/recreate or expiry:
otherwise stale CAS can modify a different incarnation of a key (ABA).
Use a non-repeating revision scheme, with a defined signed-SQLite-integer bound
or an opaque generation token. The example uint64 API and INTEGER column alone
do not settle overflow. TTL preservation/reset behavior must be explicit.

**KV's WITHOUT ROWID choice is provisional.** Arbitrary large values have a
different B-tree overflow cost from small metadata keys. Benchmark small and
large values before fixing that schema. No telemetry payload manager is needed
to implement ordinary KV in app.db.

**SQL mapping is optional application tooling.** Cached reflection plans and a
typed builder can be useful for Dashbin, but should not become the metrics
storage executor or a prerequisite for this slice. Raw SQL/transactions remain
available for app.db. An iterator that holds a read transaction is not a bounded
WAL guarantee merely because rows are streamed. Bound mapper caches and expose
close/cancellation behavior; document where raw access bypasses managed limits.

## Acceptance before calling this an engine

1. Ingest, replace, query, reopen and expire samples without loss or duplication;
   ordered/backfilled batches, retention edges and exact float cases included.
2. Readers racing ingest, sealing and GC see one consistent result. Crash/fault
   injection at transaction boundaries leaves no leaked ownership, payloads or
   advanced frontier without the corresponding blocks.
3. Partial expiry preserves allocation addresses, remaining microblock summaries
   and parent summaries. Last-owner deletion and series reactivation are tested.
4. Fuzz all new parsers; enforce decoded-directory, clock and value limits before
   allocations. Bitwise round trips over NAB, TSBS and Alibaba remain gates.
5. Measure steady ingest plus reads and retention: WAL, RSS, writer hold times,
   cold narrow reads and p95/p99. Bulk-file density remains a separate report.

The first implementation proceeds through file/registry/head/raw query, then
atomic sealing into the target layout, then retention/restart/concurrency gates.
Records, KV, SQL convenience APIs and a network wrapper follow independently.
No dormant engine packages or universal object framework are required to start.
