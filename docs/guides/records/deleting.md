# Deleting events

`delete` takes the events a query finds out of the Records engine's streams,
for when a person asks for their data to go. It removes the events and
whatever the store kept only for them, and answers how many it deleted. Use
it for a request to erase, not for clean-up: a stream's `keep` removes old
events by itself.

```ts
const deleted = await store.events().delete({
	streams: ['activity', 'client-errors', 'key-calls', 'oauth-steps'],
	where: { userId: 97516 },
}) // 412
const gone = await store.events('activity').delete({ where: { userId: [97516, 97517] }, to: new Date('2026-06-01') })
```

```python
deleted = await store.events().delete(streams=["activity", "client-errors"], where={"userId": 97516})  # 412
gone = await store.events("activity").delete(where={"userId": [97516, 97517]}, to=datetime(2026, 6, 1, tzinfo=UTC))
```

```go
events, err := records.OpenAllEvents(store)
deleted, err := events.Delete(ctx, records.Query{
	Streams: []string{"activity", "client-errors"},
	Where:   records.Where{"userId": 97516},
}) // 412
activity, err := records.OpenEvents(store, "activity")
gone, err := activity.Delete(ctx, records.Query{Where: records.Where{"userId": []int{97516, 97517}}})
```

```rust
let deleted = store.all_events()?.list().streams(["activity", "client-errors"]).filter("userId", &97516).delete()?;
let gone = store.events("activity").open()?.list().filter("userId", &[97516, 97517]).delete()?; // u64
```

## What the query takes

A delete takes the same query as a [read](reading.md): `streams` on every
stream's handle, `names`, a range with `since` or `from` and `to`,
`minLevel`, `traceId`, and `where` with its lists, `{ not }` and
`{ prefix }`. A key matches in the event's fields or in its context, so a
logger's `with({ userId })` is found too.

- `where` is required. A delete without one would remove every event of its
  range, and is refused `invalid`.
- `search` is refused: a phrase can match more than one person's events.
- `newest`, `limit` and `after` are refused: a delete takes every event the
  query finds, not a page of them.

## What is deleted

Every event appended before the call that the query finds, waiting to be
packed or packed already, and with it what the store kept only for those
events (contract REC-20):

- a context only they had, and the keys and counts that `fields` and
  `fieldNames` give;
- their values in the filters a read uses to skip blocks;
- the bytes they lay in: the store writes zeros over what it frees and
  empties the WAL into the file, so that `records.db` and its WAL keep none
  of them once the delete returns.

An event appended while the delete runs may or may not be deleted; running
the delete again catches it.

## Cursors, order and ids

A delete moves nothing a program holds (contract REC-21). A poll from a
cursor taken before it gives every other packed event once, and never the
deleted ones. Events that share a time keep their order, those an old
release packed included.

An event's `id` stays held until the stream's `keep` passes the event's
time, so a producer that sends the event again writes nothing. Do not put
personal data in an id.

## It cannot be undone, and it can be repeated

A delete writes each part of a stream in its own commit, since a year of a
stream fits in no single transaction. A read during a delete can still find
the events it has not reached yet; once it returns, none are left. A crash, a
cancel or a closed store in the middle leaves what it reached deleted and
fails the call. Run it again to finish: it deletes what is left and counts
only that. A program that must not lose the request runs it as a
[job](../jobs/README.md).

Packing and merging wait for a delete to finish; appends and reads do not.
Deleting someone who was active all year rewrites much of the stream, which
takes minutes on a large file.

## What a delete cannot reach

- **A backup taken before it.** A restore brings the events back: delete
  again after restoring. Keep the list of requests, for example as an audit
  event, to replay it.
- **A backup taken while it runs** may hold some of them.
- **Another node.** Each node keeps its own `records.db`: delete on each.
- **What an earlier release freed.** Space a release before deleting freed
  inside a page, as packing frees an event's first copy, keeps its bytes
  until SQLite reuses it; and a file v0.1.0 made larger than 8 MiB, which
  never gives free pages back, keeps those too.
- **A database's events not yet moved.** An event appended inside a
  database's transaction with `db.events` waits in that database until it
  moves to its stream. A delete moves what waits in the databases open in the
  process first; a database not open keeps its events until it opens.

A part of a stream that no longer reads fails the delete `corrupt` and names
it: [Repair](repair.md) lists and drops it.
