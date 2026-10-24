# Channels

The Channels engine carries messages to whoever listens at that moment: a new
reply to every open tab, a "typing" indicator, a signal to reload. Messages
live in memory and are delivered at most once. A channel has branches, such
as one per chat, and a message published inside a transaction is delivered
after its commit.

```ts
const events = store.channel<ChatEvent>('chat-events')

const sub = await events.under(chatId).subscribe(m => send(m.value), {
	onMissed: () => send({ kind: 'reload' }),
})
await events.under(chatId).publish({ kind: 'message', id: 41 })
await sub.stop()
```

```python
events = store.channel("chat-events", ChatEvent)

sub = await events.under(chat_id).subscribe(lambda m: send(m.value), on_missed=lambda missed: send({"kind": "reload"}))
await events.under(chat_id).publish(ChatEvent(kind="message", id=41))
await sub.stop()
```

```go
events, err := channels.OpenChannel[ChatEvent](store, "chat-events")

sub, err := events.Under(chatID).Subscribe(ctx, func(ctx context.Context, m channels.Message[ChatEvent]) error {
	send(m.Value)
	return nil
}, channels.OnMissed(func(channels.Missed) { send(reload) }))
err = events.Under(chatID).Publish(ctx, ChatEvent{Kind: "message", ID: 41})
err = sub.Stop(ctx)
```

```rust
let events = store.channel::<ChatEvent>("chat-events").open()?;

let sub = events.under(chat_id).on_missed(|_| reload()).subscribe(|m| send(m.value))?;
events.under(chat_id).publish(&ChatEvent { kind: "message".into(), id: 41 })?;
sub.stop();
```

## What it guarantees

- A subscriber receives what is published after it subscribed, in order, and
  nothing after it stops (contract CHAN-01).
- A publisher never waits for a subscriber; one that falls behind is told
  exactly how many messages it missed (contract CHAN-02).
- A message published in a transaction is delivered after its commit, and
  never after a rollback (contract CHAN-03).
- A handler that fails is reported, and the next message follows (contract
  CHAN-04).
- Not promised: a message nobody listens to, or one before a restart, is
  gone (NOT-02). What must not be lost belongs in a queue.

## In these guides

| Page                                                  | Covers                                                   |
|-------------------------------------------------------|----------------------------------------------------------|
| [Subscriptions and missed messages](subscriptions.md) | `subscribe`, `each`, `recursive`, `onMissed`, reconnects |
| [Publishing from a transaction](transactions.md)      | `tx.with(channel)` in `db.tx` and `store.tx`             |

New to Channels? [Live updates](../../handbook/chat/live-updates.md) in the
Handbook tells every open tab about a new reply.

## Async Rust

With the `async` feature, a Rust program opens the same channels from
`tinystore::aio::Store`, and a loop over a channel's messages is a `Stream`.
This section shows only Rust, because in Bun, Node and Python every call is
already awaited, and in Go a call blocks its goroutine, which costs little.

```rust
use futures::StreamExt; // next() on a Stream
use tinystore::aio::Store;

let store = Store::open("data", Options::default())?;
let events = store.channel::<ChatEvent>("chat-events").open()?;

let mut each = events.under(chat_id).on_missed(|_| reload()).each()?; // listens from here
events.under(chat_id).publish(&ChatEvent { kind: "message".into(), id: 41 })?; // not awaited
while let Some(m) = each.next().await {
    send(m?.value); // blocking: for m in events.under(chat_id).each()? { … }
}
```

- `publish` is a plain call. It puts the message in memory and never waits.
- `each()` returns a `Stream`, and it listens from the moment you call it,
  before its first poll. A program that publishes and must hear its own
  message calls `each()` first.
- The stream blocks no thread while it waits. A poll takes up to 64 messages
  from the branch's log and decodes them on your executor's thread.
- A message that does not read as the channel's type is an error item, and
  the next message follows. The stream ends when its `stopper()` stops it or
  when the store closes.
- A handler is the blocking handle's: `events.blocking().subscribe(…)`. In
  async code, spawn a task that reads the stream instead.
- Inside an awaited `store.tx`, `tx.with(&events).publish(…)` hands the
  message over after the commit, as in blocking Rust.

[Rust with async](../../handbook/start/rust-async.md) explains where the work
runs and what dropping a future does.

## Reference

[Bun and Node](../../reference/bun/channels.md) ·
[Python](../../reference/python/channels.md) ·
[Go](../../reference/go/channels.md) ·
[Rust](../../reference/rust/channels.md)
