# Records: one model for logs and events

The design the records engine is to be built to. `records/` does not follow it
yet: it stores `slog` records as rows with JSON attributes. The prototype is
`spike/record_v2_*`; every figure below comes from a dated report that names
its commit and command:

- [v2 codec, layout and search](reports/record-v2-2026-09-25.md), on synthetic
  fixtures and one public control;
- [production container logs](reports/record-docker-logs-2026-09-25.md), 1.32
  million real lines;
- [sealing, the head, late records, id blooms and memory](reports/record-sealing-2026-09-25.md).

The first prototype, with its whole-segment envelope and exhaustive encoder, is
history in [its reports](reports/record-reconstruction-2026-09-25.md); nothing
reads its format and nothing will.

## The record

A record has an event time, a stream, an event name, an optional level and
body, optional trace and span ids, a context and attributes. An application's
log line and a browser's click are the same model; a body need not yield a
template before it can be stored.

- **Time** is a signed nanosecond instant.
- **Stream** is a namespace the application names, such as a service or a
  container; never the product of its metadata. Session and trace ids do not
  create streams, writers or caches.
- **Context** describes the producer or session and **attributes** this
  occurrence; which is which is the caller's statement, and whether it saves
  bytes is the encoder's business.
- **Values are kept exactly.** Attributes and context keep their order,
  duplicate keys and each value's JSON spelling: `1.2300`, `-0`, big integers,
  nested objects. An absent attribute, `null` and `""` are three things, and so
  are an absent body and an empty one. A body is arbitrary bytes.
- **Adapters map, the store does not guess.** `slog` is the first adapter.
  Others map a JSON line's time, level and message into the record instead of
  leaving them inside it. The root imports no engine; an append API belongs to
  the handle `records.Open` returns.

## Order

**A record's order is its event time.** A segment stores one stream's records
sorted by event time; equal times keep their arrival order. `Append` order is
not otherwise observable: concurrent producers make it noise, and keeping it
cost 1.0 byte per record on the frontend fixture. A query returns records in
event-time order, merging the segments that overlap its range. A consumer that
follows the store reads segments in publication order, each in event-time
order, through a `(segment, row)` cursor. A record that arrives after its
segment was sealed appears in a later one, so storage holds no global
event-time order; query results do. The metrics watermark and `ErrTooOld` do
not apply.

```text
arrival   .300 buy    .100 menu   .200 save
stored    .100 menu   .200 save   .300 buy      time gaps +100 +100, not −200 +100
```

## Storage

**What a query may skip is its own row.** SQL reads a blob column whole, its
overflow chain included, so a byte range inside a larger blob is not a
selective read. A segment row holds what its blocks share; each block of at
most 1024 records is a row of its own, decodable with that segment row and
nothing else.

```text
segments       id | stream | first_at | last_at | count | body: names, shapes, contexts, text sample
blocks         id | segment | first_at | last_at | count | levels | body: column directory, columns
               index (first_at, last_at, levels)
block_traces   block | bloom over trace ids
block_filters  key | block | bloom over one id-like attribute        primary key (key, block)
```

Every row carries a CRC-32. `records.db` uses 1 KiB pages: SQLite leaves a
large row's remainder, up to 4061 bytes of a 4 KiB page, on a leaf page, and
7 KB blocks left a tenth of the table empty.

**A head collects, a segment seals.** The head keeps one row per stream and
flush, the records under zstd; a segment of two records costs more than the
rows. A stream seals at 16,384 records, 4 MiB of input, or when its oldest
waiting record is an hour old. On production logs sealing after an hour costs
9 % of the file and after a minute triples it. Sealing writes the segment, its
blocks and filters and deletes the head rows in one transaction, as metrics
does.

**Late records have a head of their own.** What arrives more than a minute
behind its batch's median goes to a late head of the same stream and seals the
same way. Without it, one record in a hundred arriving ten minutes late made
one-second reads read thirteen times as many blocks; with it, under three
times.

**A segment in flight reserves 24 MiB.** Encoding allocates three to five
times a segment's input and decoding four to six, and the input is at most
4 MiB.

## Encoding

**A shape is presence and ordered keys.** A record's shape names which of
level, body, trace, span and context it has and its attribute keys in order;
attribute columns are keyed by name and occurrence, so duplicate keys keep
their places. Past 64 shapes or 1024 attribute columns in a segment, a record
keeps its attributes as one serialized value.

**Contexts are a per-segment dictionary.** Distinct contexts are stored once in
the segment row, as columns grouped by their keys; a block stores context ids.

**A column is written the cheapest exact way, chosen by computed size, not by
compressing candidates.**

- Integers pick a transform (none, delta, linear trend), a base and common
  divisor, and a packer (bit width, radix words, Rice, FSE for small
  alphabets), or a dictionary of at most 256 values, or a dominant value with
  exceptions. Every transform wraps on overflow, and so does its inverse.
- A value column types JSON fragments: integers, with exceptions when one in
  eight is not; quoted integers; quoted UUIDs and lowercase hex as bytes; text
  otherwise, as a dictionary when half the values repeat.
- Text of one length keeps a length column of one width and no bits; text
  without newlines is stored newline-separated; any other text keeps lengths.
  A text blob goes through zstd once, and not at all when its bytes look random.
- A segment whose bodies hold at least 256 KiB keeps a 64 KiB sample of them;
  its blocks' text compresses against the sample.
- A column may be a recipe, `prefix + earlier column + suffix`, or an affine
  function of an earlier column, with exact exceptions. It is tried only when
  the first 8 to 16 rows agree, only against one of the eight columns before it,
  and only against a column that is not itself predicted. Bodies come last so
  they can reference attributes.

```text
user_id  92831              route  "/users/92831"   → recipe "/users/ + user_id + "
x        100007             y      300028           → affine y = 3x + 7
sorted times 1000 1250 1250 1900 → delta 250 0 650 → gcd 50 → 5 0 13 → rice k=2
```

## Search

- **Time and level** prune through the covering index; a block of 1024
  frontend records spans about 1.3 s.
- **An attribute** is found by decoding its one column in the blocks whose
  segments have the key, then rebuilding only the matching rows.
- **An id-like attribute**, short JSON strings nine in ten distinct, gets a
  bloom per block, 10 bits a distinct value. Numbers get none: a range asks for
  them.
- **A trace id** gets a bloom per block.
- **A context value** is found in the segment dictionaries, then in the
  context-id column.

## Bounds

| Object | Limit |
|---|---:|
| Segment | 16,384 records, 4 MiB of input |
| Block | 1024 records, 256 KiB of input |
| Fields per context or attribute object | 128 |
| Shapes per segment before attributes are serialized | 64 |
| Attribute columns per segment | 1024 |
| Context cells per segment | 2^20 |
| Integer dictionary | 256 values |
| Text sample | 64 KiB, when bodies hold 256 KiB |
| Expanded text per block | 4 MiB |
| Prediction | eight columns back, one level deep |
| Head | an hour of age; a minute behind is late |
| Memory reservation | 24 MiB per segment in flight |

Counts, lengths, references, radix words, Rice streams, FSE expansion, text
expansion and checksums are validated on decode.

## What was measured

| | Result |
|---|---|
| frontend fixture, one million records | 7.85 B/record in the SQLite file; its time-order floor is 7.74 |
| production logs, 1.32 million records | 20.09 B/record in the file, blooms included |
| structured services, as `slog` records | 18.99 B/record, 15.84 of it a distinct random request id per record |
| text from third-party software | about zstd over the same lines, 19.28 against 19.64 |
| encoder, one CPU | 405,000 to 480,000 frontend records/s; 950 before |
| decoder, one CPU | about 1.5 million records/s |
| one second of one million records | 2 of 977 blocks read |
| one request id among 1.32 million | 11 blocks read, 21 ms |

## Open

- Text: templates with typed variables, a line's own timestamp first; one
  zstd frame per segment shows about 2.5 bytes a record that independent
  blocks leave.
- Input: a continuation rule for multi-line records; adapters for pino, logfmt,
  glog and log4j lines.
- Storage: a merge of a stream's small sealed segments; an index of attribute
  keys per segment; a bound on the time-index scan with late blocks present.
- Encoding: a store-level context registry, per-context numeric state, nested
  JSON decomposition.
- The engine itself: head recovery, retention by segment, concurrent readers,
  the memory reservation, the handler.
