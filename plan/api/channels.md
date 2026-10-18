# channels: the API book

Messages a program publishes on a named channel and every subscriber that
listens at that moment receives, in memory and at most once. This book is a
proposal of 16 October 2026, not yet accepted: it holds the call sites after
a newcomer check and three outside reviews, and the contract they asked for.
[dx.md](../dx.md) holds the rules it follows, and how Python and Go spell the
calls of TypeScript and Rust; research's
[channels round](https://github.com/tinyshed/research/blob/main/tinystore/reports/channels-2026-10-15.md),
its [follow-up with real receivers](https://github.com/tinyshed/research/blob/main/tinystore/reports/channels-wire-followup-2026-10-15.md)
and the [handoff](https://github.com/tinyshed/research/blob/main/tinystore/reports/channels-locks-handoff-2026-10-15.md)
the evidence and the traps. The Go engine had no channels.

## What a newcomer learns

| Word         | What it is                                                                           |
|--------------|--------------------------------------------------------------------------------------|
| a channel    | a name messages of one type are published on, kept in memory only                    |
| a branch     | whose messages these are, `under(roomId)`, as a bucket's branch is                   |
| a subscriber | a function the store calls with each message published while it listens              |
| missed       | what a subscriber hears when it lost messages: it fell behind, or its connection did |
| a message    | `{ value, branch }`: what was published and the branch it was published under        |

A channel is not a queue and not a log. A message published while nobody
listens is gone; there is no history, no acknowledgement and no retry. Work
that must be done is a job; events to read again are records.

## Publish

```ts
const messages = store.channel<Chat>('messages')
const typing = store.channel<Typing>('typing')    // another type, another channel

await messages.under(roomId).publish({ from: 'ann', text: 'hi' })
await messages.publish(notice)                    // the channel's root
```

```python
messages = store.channel("messages", Chat)
typing = store.channel("typing", Typing)

await messages.under(room_id).publish(Chat(sender="ann", text="hi"))
await messages.publish(notice)
```

```go
messages, err := channels.OpenChannel[Chat](store, "messages")   // *channels.Channel[Chat]
typing, err := channels.OpenChannel[Typing](store, "typing")

err = messages.Under(roomID).Publish(ctx, Chat{From: "ann", Text: "hi"})
err = messages.Publish(ctx, notice)
```

```rust
let messages = store.channel::<Chat>("messages").open()?;
messages.under(&room_id).publish(&chat)?;
```

- `publish` answers once the store has the message, not once anyone handled
  it, and the publisher never waits for a subscriber. It answers nothing:
  how many received it has no meaning a relay, a sidecar and a process
  would all agree on.
- A message is any value the channel's type serializes, as a bucket's value
  is, and at most 1 MiB.
- Every subscriber receives every message: work shared among processes, one
  of N, is a queue's.

## Subscribe

```ts
const sub = await messages.under(roomId).subscribe(m => show(m.value), {
  onMissed: missed => reloadFromDb(),   // { reason: 'behind' | 'reconnect', count?: number }
  onError: (error, m) => report(error),
})
await sub.stop()

await messages.under(region).subscribe(onMessage, { recursive: true })   // and every branch below
await messages.subscribe(onNotice)                                        // the root alone

for await (const m of messages.under(roomId).each({ onMissed })) {
  await save(m.value)                   // a throw leaves the loop and ends the subscription
}
```

```python
sub = await messages.under(room_id).subscribe(
    lambda m: show(m.value),
    on_missed=lambda missed: reload_from_db(),   # Missed(reason="behind" | "reconnect", count)
    on_error=lambda error, m: report(error),
)
await sub.stop()

await messages.under(region).subscribe(on_message, recursive=True)
await messages.subscribe(on_notice)

async for m in messages.under(room_id).each(on_missed=on_missed):
    await save(m.value)
```

```go
sub, err := messages.Under(roomID).Subscribe(ctx, func(ctx context.Context, m channels.Message[Chat]) error {
	show(m.Value)
	return nil // an error goes to OnError
},
	channels.OnMissed(func(missed channels.Missed) { reloadFromDB() }),
	channels.OnError(func(err error, m channels.Message[Chat]) { report(err) }),
)
err = sub.Stop(ctx)

_, err = messages.Under(region).Subscribe(ctx, onMessage, channels.Recursive())
_, err = messages.Subscribe(ctx, onNotice)

for m, err := range messages.Under(roomID).Each(ctx, channels.OnMissed(onMissed)) {
	if err != nil {
		return err
	}
	if err := save(ctx, m.Value); err != nil {
		return err // leaving the loop ends the subscription
	}
}
```

```rust
let sub = messages
    .under(&room_id)
    .on_missed(|missed| reload())
    .on_error(|error, _| report(error))
    .subscribe(|m| show(&m.value))?;
sub.stop();

for m in messages.under(&room_id).each()? {
    save(&m?.value)?;
}
```

- `await subscribe()` answers once the store has the subscription: a
  message published after that reaches it. Go's `Subscribe` returns then
  too, its handler on a goroutine of the SDK's, rather than blocking as a
  queue's `Work` does. The loop of `each` listens from
  its first turn, so a program that publishes and must hear its own message
  subscribes first.
- A subscription matches the branch it was made on. `recursive` adds every
  branch below it, and `m.branch` says which, `['eu', 'shop-7']`. The root
  without `recursive` hears only what was published at the root.
- A subscription's calls come one at a time, in the order the messages were
  released: a publish outside a transaction at its call, one inside at its
  commit. A transaction that runs long may so release its message after one
  published later. Two channels promise no order between them.
- `onMissed` and `onError` come in the same line as the messages, between
  them, never beside a running call.
- A handler that throws goes to `onError` with the message, and the next
  message follows; the message is not delivered again. Without `onError`
  the store's logger says it, one line a minute a subscription with how many.
  A message that does not read as the channel's type, a Python publisher's
  with a field missing, goes to `onError` as `corrupt`.
- `await sub.stop()`: no call starts after it, and it answers when the
  running one has finished. Called from inside the subscription's own
  handler, it answers at once, since the running call is its caller. A
  second `stop` answers as the first.

## Missed

- Each branch someone listens to keeps its newest messages in a bounded
  log, and each of its subscribers reads it at its own place. One that falls
  behind the log's oldest message
  loses what it skipped, and its next call is `onMissed({ reason: 'behind',
  count })`, the count exact.
- A connection that drops is subscribed again by the SDK, and the next call
  is `onMissed({ reason: 'reconnect' })`: messages may have been lost, and
  how many is unknown, so `count` is absent. It says a loss was possible,
  not that one happened.
- What was missed is kept beside the log, never in it, so it cannot be lost
  to the same overflow; two misses before the next call are one, their
  counts added, or no count when either had none.
- Without `onMissed` the store's logger says it, one line a minute.
- After a miss, read the state again from where it is kept. Subscribe first,
  then read: whatever was published between a read and a later subscription
  is lost with nothing to say so.

## Transactions

```ts
await store.tx(async tx => {
  await tx.with(orders).set(id, order)
  await tx.with(events).under(id).publish({ kind: 'created' })
})
```

```python
async def create(tx) -> None:
    await tx.with_(orders).set(id, order)
    await tx.with_(events).under(id).publish(Created(kind="created"))


await store.tx(create)
```

```go
err = store.Tx(ctx, func(tx *tinystore.Tx) error {
	if err := orders.In(tx).Set(ctx, id, order); err != nil {
		return err
	}
	return events.In(tx).Under(id).Publish(ctx, Created{Kind: "created"})
})
```

- A message published in a transaction waits beside it and is released
  after its commit; a rollback, a failed call's savepoint or an error
  releases nothing. A channel keeps no file, so it joins any transaction:
  `store.tx` and `db.tx`. Over the wire, a database's transaction carries
  its messages to the server, which releases them at its commit; a store's
  transaction commits in one `kv.tx`, which carries none, so the SDK sends
  its messages once that commit answered.
- A message released after a commit is in memory: a process that dies
  between the commit and the release loses it. An event that must go out
  with its rows is a queue in the same database, `db.queue`, which commits
  with them.

## Through a server

- Every process connected to one store shares its channels, in the same
  process, through a sidecar or a remote server alike. Two processes cannot
  open one directory, so all of them publish and listen in one place.
- `channel.publish` is a call. `channel.subscribe` is a download: the
  server answers once the subscription is registered, then sends messages in
  batches within the client's credit, and a client that grants none falls
  behind and misses. A connection's pending output is bounded in bytes
  across its streams, which it drains in turn, so that one subscription
  cannot starve the others of its connection. A message larger than the
  body its connection agreed to take is missed there, and counted.
- A token that may write publishes and subscribes; a read token subscribes.

## Errors

| Kind      | When                                                                     |
|-----------|--------------------------------------------------------------------------|
| `invalid` | a bad name or owner; a message over 1 MiB                                |
| `corrupt` | a message that does not read as the channel's type, given to `onError`   |
| `limit`   | the store's memory for channels, or its subscriptions, is spent          |
| `closed`  | the store closed: a subscription stops as at `stop`, and `publish` fails |

Every error names the channel and its branch:
`channel messages: under "room-7": invalid`.

## Bounds

Proposed, to be measured before they are promised.

| What                     | Bound                                                   |
|--------------------------|---------------------------------------------------------|
| a message                | 1 MiB                                                   |
| a branch's log           | its newest 1024 messages or 4 MiB                       |
| every channel's messages | 64 MiB of the store's memory, each message counted once |
| a connection's output    | 8 MiB                                                   |
| a channel's name         | 64 bytes                                                |
| a branch's owners        | 1 KiB                                                   |
| a store's subscriptions  | 10,000                                                  |

## Under the surface

- A channel is a tree of the branches someone listens to, each with a log
  and a place in it a subscriber; subscribers of one branch share its log.
  A publish finds its branch and the ancestors listened to `recursive`, and
  puts the message in their logs: it is kept once, and the logs hold it by
  reference. A subscriber of one room never reads another's messages, and a
  publish on a branch nobody listens to keeps nothing, so no log waits
  empty to be cleaned up.
- The log is the core's own, a lock and a ring: the library runs no async
  runtime, and research found tokio's broadcast no better than a log.
- Memory is bounded three ways: a branch's log, every channel's messages
  together in the store's memory, and each connection's output. A slow
  subscriber and a recursive one cost places, never copies.
- A transaction's messages are staged with its savepoints and handed over in
  commit order at the writer, after the commit and before the writer goes:
  returning from a commit on two threads could otherwise deliver B before A.
  Under the writer the store only puts each message in its logs and marks
  them ready; no handler runs and nothing is sent there. It is the seam the
  replication stream takes later. Two files commit in no order between them.
- Rust's handlers run on threads of the store's, one call at a time a
  subscription; a handler that blocks falls behind and misses. How many
  threads is the core slice's question.

## Slices

1. The seam: messages staged with a transaction's savepoints and released
   in commit order, in the SQLite adapter's commits and the store's
   transactions.
2. The Rust core: channels, branches, `recursive`, the log and each
   subscriber's place, missed, `subscribe`, `each`, `stop`.
3. The wire, a connection's output bounded in bytes, and the Bun SDK.

The Python and Go SDKs followed, closed by the owner on 18 October.

## Gates

- A message published in a transaction that rolls back, or in a call whose
  savepoint failed, is never delivered; none is delivered before its commit.
- A subscriber that falls behind hears `onMissed` with the exact count, and
  one that loses its last messages hears it too.
- `await subscribe()` is ready: a message published after it arrives.
- `await stop()` from inside its own handler answers; from outside it waits
  for the running call.
- A subscription that cancels or disconnects frees its place, and zero
  credit never grows the server's memory.
- A channels-only build links no jobs.

## Later

- Replication: channels ride the tree of connections the page stream uses,
  as a live lane beside it, never in it. A publish goes to the owner, and a
  replica subscribes upstream once a branch and fans out. A message from a
  transaction carries its commit's number, and a replica releases it only
  once it has applied that commit, so that whoever hears it can read what
  the transaction wrote.
- Leaving out the publisher's own connection, as Pusher does.
- A subscription's status, connected or not, on the object `subscribe`
  answers.
- Patterns inside a branch, `room/*/typing`, when someone needs them.

## Newcomer check and reviews, 16 October

Three Haiku agents that had read nothing about TinyStore read a draft at the
highest effort: a newcomer's reading of fifteen bare calls, a free review,
and a blind choice between spellings. An outside reviewer read it before and
after them, and again after research's follow-up.

| It read                                                        | Change                                                   |
|----------------------------------------------------------------|----------------------------------------------------------|
| `subscribe` answering an async iterator, a loop to keep alive  | a handler; `each()` for the loop, the SDK's word for one |
| a branch's subscription hearing its sub-branches unasked       | the exact branch; `recursive` asks for more              |
| `under` as both a method and a message's field                 | `m.branch`                                               |
| `gap` as a term of sequence numbers                            | `onMissed`                                               |
| `stop()` as a function, awaited inside its handler: a deadlock | `sub.stop()`, which answers at once from inside          |
| a missed count after a reconnect                               | no count: a loss was possible, not proven                |
| `publish` answering how many received it                       | nothing: a relay and a process would not agree           |
| a message after a commit as delivered for sure                 | in memory; `db.queue` for an event that must go out      |
| `subscribe()` without a handler as a forgotten argument        | two verbs, `subscribe` and `each`                        |
| one log a channel, read by a room's subscriber for every room  | a log a branch someone listens to                        |
| a connection's credit as the bound of what a channel holds     | the store's memory too, each message counted once        |

The newcomer read `channel` as Go's and offered `topic`; the other two did
not stumble, and channels is decision 9.
