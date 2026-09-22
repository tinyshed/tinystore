# Shared SQLite mechanics, separate engines

This is an assessment of the supplied `deep-research-kv.md`,
`deep-research-kv-2.md` and `tinystore-kv-blobstore-design.md`, against the engine
on 22 September 2026. Their proposed APIs, thresholds and implementation tasks
are proposals, not shipped features or additional authorization to build them.
Metrics remains the current implementation priority.

## What is already common

`internal/sqlite` already owns file opening, checked migrations, WAL, a single
immediate writer, query-only bounded readers, cancellation and transactions.
Prepared reads now also live there: 32 retained statements per connection, with
FIFO eviction and no result cache. The metrics vocabulary stays in `metrics/`.
KV and blob metadata can reuse this code without changing the metrics API.

Shared mechanics do not require one database file or one global writer queue.
A future engine should get its own file, migration identity and resource
budgets. Blob collection must not consume the metrics writer. A facade may open
the handles, but must not promise atomic writes across independent files.

The writer still executes uncached SQL. Bounded statement reuse on that path is
a concrete next profiling candidate. Bounded batching of maintenance commits is
another: keep value encoding outside the writer and cap staging memory and lock
duration. Increasing a reader pool does not solve either cost.

## Corrections to carry forward

- `SQLITE_PREPARE_PERSISTENT` is not a statement cache. Reuse the prepared
  program on its owning connection. The current implementation does not need a
  direct-driver or unsafe fast path.
- The measured `LIMIT CAST(? AS INTEGER)` benefit concerns parameter-dependent
  recompilation, not a change in the SQL text or merely the Go argument's type.
  It does not justify casting every parameter.
- One transaction amortizes durable commits. A multi-row INSERT is a separate
  optimization; many prepared INSERTs inside one transaction also share a commit.
- `database/sql` is not a cgo driver. TinyStore already uses it with pure-Go
  modernc, so “database/sql versus pure Go” is not a valid comparison axis.
- Keep `synchronous=FULL`. A proposed NORMAL profile changes durability and must
  be explicit; it is not a free optimization or a bounded “last few seconds” loss.
- Do not disable automatic checkpointing before implementing and measuring its
  replacement. Cache memory is multiplied by connections; proposed 32 MiB
  caches, mmap and temporary in-memory structures are not free defaults.
- Do not copy `PRIMARY KEY(rowid)` or universal overflow thresholds. Compare
  actual schemas and `dbstat` objects at the actual page size. Binary prefix
  scans need the bytewise next prefix, including empty/all-0xff boundaries.

The later design document corrects several of these issues itself. Its reported
latency aspirations are not measurements of a TinyStore KV or blob engine.

## A feasible blob path

The useful distinction is a mutable logical object (`bucket/key`, metadata,
version) pointing to immutable physical bytes. Start with a complete, small
cycle: Put, Open/ReadAt, Stat, Delete, reopen and recovery. SQLite-inline small
objects and dedicated files for larger objects are enough to test those
semantics. Packed segments can then be compared against them; an adaptive
placement threshold needs measurements, not a copied 100 KiB constant.

For external payloads, force complete bytes to durable storage before committing
their metadata reference. Crashes may leave orphan bytes, never a committed
reference to unfinished data. Temp-file publication, final-name durability and
recovery need platform-specific tests. A SQLite transaction alone cannot make
an arbitrary filesystem append atomic with its metadata update.

A stream captures metadata and acquires a file/segment-generation lease before
ending its short SQLite snapshot. It then reads the external bytes without a
database transaction. Collection may retire a generation, but cannot remove it
until readers release their leases. Acquiring the lease must be coordinated with
retirement; just returning a path after a SELECT leaves a deletion race.

This reuses the mechanics and the ownership principles already exercised by
metrics clocks. It does not reuse 240-sample codecs, counter summaries or metric
retention semantics. Blob framing, streaming, leases, external-file recovery and
collection belong to the future blob engine. Gateways, signed URLs and optional
crypto integrations must not enlarge the metrics consumer's mandatory module
graph. No placeholder packages or new dependencies are introduced here.

## Remaining metrics work, in practical order

| mechanism | current state / next gate |
|---|---|
| packed heads, dictionary ids, shared clocks, constant/change/grid values, compact summaries, inline bodies | implemented |
| bounded prepared reads and incremental group merging | implemented; see the dated reports, including the point-read trade |
| aggregate queries over existing summaries | unbuilt; define exact boundary and counter semantics, decode only clipped edges |
| writer SQL reuse and maintenance batching | unbuilt; profile and compare durable throughput, staging memory, WAL and writer latency |
| digest as binary rather than base64 text | unbuilt; measure remaining registry/index bytes and preserve full-label collision checking |
| shared Huffman tables | research-only; roughly 4% of value payload on both measured corpora, with extra ownership/read costs |
| sibling prediction with bit corrections | research-only; real-corpus gain much smaller than TSBS, dependency and retention costs remain unpaid |

The earlier research checklist is not a current backlog: several entries there
already shipped. Another codec should follow a per-field and per-object census,
not an attempt to reproduce a prototype's total without its workload conditions.
