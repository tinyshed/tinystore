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

## Reference

[Bun and Node](../../reference/bun/channels.md) ·
[Python](../../reference/python/channels.md) ·
[Go](../../reference/go/channels.md) ·
[Rust](../../reference/rust/channels.md)
