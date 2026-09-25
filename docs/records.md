# Records: one model for logs and events

Research direction, not a new public API. `records/` still stores `slog` records
as rows with JSON attributes. The executable prototype is in
`spike/record_*_test.go`; its measurements and limits are in
[the 25 September report](reports/record-events-2026-09-25.md).
The [reconstruction follow-up](reports/record-reconstruction-2026-09-25.md)
meets a fixed frontend density target with optional deeper candidates; it also
records workloads that remain much larger.

The [v2 round](reports/record-v2-2026-09-25.md) settles order, context scope and
the SQLite layout, and replaces the exhaustive encoder; `spike/record_v2_*` is
its prototype. [Order and layout](#order-and-layout) states those decisions; the
sections after it describe the first prototype, whose candidate families v2
keeps only where a sample of rows says they pay.

## Order and layout

**A record's order is its event time.** A segment stores one stream's records
sorted by event time; records with equal times keep their arrival order.
`Append` order between different times is not observable: concurrent producers
make it noise, and on the frontend fixture keeping it cost 1.0 byte per record.
A query returns records in event-time order, merging the segments that overlap
its range. A consumer that follows the store reads segments in publication
order, each in event-time order, through a `(segment, row)` cursor. A record
that arrives after its segment was sealed appears in a later segment, so
storage holds no global event-time order; query results do. The metrics
watermark and `ErrTooOld` do not apply. An ingest sequence stored beside time
order would restore arrival order at the cost of most of the byte saved.

```text
arrival   .300 buy    .100 menu   .200 save
stored    .100 menu   .200 save   .300 buy      time gaps +100 +100, not −200 +100
```

**A segment's shared part and each block are separate rows.** SQLite reads a
blob column whole, overflow chain included, so a byte range inside a larger
blob is not a selective read. The segment row holds the stream, event names,
record shapes and the context dictionary; a block row holds at most 1024
records as a column directory, columns and a CRC-32, with its time range and
level mask in a covering index; trace ids get a bloom filter row per block.
Nothing is compressed across blocks.

```text
segments  id | stream | first_at | last_at | count | body: names, shapes, contexts
blocks    id | segment | first_at | last_at | count | levels | body: columns
          index (first_at, last_at, levels)
block_traces  block | bloom, 10 bits a distinct trace id
```

**Pages are sized for rows larger than a page.** SQLite keeps a large row's
remainder, up to 4061 bytes of a 4 KiB page, on a leaf page; with 7 KB blocks
a tenth of the table was empty. `records.db` uses 1 KiB pages, which bound the
remainder to 989 bytes.

**Contexts stay per segment until a corpus needs more.** A store-level
registry would save a session's context in every later segment it appears in,
at the cost of references across segments and a reclamation rule; the
per-segment dictionary cost 0.54 bytes per frontend record.

**A head collects, a segment seals.** The head keeps one row per stream and
flush, rows under zstd; a v2 segment of two records costs more than the rows. A
stream seals at 16,384 records, 4 MiB, or when its oldest waiting record is an
hour old: on production logs sealing after an hour costs 9 % of the file and
after a minute triples it ([report](reports/record-sealing-2026-09-25.md)).
Records more than a minute behind their batch's median go to a late head of the
same stream, so that one late record does not stretch a block over minutes;
without it one record in a hundred, ten minutes late, made one-second reads
read thirteen times as many blocks.

**Id-like attributes are found through blooms.** A block writes a bloom filter,
10 bits a distinct value, for each attribute column of short JSON strings of
which nine in ten are distinct, into `block_filters (key, block, bloom)`.
Numbers get none: a range asks for them.

**A segment in flight reserves 24 MiB.** Encoding allocates three to five
times a segment's input and decoding four to six; the input is limited to
4 MiB.

**A column is written the cheapest exact way, chosen by computed size.** An
integer column picks a transform (none, delta, linear trend), a base and common
divisor, and a packer (bit width, radix words, Rice, FSE for small alphabets)
by counting bits, not by compressing candidates; only text blobs go through
zstd, once. A column may be a recipe (`prefix + earlier column + suffix`) or an
affine function of an earlier column with exact exceptions, tried only when the
first 8 to 16 rows agree and only against a column that is not itself
predicted. Text without newlines is stored newline-separated, without a length
column. A segment whose bodies hold at least 256 KiB keeps a 64 KiB sample of
them in its row, and its blocks' text compresses against the sample.

**Real logs are decided by what programs write.** On production container logs
([report](reports/record-docker-logs-2026-09-25.md)) structured services cost
17.75 bytes per record as `slog` records, 15.84 of them a distinct random
request id per record; text from third-party software costs about what zstd
does over the same lines. Text templates with typed timestamps, a continuation
rule for multi-line records and adapters that map time, level and message are
the open work for such logs.

## Logical input

A record has event time, stream, event name, optional level and body, optional
trace and span identifiers, context and event attributes. An application log
and a browser click are instances of this same model. A string body does not
have to yield a template before it can be stored.

`stream` is an application-supplied namespace, not the Cartesian product of
every metadata value. Session identifiers and trace identifiers must not
silently create streams, writers, or permanent caches. Context describes the
producer or session; attributes describe this occurrence. Whether that
distinction saves bytes is an encoding decision, not a caller's obligation to
register immutable objects with the store.

Native adapters and a separate server can normalize input to this model. The
root remains a library that imports no engine; a future append API belongs to
the handle returned by `records.Open`, with `slog.Handler` as an adapter. This
round adds neither a public append API nor a server, OTLP support or SDKs.

The prototype accepts this versioned JSON envelope as one language-independent
input, and also constructs the same internal events directly:

```json
{
  "version": 1,
  "time": "2026-09-25T12:00:00.123Z",
  "stream": "frontend",
  "name": "ui.click",
  "context": {
    "browser": "Chrome",
    "browser_version": "154",
    "session_id": "9cbaf3d1-0c27-47bc-8fed-cb6e0763b9b2"
  },
  "attrs": {
    "element": "buy-button",
    "url": "/checkout",
    "viewport_w": 1920,
    "viewport_h": 1080
  }
}
```

Time is RFC3339 with optional fractions on the wire, an exact signed nanosecond
instant internally. Integers in metadata never pass through `float64`.
Unknown or duplicate envelope keys are refused. Field order and the original
JSON value fragments in context and attributes are preserved, including large
numbers, decimal spelling, nested objects, arrays and duplicate field names.
Those duplicate field names are not collapsed into a Go map. An absent
attribute, `null`, and an empty string remain different. An absent body and a
present empty body also remain different. Native bodies retain arbitrary
bytes; the JSON envelope follows JSON string semantics.

The prototype's optional trace/span strings decode from hex to 16/8 binary
bytes. Hex letter case is not identity. Its body is a string, and its optional
level is a signed integer. Observed time, severity text, arbitrary typed
bodies, native IEEE-754 values, automatic source-format detection and a full
OpenTelemetry mapping are outside this projection. It is not an OTLP protocol.

## Bounded shapes and columns

A shape describes ordered context/attribute keys and which standard optional
fields exist. A record's shape ID already supplies their presence information;
the encoder does not also write nulls or a redundant bitmap per attribute.
Changing a field's value type is allowed: a column carries its own formats.

Time, stream and name have common columns. Other columns contain only the
values actually present. In the first prototype arrival order was a shape-ID
sequence, separate from event time, with duplicate and decreasing timestamps
preserved; v2 stores event-time order instead, see [Order and layout](#order-and-layout).

Each block compares four layouts: whole contexts versus context fields, and
columns shared by field key versus scoped to stream, name and shape. The
scoped variant may have multiple physical descriptors for the same logical
shape. A large number of scopes cannot create unbounded state: the candidate
is declined when a cap is reached, leaving the common or row representation.

Each column compares length-prefixed original values, a local string
dictionary, and typed scalar streams. Scalars include exact signed/unsigned
integers, fixed-point decimal text, lower/upper-case hex and UUIDs; other
spellings retain their original bytes. Numerical streams compare absolute and
modular ZigZag delta representations, with varints or minimum-relative bit
packing. Constant packed values need no per-value bits. Candidates include
their descriptors and zstd framing when compared.

Metadata and columns have separate bounded frames. Equal columns may reference
an earlier column after full encoded-content and count comparison. A complete
normalized row block under zstd is always a candidate, so an independent
adaptive block cannot exceed that baseline's complete encoded size. This is
not a guarantee about SQLite page allocation or a larger-window compressor.

## Reconstruction with exact exceptions

Two more column candidates depend on an earlier column of the same length:

1. A mapping from the source value to the first corresponding target value.
   The decoder encounters source keys in the same order, so the mapping stores
   target values without repeating source keys. Exceptions store positions and
   exact replacement values.
2. A string recipe: prefix, source text with surrounding quotes removed when
   present, and suffix. Every nonmatching position is an explicit exception.

For example, a producer that repeats the same identifier can yield:

```text
user_id = 92831
route   = "/users/92831"
url     = "https://example.test/users/92831"
body    = "loaded user 92831"

stored: user_id + three recipes + exception positions/values
```

`derived_fields` and `TestRecordPredictionsKeepExceptions` exercise this case,
including nonmatching messages. Bodies are ordered after attributes in the
physical column directory so they can depend on them. References only point
backwards, so decoding cannot form a cycle. Search is limited to eight earlier
columns, mappings to 256 keys, and recipes to a small bounded set of seeds.
The encoder compares the full dependency representation against the existing
column candidate. It never omits an independent value merely because a
relationship seems plausible.

## Contexts shared across microblocks

A segment compares independently encoded microblocks with a shared immutable
context dictionary plus stripped microblocks and per-record context IDs. It
contains at most eight microblocks by default, or sixteen in the newer bounded
scope experiment. The dictionary itself compares a string
column of serialized contexts with normalized context rows under the same
column codec. This lets repeated metadata inside different contexts share
their structure too.

The segment contains every dependency needed to decode it. Nothing refers to
a previous segment, future input, a producer's memory, or a global dictionary.
A changed context is another exact value. The dictionary is discarded with
its segment. High-cardinality contexts can make sharing lose or exceed a cap;
independent microblocks remain a candidate.

The prototype decodes the whole segment and checks its whole checksum. Its
microblocks remain separate encoded objects, but selective query reads and
partial retention have not been built or measured. A future engine must solve
dictionary ownership and per-object integrity before claiming those benefits.

The follow-up also compares per-key numeric changes, affine predictions with
exact residuals, integer trends/common factors, sparse and radix representations,
fixed-byte/byte-plane columns, and exact nested-JSON reconstruction. JSON
scaffolding retains original key order, punctuation and whitespace; scalar
spelling remains available. A stable context grouping can reuse the context-ID
sequence to restore arrival order. A final compression envelope is optional,
uses a 256 KiB window and refuses recursive envelopes on decode. It adds a
whole-segment expansion before the existing decoder; selective reads remain
unimplemented. Candidate flags in the spike keep the previous encoder available
for comparisons on exactly the same input.

## Bounds and persistence boundary

| Object | Prototype limit |
|---|---:|
| Microblock | 1024 events, 256 KiB of serialized normalized rows |
| Fields per context or attribute object | 128 |
| Admission cells per microblock, including common fields | 65,536 |
| Physical shape descriptors / columns | 128 / 1024 |
| Scalar formats per column | 16 |
| Inflated streams / expanded column text per block | 4 MiB each |
| Segment | 8 microblocks by default; optional 16, at most 16,384 events / 4 MiB of original rows |
| Shared context dictionary | 2048 entries / 256 KiB before compression |
| JSON decomposition | 128 leaves/value, 16,384 leaf cells, four column nesting levels |
| Per-key numerical state | 2048 keys |
| Final envelope expansion | 4 MiB; another final envelope inside it is refused |

Counts, lengths, references, packed padding, reconstructions and checksums are
validated on decode. The byte and object-count caps do not claim a measured
heap/RSS ceiling: Go objects and compression workspaces also cost memory.

The streaming batcher owns the queued values and seals on count, bytes, cells
or explicit flush. It does not inspect future blocks. A segment may inspect
its bounded set of at most sixteen microblocks before being sealed. This larger
dictionary scope trades buffering and retention granularity for density; the
benchmark also gives plain zstd that larger scope.

The harness persists sealed objects in SQLite for file accounting. It is not
a durable ingestion engine: there is no head, recovery protocol, retention,
query index beyond the harness's time bounds, Store memory reservation, or
nonblocking handler integration. Promotion requires those gates plus real
structured-event corpora, selective reads, CPU, allocation and peak-memory
measurements. Name IDs and small dictionaries are not free search indexes.
