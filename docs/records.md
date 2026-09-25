# Records: one model for logs and events

The design of the records engine. `records/` is built to it, with the one
difference [below](#what-the-engine-leaves-out); its contract is
[records/README.md](../records/README.md), and
[the engine report](reports/records-engine-2026-09-25.md) measures it on the
corpora the research used. The prototype is `spike/record_v2_*`; every figure
below comes from a dated report that names its commit and command:

- [v2 codec, layout and search](reports/record-v2-2026-09-25.md), on synthetic
  fixtures and one public control;
- [production container logs](reports/record-docker-logs-2026-09-25.md), 1.32
  million real lines;
- [sealing, the head, late records, id blooms and memory](reports/record-sealing-2026-09-25.md).

The first prototype, with its whole-segment envelope and exhaustive encoder, is
history in [its reports](reports/record-reconstruction-2026-09-25.md); nothing
reads its format and nothing will.

## The public surface

```go
logs, err := records.Open(ctx, store, records.Options{})

logger := slog.New(logs.Handler("notes"))                 // never blocks: queued, flushed each second
err = logs.Append(ctx, records.Record{At: t, Stream: "web", Name: "click",
	Context: []records.Field{records.String("session", sid)},
	Attrs:   []records.Field{records.String("element", "buy")}})     // one transaction, readable on return

page, err := logs.Read(ctx, records.Query{From: from, To: to,     // [From, To), a zero end is open
	Streams: []string{"web"}, Names: []string{"click"}, MinLevel: new(slog.LevelWarn),
	TraceID: trace, Attrs: []records.Field{records.String("requestId", id)},
	Newest: true, Limit: 100, Budget: records.Budget{Blocks: 64}})
next, err := logs.Read(ctx, page.Next)                            // when page.More

batch, err := logs.Follow(ctx, records.Cursor{Segment: s, Row: r}, 1000)
```

- **A page ends where it can prove it is whole.** It never splits a
  timestamp; its limit, its budget, or a plain range that has fetched about a
  page ends it, `More` says so, and `Next` is the query moved past it. Many
  more records at one time than a page holds is `ErrLimit`.
- **A consumer follows what is sealed.** `Follow` reads segments in the order
  they were sealed through a cursor the caller keeps, and says how many
  segments retention removed before the cursor reached them; a record in the
  head waits for its seal, up to `SealAge`.
- **The handler maps, the model does not guess.** A `slog` line is a record
  named `log`, the message its body, `logger.With` its context, the call's
  attributes its attributes.

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
segments       id | stream | first_at | last_at | count | first_block | last_block | body: names, shapes, contexts
segment_keys   segment | kind | key          the event names, attribute keys and context keys it holds
blocks         id | segment | stream | first_at | last_at | levels | count | size | body: column directory, columns
               index (last_at, first_at, levels, stream, segment, count, size)
block_traces   block | bloom over trace ids
block_filters  block | key | bloom over one id-like attribute
heads          id | stream | late | first_at | last_at | levels | count | input | size | written_at | body
head_state     stream | late | count | input | since       a row per head
streams        id | name
```

Every row carries a CRC-32. `records.db` uses 1 KiB pages: SQLite leaves a
large row's remainder, up to 4061 bytes of a 4 KiB page, on a leaf page, and
7 KB blocks left a tenth of the table empty.

**Every lookup starts from the candidates.** The block index covers every
column a query chooses blocks by and leads with `last_at`, so a read of the
recent past walks only recent blocks. A segment's keys and a block's filters
are keyed by their owner: a query asks them about the candidates it already
has, and retention deletes a segment's blocks, filters and keys as the id
ranges the segment row names. Segment ids never repeat, so a consumer's
cursor never meets one twice.

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
- Bodies come last in a block.

```text
sorted times 1000 1250 1250 1900 → delta 250 0 650 → gcd 50 → 5 0 13 → 4 bits a value
```

The prototype also kept, for a segment whose bodies hold at least 256 KiB, a
64 KiB sample its blocks' text compressed against, and wrote a column as a
recipe, `prefix + earlier column + suffix`, or an affine function of an earlier
column, with exact exceptions. The engine leaves both out; what that costs is
[below](#what-the-engine-leaves-out).

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
| Expanded or copied text per block | 4 MiB |
| Head row | a block's bounds |
| Head | an hour of age; a minute behind its batch's median is late |
| Memory reservation | 24 MiB per segment in flight |

Counts, lengths, references, radix words, Rice streams, FSE expansion, text
expansion and checksums are validated on decode, and a segment row may
decompress or copy at most twice a segment's input.

## What was measured

The prototype, and the engine on the same fixtures and corpus, in the same
kind of container; [the engine report](reports/records-engine-2026-09-25.md)
has the environment and the commands.

| | Prototype | Engine |
|---|---|---|
| frontend fixture, one million records, B/record in the file | 7.85; its time-order floor is 7.74 | 7.89 |
| production logs, 1.32 million records, full segments | 20.09, blooms included | 20.90, without the text sample |
| the same, sealed hourly on the corpus's own clock | 21.96 | 22.95; 21.33 at six hours, 21.00 at a day |
| structured services, as `slog` records | 18.99, 15.84 of it a random request id | — |
| text from third-party software | about zstd over the same lines, 19.28 against 19.64 | 20.16 |
| encoder, one CPU | 405,000 to 480,000 frontend records/s | 570,000 to 710,000 |
| decoder, one CPU | about 1.5 million records/s | 1.29 to 1.44 million |
| one second of one million records | 2 of 977 blocks read | 1.92 blocks |
| the same, one record in a hundred ten minutes late | 5.12 blocks | 5.12; 5.26 in flushes of 1024 |
| one request id among 1.32 million | 11 blocks read, 21 ms | 11 blocks, 6 ms |

## What the engine leaves out

**The per-segment text sample.** A segment whose bodies hold 256 KiB kept a
64 KiB sample of them, and its blocks' text compressed against it. The hand-off
left it out for the decoder state it costs a segment and counted its gain at
2.6 % of text records. The engine measures more: without it the production
corpus's text grows from 19.28 to 20.16 bytes a record, and the whole corpus
from 20.09 to 20.90, 0.80 bytes, where the JSON services' bytes are the
prototype's to the hundredth. Decided on 25 September: the engine stays
without it, and text's next lever is templates, not a decoder state per
segment. Recipe and affine predictions are left out as well, at no cost on the
production logs.

**Sealing sparse streams sooner than it pays.** `SealAge` trades how soon
`Follow` sees a record for the bytes a sparse stream's small segments cost:
an hour, the default, keeps a follower an hour behind at most; six hours takes
back four fifths of what the hour costs on the production corpus, and a day
nearly all of it. The default stays an hour; a store that keeps sparse streams
for long and follows them rarely raises it.

## Open

- Text: the per-segment sample back, 0.80 B/record on the production corpus,
  or templates with typed variables, a line's own timestamp first; one zstd
  frame per segment shows about 2.5 bytes a record that independent blocks
  leave.
- Late records: a batch's median is a flush's, and a flush of 1024 frontend
  records separates the late ones slightly worse than the research's segment
  of 16,384: 5.26 blocks a one-second read against 5.12. The head's newest time
  would be a steadier reference.
- Input: a continuation rule for multi-line records; adapters for pino, logfmt,
  glog and log4j lines.
- Storage: a merge of a stream's small sealed segments, which cost the hourly
  replay 2.05 bytes a record over full segments, where a six-hour `SealAge`
  costs 0.43; a merge has to keep a follower's `(segment, row)` cursor valid.
  A bound on the time-index scan when late blocks are wide.
- Encoding: a store-level context registry, per-context numeric state, nested
  JSON decomposition.
- A head whose row no longer reads is logged and left; there is no call to
  drop it, as `metrics.DropSeries` drops a series.
