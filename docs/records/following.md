# Following

Process records in order, from a cursor that you keep: export them to another
system, build a summary, or feed an analytics job. A follower reads records in
batches, and it never skips or repeats a record, even across restarts.

## Read the next batch

```ts
let cursor = await loadCursor() // undefined the first time: start from the oldest record

const batch = await store.records.follow(cursor, 1000)
await exportRecords(batch.records)
await saveCursor(batch.cursor)
```

```python
cursor = await load_cursor()  # None the first time: start from the oldest record

records, cursor, expired = await store.records.follow(cursor, 1000)
await export_records(records)
await save_cursor(cursor)
```

```go
cursor := loadCursor() // the zero cursor starts from the oldest record

batch, err := logs.Follow(ctx, cursor, 1000)
if err != nil {
	return err
}
exportRecords(batch.Records)
saveCursor(batch.Next)
```

A cursor is two numbers, a segment and a row. Save it after you have processed
a batch, for example in a [KV bucket](../kv/README.md) or in the same SQL
transaction as your results. After a restart, continue from the saved cursor.

## What a follower sees

Records are first collected in a head, and sealed into segments of up to
16,384 records, or after an hour at the latest. A follower reads sealed
segments in the order they were sealed, and the records of each segment in
time order. So a follower can be up to an hour behind a quiet stream. A
`scan` sees every record at once, including those that are not sealed yet.

A record that arrives late appears in a later segment, and the follower still
gets it exactly once. Small segments are merged in the background, and a
cursor keeps its place across merges.

## Records that expired

If a follower falls behind further than the retention, the oldest segments are
deleted before it reaches them. The batch tells you how many segments it
missed, in `expired`, so that you can report the gap instead of losing records
silently.

## Limits and defaults

|           |                                      |
|-----------|--------------------------------------|
| A segment | 16,384 records or 4 MiB, or one hour |
| Retention | 14 days                              |

## See also

- [Reading](reading.md): read records by time instead of following them.
- [records/README.md](../../records/README.md): the full contract of
  `follow`.
