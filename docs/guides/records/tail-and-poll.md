# Tail and poll

Two ways to read events as they are written, for two jobs. `tail` is for a
person watching now: a live page of errors, `tinystore logs -f`. It skips
what it cannot keep up with and says so. `poll` is for a copy that must be
whole: shipping every event to a warehouse once, from a cursor the program
keeps.

| You want                                      | Use    | Order                      | Loses                                                |
|-----------------------------------------------|--------|----------------------------|------------------------------------------------------|
| what is written from now on, as it is written | `tail` | by time                    | what a slow reader falls behind, counted in `missed` |
| every event once, from where you stopped      | `poll` | a stream's batch at a time | only what `keep` removed first, counted in `expired` |

## Tail

```ts
for await (const { events, missed } of store.events().tail({ streams: ['api'], minLevel: 'warn' })) {
	if (missed > 0) socket.send(JSON.stringify({ skipped: missed }))
	for (const event of events) socket.send(JSON.stringify(event))
}
```

```python
async for batch in store.events().tail(streams=["api"], min_level="warn"):
    if batch.missed > 0:
        await socket.send_json({"skipped": batch.missed})
    for event in batch.events:
        await socket.send_json(event)
```

```go
for batch, err := range events.Tail(ctx, records.Query{Streams: []string{"api"}, MinLevel: slog.LevelWarn}) {
	if err != nil {
		return err
	}
	if batch.Missed > 0 {
		send(map[string]int{"skipped": batch.Missed})
	}
	for _, event := range batch.Events {
		send(event)
	}
}
```

```rust
let mut tail = store.all_events()?.list().streams(["api"]).min_level(Level::WARN).tail()?;
while let Some(batch) = tail.next_within(Duration::from_secs(30))? {
    if batch.missed > 0 {
        send_skipped(batch.missed);
    }
    batch.events.iter().for_each(send);
}
```

- A tail gives the events written after the call that made it: an `append`'s
  at once, a logger's lines when its logger writes them, within a second.
- It takes a query's `streams`, `names`, `minLevel`, `traceId`, `where` and
  `search`, and no range, order or limit.
- It is live, not complete. The store holds its last megabyte of events for
  its tails; a reader slower than the writers skips what left it, and the
  batch after says how many in `missed`.
- It ends when its loop is left, and when the store closes.

## Poll

```ts
let cursor = await shipping.get('cursor')
for (;;) {
	const batch = await store.events().poll(cursor) // { events, cursor, expired }
	if (batch.events.length === 0) break
	if (batch.expired > 0) log.warn('events expired before their copy', { count: batch.expired })
	await warehouse.insert(batch.events)
	cursor = batch.cursor
	await shipping.set('cursor', cursor) // after the copy is safe
}
```

```python
cursor = await shipping.get("cursor")
while True:
    batch = await store.events().poll(cursor)  # PollBatch(events, cursor, expired)
    if not batch.events:
        break
    if batch.expired > 0:
        log.warning("events expired before their copy", count=batch.expired)
    await warehouse.insert(batch.events)
    cursor = batch.cursor
    await shipping.set("cursor", cursor)
```

```go
cursor, _, err := shipping.Get(ctx, "cursor")
for {
	batch, err := events.Poll(ctx, cursor) // records.Batch{Events, Cursor, Expired}
	if err != nil || len(batch.Events) == 0 {
		return err
	}
	if err := warehouse.Insert(ctx, batch.Events); err != nil {
		return err
	}
	cursor = batch.Cursor
	if err := shipping.Set(ctx, "cursor", cursor); err != nil {
		return err
	}
}
```

```rust
let mut cursor: Option<String> = shipping.get("cursor")?;
loop {
    let batch = store.all_events()?.poll(cursor.as_deref())?;
    if batch.events.is_empty() {
        break;
    }
    warehouse.insert(&batch.events)?;
    cursor = Some(batch.cursor.clone());
    shipping.set("cursor", &batch.cursor)?;
}
```

- `poll` gives every event once, a batch a call, from a cursor the program
  keeps: text. Without one it starts at the oldest event kept.
- It returns at once: with no event past the cursor, an empty batch and the
  cursor it was given. Run the loop from a [repeating
  job](../jobs/schedules.md), every minute.
- Keep the cursor after the copy is safe: a crash between the two reads the
  batch again and loses none. Make the copy's insert idempotent.
- The order is the store's, not the events': a batch at a time of one
  stream, in the order of its times. Sort where the events land.
- An event reaches `poll` within an hour of being written, sooner in a busy
  stream, since `poll` reads packed segments.
- `expired` counts the events `keep` removed before the cursor reached them:
  more than none means the copy has a hole.
- A batch is 1,000 events unless `limit` says. Only `store.events()` polls.

## See also

- [Records](README.md): the engine's overview.
- [Reading](reading.md): the same events by a query.
