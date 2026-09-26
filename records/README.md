# records

An application's logs and events, in `records.db` inside a `tinystore.Store`:
written through `log/slog` or appended as records, read back by time and by
what they hold, and followed in the order they were sealed. The design and
the measurements behind it are [docs/records.md](../docs/records.md).

```go
logs, err := records.Open(ctx, store, records.Options{Retention: 14 * 24 * time.Hour})

logger := slog.New(slog.NewMultiHandler(console, logs.Handler("notes")))
logger.With("request_id", id).Warn("slow request", "route", "/notes", "ms", 1200)

err = logs.Append(ctx, records.Record{
	At: time.Now(), Stream: "web", Name: "click",
	Context: []records.Field{records.String("session", sid)},
	Attrs:   []records.Field{records.String("element", "buy"), records.Int("x", 812)},
})

page, err := logs.Read(ctx, records.Query{
	From: time.Now().Add(-time.Hour), Streams: []string{"notes"},
	MinLevel: new(slog.LevelWarn), Newest: true, Limit: 100,
})

batch, err := logs.Follow(ctx, cursor, 1000) // sealed segments, from a cursor the caller keeps
```

## The record

- A record has a time, a stream the application names, an event name, an
  optional level and body, optional trace and span ids, a context (who
  produced it) and attributes (what happened). A nil `Level` or `Body` is
  absent, and an empty body is not an absent one.
- A value is JSON and keeps its spelling: `1.2300`, `-0`, a big integer and a
  nested object come back byte for byte, and so do the order of fields and
  repeated keys. `String`, `Int`, `Float`, `Bool` and `JSON` build fields.
- Times are nanoseconds since 1970 in the file and come back in UTC; levels
  are slog's, as 32-bit integers.

## Contracts

- `Append` writes every record or none, in one transaction, and a `Read` sees
  them as soon as it returns. A record the format cannot keep is refused as a
  `*RecordError` naming it: no stream or name, a value that is not JSON, a
  time past what nanoseconds hold, more than 128 fields, more than 256 KiB. So
  is a record outside the store's window: older than `Options.Retention`
  (`tinystore.ErrTooOld`), which no read would return, or more than
  `Options.ClockSkew` (ten minutes) ahead of the store's clock
  (`tinystore.ErrTooNew`), which would hold its segment past retention. An
  application taking events from clocks it does not own gives them the time
  it received them and keeps theirs as an attribute.
- `Handler(stream)` never blocks its caller. Lines wait in a buffer
  (`Options.Buffer`, 1024) and are written as one `Append` every
  `Options.Flush` (a second), on `Close`, or on `Flush(ctx)` in a Manual store;
  a line that does not fit the buffer or the format, or a batch whose write
  failed, or a line outside the store's window, is dropped and counted in
  `Stats`. Lines of the records engine itself
  are refused. A line is a record named `log`: its message is the body, the
  attributes of `logger.With` its context, the call's its attributes, a
  group's keys written `group.key`, values spelled as `slog.JSONHandler`
  spells them.
- A record's order is its time. A segment stores one stream's records in time
  order, equal times in the order they arrived; nothing else of the arrival
  order is kept. A record more than a minute behind the newest record its
  stream has shown, in its batch or waiting in the head, or behind the store's
  clock when that is earlier, goes to the stream's late head, so that it does
  not stretch the blocks of its neighbours; a producer whose clock runs ahead
  of the store's does not send its neighbours there.
- Records wait in a durable head until their head holds a segment's worth
  (16,384 records or 4 MiB) or its oldest row is `Options.SealAge` old (an
  hour). A longer `SealAge` trades how soon `Follow` sees a sparse stream's
  records for fewer, larger segments: on the production corpus 18.93 bytes a
  record at an hour, 17.04 at six, 16.67 at a day, 16.55 in full segments.
  `Maintain`, every minute unless the store is Manual, seals them: the
  segment, its blocks, filters and keys are written and the head rows deleted
  in one transaction, so a reader finds each record once.
- `Read` returns one page, oldest first or newest first, from one snapshot,
  decoded after the snapshot is released. A page never splits a timestamp; it
  ends early when its `Limit` (1000, at most 10000) or its `Budget` (the
  blocks, bytes and records it may fetch) runs out, and says so with `More`;
  `Next` is the query for the rest. A plain range stops fetching once the
  blocks taken hold about a page, so paging costs what the page holds. More
  records at one time than a page holds is `tinystore.ErrLimit`.
- A query's conditions all hold: one of `Streams`, one of `Names`, a level of
  `MinLevel` or above (a record without a level does not match), the
  `TraceID`, and each of `Attrs` and `Context` by key and exact JSON spelling.
  The time index and level masks pick candidate blocks; a segment's keys, a
  bloom over each block's trace ids and a bloom over each id-like attribute
  (short strings nearly all distinct) skip the blocks that cannot match.
- `Follow` reads sealed segments in the order they were sealed, each in time
  order, from a `(segment, row)` cursor the caller keeps; `Batch.Expired`
  counts the segments retention removed before the cursor reached them. A
  record reaches `Follow` only once it is sealed.
- Retention removes whole segments whose newest record is older than
  `Options.Retention` (fourteen days) by the store's clock, and head rows
  likewise; a read never returns an older record, even from a segment only
  partly past it.
- Every operation reserves its weight in the store's memory: an append its
  input, a seal 24 MiB for a segment in flight, a read its budget's bytes, a
  decoded block and a page of records. Without `tinystore.Options.Memory` the
  engine still bounds what runs at once: two reads or follows, decoding
  included, and two appends; the rest wait for a slot, and a caller that stops
  waiting leaves. One `Append` carries at most a segment's input, 4 MiB, or is
  refused with `tinystore.ErrLimit`; the handler writes a larger flush in
  pieces.
- A changed byte is refused with `tinystore.ErrCorrupt` naming the invariant
  it broke: every row carries a CRC-32 and every count and length is bounded
  before anything is allocated.
- A row that no longer reads is lost, and says so once. `Maintain` logs a
  damaged head row at Error the first time it meets it, counts it in
  `Maintenance.Damaged`, leaves it and seals the rest of its head. A `Read` or
  a `Follow` over it fails with a `*DamageError` naming the head row, or the
  segment whose row or block it is, and the times it held. `Damaged` lists
  what this handle has met, and `Drop` removes one: a head row alone, a
  segment whole, since a `Follow` cursor counts a segment's rows through its
  blocks. It refuses a row that still reads with `tinystore.ErrConflict`, and
  `Batch.Expired` counts a dropped segment as it counts retention's.

## What it does not do yet

No text templates beyond the times a line spells, no full-text search, no
merging of a stream's small segments, no adapters beyond `slog`. Text
compresses per block, without the per-segment sample the research measured;
see docs/records.md for what that costs.
