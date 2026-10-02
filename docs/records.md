# Records: one model for logs and events

The design of the records engine. `records/` is built to it, with the one
difference [below](#what-the-engine-leaves-out); its contract is
[records/README.md](../records/README.md), and
[the engine report](https://github.com/tinyshed/research/blob/main/tinystore/reports/records-engine-2026-09-25.md) measures it on the
corpora the research used. The prototype is research's `spike/record_v2_*`; every figure
below comes from a dated report that names its commit and command:

- [v2 codec, layout and search](https://github.com/tinyshed/research/blob/main/tinystore/reports/record-v2-2026-09-25.md), on synthetic
  fixtures and one public control;
- [production container logs](https://github.com/tinyshed/research/blob/main/tinystore/reports/record-docker-logs-2026-09-25.md), 1.32
  million real lines;
- [sealing, the head, late records, id blooms and memory](https://github.com/tinyshed/research/blob/main/tinystore/reports/record-sealing-2026-09-25.md).

The first prototype, with its whole-segment envelope and exhaustive encoder, is
history in [its reports](https://github.com/tinyshed/research/blob/main/tinystore/reports/record-reconstruction-2026-09-25.md); nothing
reads its format and nothing will.

## The public surface

```go
logs, err := records.Open(ctx, store, records.Options{})

logger := slog.New(logs.Handler("notes"))                 // never blocks: queued, flushed each second
cmd.Stdout = logs.Lines("worker")                          // another program's lines, stack traces joined
err = logs.Append(ctx, records.Record{At: t, Stream: "web", Name: "click",
	Context: []records.Field{records.String("session", sid)},
	Attrs:   []records.Field{records.String("element", "buy")}})     // one transaction, readable on return

page, err := logs.Scan(ctx, records.Query{From: from, To: to,     // [From, To), a zero end is open; or Since
	Streams: []string{"web"}, Names: []string{"click"}, MinLevel: new(slog.LevelWarn),
	TraceID: trace, Attrs: []records.Field{records.String("requestId", id)},
	Newest: true, Limit: 100, Budget: records.Budget{Blocks: 64}})
next, err := logs.Scan(ctx, page.Next)                            // when page.More
for record, err := range logs.All(ctx, records.Query{Since: time.Hour}) { … }  // a page at a time
resets, err := logs.Scan(ctx, records.Query{Since: time.Hour, Search: "connection reset"})
```

A search finds a record whose body, or name, holds its text, the case
ignored. Nothing indexes the text yet: a search reads every record the rest of
the query leaves, block by block within the range, so over much text it ends a
page early at the budget, and the page's `Next` goes on. A word index of each
block, which would let a search pass blocks by, waits for a measurement of
what it costs the file.

```go
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
- **Another program's lines keep their bytes.** A writer of `Lines` makes a
  record of each line at the time it arrived and joins the lines of a stack
  trace, a traceback or a JSON value printed over lines. A record named `log`
  keeps the text byte for byte; one named `json` keeps a JSON object's
  fields and one named `logfmt` a logfmt line's pairs, when they spell the
  line again, so that `Attrs` finds them. The level comes from where pino,
  logfmt, glog, Redis, log4j, zerolog and Postgres write it, in colour or
  not, so that `MinLevel` finds their errors through the blocks' level masks.
  A line's own time stays in its text, or in its time pair, where the codec
  keeps it at a few bits.

```text
level=info msg="slow request" ms=1200   → logfmt: level "info", msg "slow request", ms 1200
level=info msg="ok"                     → log: the rule spells ok bare, so the line stays text
\x1b[32mINFO\x1b[0m started              → log, level info: a word in colour counts as one in brackets
```

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
- **Adapters map, the store does not guess.** `slog` and `Lines` are the
  adapters. `Lines` maps a line's level into the record and leaves the rest
  where the line has it, its own time included: a time whose zone a line does
  not say cannot be placed, and the time a line arrived always can. The root
  imports no engine; an append API belongs to the handle `records.Open`
  returns.

## Order

**A record's order is its event time.** A segment stores one stream's records
sorted by event time; equal times keep their arrival order. `Append` order is
not otherwise observable: concurrent producers make it noise, and keeping it
cost 1.0 byte per record on the frontend fixture. A query returns records in
event-time order, merging the segments that overlap its range. A consumer that
follows the store reads segments in publication order, each in event-time
order, through a `(segment, row)` cursor. A record that arrives after its
segment was sealed appears in a later one, so storage holds no global
event-time order; query results do. The metrics watermark does not apply: a
record is never refused for arriving late, only for falling outside the
store's window, older than `Retention` or more than `ClockSkew` ahead of the
store's clock.

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
segments       id | stream | first_at | last_at | count | holder | start | held | input | first_block | last_block
               | body: names, shapes, contexts
segment_keys   segment | kind | key          the event names, attribute keys and context keys it holds
blocks         id | segment | stream | first_at | last_at | span | levels | count | size | body: column directory, columns
               index (span, last_at, first_at, levels, stream, segment, count, size)
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
column a query chooses blocks by. It leads with a block's span, the power of
two its width in nanoseconds stays under, then `last_at`: a read asks it once
for each span the file holds, for the blocks ending between its start and its
end moved on by that width, so it walks the blocks near its range whatever
their widths, not every block ending after its start.

```text
span 29, blocks under half a second: a read of [12:00, 12:01) walks last_at in [12:00, 12:01 + 0.54 s)
span 51, blocks under 26 days:       the same read walks last_at in [12:00, 12:01 + 26 days)
```

A segment's keys and a block's filters are keyed by their owner: a query asks
them about the candidates it already has, and retention deletes a segment's
blocks, filters and keys as the id ranges the segment row names. Segment ids
never repeat, so a consumer's cursor never meets one twice.

**A head collects, a segment seals.** The head keeps one row per stream and
flush, the records under zstd; a segment of two records costs more than the
rows. A stream seals at 16,384 records, 4 MiB of input, or when its oldest
waiting record is an hour old. On production logs sealing after an hour costs
9 % of the file and after a minute triples it. Sealing writes the segment, its
blocks and filters and deletes the head rows in one transaction, as metrics
does.

**Small segments merge, and keep their places.** A segment's id is its place
in the order segments were sealed, the place a `Follow` cursor names. After a
pass seals a stream, its segments holding fewer than a quarter of a
segment's records and input merge four of a size at a time, four whose
records share a power of four, taken in time order so that one after another
their records stay in time order; a record is written again a few times,
not once a seal. The merged segment is written into the row of the lowest
id, and every other member keeps its row as a place: `holder` names the
segment that holds its records now, `start` where they begin among the
holder's, and a cursor at the place, or inside it, finds each record once.
Merging writes the segment, deletes the blocks and keys its members were
made of and turns them into places in one transaction; a holder's places come
after it, and retention and `Drop` remove a holder with its places.

```text
places 1 4 7 9 of a stream, 30 40 35 38 records   → 1 holds 143; 1 4 7 9 start at 0 30 70 105
four such, 143 150 160 140 records                → one holds 593
```

A holder's places lie hours apart in the order segments were sealed, so a
follower far behind reaches them one batch at a time, and each batch needs
the same block. `Follow` keeps the last 4 MiB of the blocks and segment rows
it fetched, and reserves them beside each call's weight; a block's id is
never given twice, even after a merge deletes the newest blocks, and a
holder's row changes only with its first block, so nothing it keeps goes
stale.

**Late records have a head of their own.** What arrives more than ten
seconds behind the newest record its stream showed before it, earlier in the
same batch or waiting on time in its head, or behind the store's clock when
that is earlier, goes to a late head of the same stream and seals the same
way. A record appended alone can be late too; a batch in time order makes
none of its own records late, however long it spans; a producer ahead of the
store's clock does not make its neighbours late. Without a late head, one
record in a hundred arriving ten minutes late made one-second reads read
thirteen times as many blocks; with it, one and a half times.

**What no longer reads is reported once and dropped by hand.** A head row or
a segment whose bytes fail their checksum is logged the first time the engine
meets it and left: sealing goes around a damaged head row, and a read or a
follow over it fails naming it, as metrics fails a read over a series that
does not decode. `Drop` removes it, a head row alone or a segment whole, and
only once it is shown not to read, so that it cannot delete a record by
mistake. What the engine has met is kept in memory: the file is the record of
the damage, and a reopened store finds it again as it meets it.

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
  A text blob goes through zstd once, and not at all when its bytes look random;
  a segment's at the better-compression level, which the corpus's text pays
  for with 44 % more zstd time and 7.5 % fewer bytes, and a head row's, which
  lives an hour, at the default.
- A time a text spells, a log line's own timestamp first, is kept as its
  distance behind its record's time in the unit its fraction counts, with the
  layout that spells it back, up to four a value; the text keeps the rest. Its
  layout is found among eight patterns: ISO and RFC 3339, Go's log, glog with
  and without its year, Redis, syslog and access logs. A date no calendar has
  and a leap second stay text, and so does a column where fewer than one value
  in eight spells one.
- Integers that count time, as a JSON line's own `time` does, are kept the same
  way, in seconds, milliseconds, microseconds or nanoseconds, when that costs
  fewer bytes than the integers.
- Bodies come last in a block.

```text
sorted times 1000 1250 1250 1900 → delta 250 0 650 → gcd 50 → 5 0 13 → 4 bits a value
record 00:47:32.101187376, "I20260923 00:47:32.100929 raft.cpp:60] ok"
       → "I raft.cpp:60] ok", "YMD h:m:s.6" 1 byte in, 258 µs behind
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
| Times one text value spells, kept apart | 4, each within 64 bytes of the last |
| Layouts of those times in one column | 64 |
| Expanded or copied text per block | 4 MiB |
| Head row | a block's bounds |
| Head | an hour of age; ten seconds behind the stream's newest before it, or the clock, is late |
| Time | from the retention cutoff to `ClockSkew`, ten minutes, past the store's clock |
| Memory reservation | 24 MiB per segment in flight |

Counts, lengths, references, radix words, Rice streams, FSE expansion, text
expansion, stamp layouts and places, and checksums are validated on decode,
and a segment row may decompress or copy at most twice a segment's input.

## What was measured

The prototype, and the engine on the same fixtures and corpus, in the same
kind of container; [the engine report](https://github.com/tinyshed/research/blob/main/tinystore/reports/records-engine-2026-09-25.md)
has the environment and the commands, [the rice round](https://github.com/tinyshed/research/blob/main/tinystore/reports/records-rice-2026-09-26.md)
chose each rice parameter among all 64, [the stamps round](https://github.com/tinyshed/research/blob/main/tinystore/reports/records-stamps-2026-09-26.md)
has the corpus's figures since a line's own time is kept apart,
[the merge round](https://github.com/tinyshed/research/blob/main/tinystore/reports/records-merge-2026-09-26.md) those sealed hourly
since small segments merge, and [the text round](https://github.com/tinyshed/research/blob/main/tinystore/reports/records-text-2026-09-26.md)
those in full segments since a segment's text takes zstd's stronger level.

| | Prototype | Engine |
|---|---|---|
| frontend fixture, one million records, B/record in the file | 7.85; its time-order floor is 7.74 | 7.89 |
| production logs, 1.32 million records, full segments | 20.09, blooms included | 16.24, without the text sample |
| the same, sealed hourly on the corpus's own clock | 21.96 | 16.77, merged; 16.69 at six hours, 16.62 at a day |
| structured services, as `slog` records | 18.99, 15.84 of it a random request id | — |
| text from third-party software | about zstd over the same lines, 19.28 against 19.64 | 15.14 |
| encoder, one CPU | 405,000 to 480,000 frontend records/s | 495,000 to 593,000; 800,000 to 864,000 text |
| decoder, one CPU | about 1.5 million records/s | 1.25 to 1.42 million; 1.73 to 1.89 million text |
| one second of one million records | 2 of 977 blocks read | 1.92 blocks |
| the same, one record in a hundred ten minutes late | 5.12 blocks | 2.93, in flushes of 1024 or 16,384 |
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
segment. The first of them, a line's own time kept apart, has since taken the
corpus to 16.55. [The text round](https://github.com/tinyshed/research/blob/main/tinystore/reports/records-text-2026-09-26.md) measured
the rest: templates with typed numbers cost 1.2 to 2.0 bytes a record more
than zstd a block at a time, and one zstd frame a segment, the most a sample
could take back, 0.48 bytes a record less; the engine keeps its blocks apart
and writes a segment's text at the stronger level instead, the corpus at
16.24. Recipe and affine predictions are left out as well, at no cost on the
production logs.

**Sealing sparse streams sooner than it pays.** `SealAge` trades how soon
`Follow` sees a record for the bytes a sparse stream's small segments cost:
an hour, the default, keeps a follower an hour behind at most. Merging takes
back 2.16 of the 2.38 bytes a record the hour cost the production corpus, each
record written again one and a half times, and the default stays an hour.

## Open

- Measured since: reads beside a writer appending and sealing, in
  [the load round](https://github.com/tinyshed/research/blob/main/tinystore/reports/records-load-2026-09-26.md); not measured on macOS,
  bare Linux or a disk.

- Input: OTLP, and a server taking lines from programs that do not embed the
  store.
- Encoding: a store-level context registry, per-context numeric state, nested
  JSON decomposition.
