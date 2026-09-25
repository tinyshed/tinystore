# Records: one model for logs and events

Research direction, not a new public API. `records/` still stores `slog` records
as rows with JSON attributes. The executable prototype is in
`spike/record_*_test.go`; its measurements and limits are in
[the 25 September report](reports/record-events-2026-09-25.md).
The [reconstruction follow-up](reports/record-reconstruction-2026-09-25.md)
meets a fixed frontend density target with optional deeper candidates; it also
records workloads that remain much larger.

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
values actually present. Arrival order is a shape-ID sequence, separate from
event time; duplicate and decreasing timestamps are preserved. No metrics
watermark or `ErrTooOld` rule is inherited.

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
