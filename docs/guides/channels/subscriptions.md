# Subscriptions and missed messages

A subscriber receives every message published on its branch after it
subscribed, in order, one call at a time. A channel keeps no history: a
message nobody listens to is gone. When a subscriber loses messages, by
falling behind or by a dropped connection, it is told, so it can read the
state again from where it is kept.

```ts
const messages = store.channel<Chat>('messages')

const sub = await messages.under(roomId).subscribe(m => show(m.value), {
	onMissed: missed => reloadFromDb(), // { reason: 'behind' | 'reconnect', count?: number }
	onError: (error, m) => report(error),
})
await sub.stop()
```

```python
messages = store.channel("messages", Chat)

sub = await messages.under(room_id).subscribe(
    lambda m: show(m.value),
    on_missed=lambda missed: reload_from_db(),  # Missed(reason="behind" | "reconnect", count)
    on_error=lambda error, m: report(error),
)
await sub.stop()
```

```go
messages, err := channels.OpenChannel[Chat](store, "messages")

sub, err := messages.Under(roomID).Subscribe(ctx, func(ctx context.Context, m channels.Message[Chat]) error {
	show(m.Value)
	return nil // an error goes to OnError
},
	channels.OnMissed(func(missed channels.Missed) { reloadFromDB() }),
	channels.OnError(func(err error, m channels.Message[Chat]) { report(err) }),
)
err = sub.Stop(ctx)
```

```rust
let messages = store.channel::<Chat>("messages").open()?;

let sub = messages
    .under(&room_id)
    .on_missed(|missed| reload())
    .on_error(|error| report(error))
    .subscribe(|m| show(&m.value))?;
sub.stop();
```

- `await subscribe()` answers once the store has the subscription: a message
  published after that reaches it. Go's `Subscribe` returns then too, its
  handler on a goroutine of the SDK's.
- A subscription's calls come one at a time, in the order the messages were
  released. Two channels promise no order between them.
- A handler that throws goes to `onError` with the message, and the next
  message follows; the message is not delivered again (contract CHAN-04).
  Without `onError`, the store's log says it, a line a minute. In Rust, a
  handler that panics goes to `on_error` with the error alone.
- `stop()` answers once the running call has finished, and no call starts
  after it. Called from inside the subscription's own handler, it answers at
  once.
- In Rust, dropping a `Subscription` does not stop it. It goes on listening
  until you call `stop()` or the store closes, as a queue's worker does.

## Branches

A message is published on a branch, `under(roomId)`, and a subscription hears
its own branch:

```ts
await messages.under('eu', 'shop-7').publish(order) // the branch ['eu', 'shop-7']
await messages.under('eu').subscribe(onOrder, { recursive: true }) // eu and every branch below it
await messages.subscribe(onNotice) // the root alone
```

```python
await messages.under("eu", "shop-7").publish(order)
await messages.under("eu").subscribe(on_order, recursive=True)
await messages.subscribe(on_notice)
```

```go
err = messages.Under("eu", "shop-7").Publish(ctx, order)
_, err = messages.Under("eu").Subscribe(ctx, onOrder, channels.Recursive())
_, err = messages.Subscribe(ctx, onNotice)
```

```rust
messages.under("eu").under("shop-7").publish(&order)?;
let all_eu = messages.under("eu").recursive().subscribe(on_order)?;
let root = messages.subscribe(on_notice)?;
```

`recursive` adds every branch below; `m.branch` says which one a message came
from, `['eu', 'shop-7']`. Without it, a subscription on `eu` hears only what
was published on `eu` itself. A subscriber of one room never reads another
room's messages.

## A loop instead of a handler

```ts
for await (const m of messages.under(roomId).each({ onMissed })) {
	await save(m.value) // a throw leaves the loop and ends the subscription
}
```

```python
async for m in messages.under(room_id).each(on_missed=on_missed):
    await save(m.value)
```

```go
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
for m in messages.under(&room_id).each()? {
    save(&m?.value)?;
}
```

`each` listens from its first turn. A program that publishes and must hear
its own message subscribes first.

## Missed messages

| `reason`    | When                                                              | `count`                                 |
|-------------|-------------------------------------------------------------------|-----------------------------------------|
| `behind`    | the subscriber fell further behind than its branch's log holds    | exact                                   |
| `reconnect` | the connection to the server dropped and the SDK subscribed again | absent: a loss was possible, not proven |

- A publisher never waits for a subscriber (contract CHAN-02). Each branch
  someone listens to keeps its newest messages in a log, 1,024 or 4 MiB, and
  each subscriber reads it at its own place. One that falls behind the log's
  oldest message loses what it skipped, and its next call is `onMissed`.
- Two misses before the next call are one, their counts added.
- Without `onMissed`, the store's log says it, a line a minute.

What to do after a miss is always the same: read the state again from where
it is kept, the database, a bucket. And subscribe first, then read: whatever
is published between a read and a later subscription is lost with nothing to
say so.

```ts
const sub = await messages.under(chatId).subscribe(m => append(m.value), {
	onMissed: async () => replaceAll(await loadMessages(chatId)),
})
replaceAll(await loadMessages(chatId)) // after subscribing, never before
```

```python
sub = await messages.under(chat_id).subscribe(
    lambda m: append(m.value),
    on_missed=lambda missed: asyncio.create_task(reload(chat_id)),
)
await reload(chat_id)  # after subscribing, never before
```

```go
sub, err := messages.Under(chatID).Subscribe(ctx, appendMessage,
	channels.OnMissed(func(channels.Missed) { reload(ctx, chatID) }))
reload(ctx, chatID) // after subscribing, never before
```

```rust
let sub = messages.under(&chat_id).on_missed(move |_| reload(chat_id)).subscribe(append)?;
reload(chat_id); // after subscribing, never before
```

## Through a server

Every process connected to one store shares its channels: a message published
by a worker in a sidecar reaches a web process's subscription. Messages travel
in batches within the client's credit, and a connection's pending output is
bounded, 8 MiB, so one slow subscription cannot starve the rest of its
connection. A token that may write publishes and subscribes; a read token
subscribes.

## Limits

| What                     | Limit                            |
|--------------------------|----------------------------------|
| a message                | 1 MiB                            |
| a branch's log           | its newest 1,024 messages, 4 MiB |
| every channel's messages | 64 MiB of the store's memory     |
| a connection's output    | 8 MiB                            |
| a store's subscriptions  | 10,000                           |

## See also

- [Channels](README.md): the engine's overview.
- [Publishing from a transaction](transactions.md).
- [Live updates](../../handbook/chat/live-updates.md) in the Handbook.
