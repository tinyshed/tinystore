# Live updates

A user has the chat open on a laptop and a phone. When a reply arrives, both
show it at once, without polling. And when a chat grows long, one worker at a
time writes its summary, even if several processes try. This chapter uses two
engines that keep nothing on the disk: Channels for messages to whoever
listens now, and Locks for one holder of a key at a time.

## Open a channel and a lock

```ts
interface ChatEvent {
	kind: 'message'
	id: number
}

const events = store.channel<ChatEvent>('chat-events')
const summaries = store.lock('summaries', { ttl: '30s' })
```

```python
@dataclass
class ChatEvent:
    kind: str
    id: int


events = store.channel("chat-events", ChatEvent)
summaries = store.lock("summaries", ttl="30s")
```

```go
type ChatEvent struct {
	Kind string `json:"kind"`
	ID   int64  `json:"id"`
}

events, err := channels.OpenChannel[ChatEvent](store, "chat-events")
summaries, err := locks.OpenLock(store, "summaries", locks.TTL(30*time.Second))
```

```rust
#[derive(Serialize, Deserialize, Clone)]
struct ChatEvent {
    kind: String,
    id: i64,
}

let events = store.channel::<ChatEvent>("chat-events").open()?;
let summaries = store.lock("summaries").open()?;
```

A channel is a name that messages of one type are published on. Every
process connected to the store shares it: four web processes and a worker
all publish and listen in one place, because they all reach the same store.

## Tell every open tab

The reply worker from [Replies in the background](replies.md) saves the reply
and publishes an event about it in the same transaction:

```ts
await db.tx(async tx => {
	const reply = await tx.exec`insert into messages (chat_id, role, text, created_at, reply_to)
		values (${chatId}, 'assistant', ${answer}, ${Date.now()}, ${messageId})`
	await tx.with(events).under(chatId).publish({ kind: 'message', id: reply.lastInsertRowid })
})
```

```python
async def save(tx: tinystore.SqlTx) -> None:
    reply = await tx.exec(
        "insert into messages (chat_id, role, text, created_at, reply_to) values (?, 'assistant', ?, ?, ?)",
        chat_id, answer, time.time_ns() // 1_000_000, message_id,
    )
    await tx.with_(events).under(chat_id).publish(ChatEvent(kind="message", id=reply.last_insert_rowid))


await db.tx(save)
```

```go
err = db.Tx(ctx, func(tx *sqldb.Tx) error {
	reply, err := tx.Exec(ctx, `insert into messages (chat_id, role, text, created_at, reply_to) values (?, 'assistant', ?, ?, ?)`,
		chatID, answer, time.Now().UnixMilli(), messageID)
	if err != nil {
		return err
	}
	return events.In(tx).Under(chatID).Publish(ctx, ChatEvent{Kind: "message", ID: reply.LastInsertRowid})
})
```

```rust
db.tx(|tx| -> tinystore::Result<()> {
    let reply = tx.exec(sql!(
        "insert into messages (chat_id, role, text, created_at, reply_to) values (?, 'assistant', ?, ?, ?)",
        chat_id,
        answer,
        unix_millis(SystemTime::now()),
        message_id
    ))?;
    tx.with(&events).under(chat_id).publish(&ChatEvent { kind: "message".into(), id: reply.last_insert_rowid })?;
    Ok(())
})?;
```

`under(chatId)` is a branch of the channel, as a bucket has branches: the
messages of one chat. A message published inside a transaction waits for its
commit, and a rollback drops it (contract CHAN-03). A tab never hears about a
reply that was never saved, and never hears about it before it can read it.

The browser listens through server-sent events. The endpoint subscribes to
the chat's branch and forwards each event:

```ts
async function stream(chatId: number, send: (data: unknown) => void, closed: Promise<void>) {
	const sub = await events.under(chatId).subscribe(m => send(m.value), {
		onMissed: () => send({ kind: 'reload' }),
	})
	await closed // the browser went away
	await sub.stop()
}
```

```python
async def stream(chat_id: int, send, closed: asyncio.Event) -> None:
    sub = await events.under(chat_id).subscribe(
        lambda m: send(m.value),
        on_missed=lambda missed: send({"kind": "reload"}),
    )
    await closed.wait()  # the browser went away
    await sub.stop()
```

```go
func stream(ctx context.Context, chatID int64, send func(any)) error {
	sub, err := events.Under(chatID).Subscribe(ctx, func(ctx context.Context, m channels.Message[ChatEvent]) error {
		send(m.Value)
		return nil
	}, channels.OnMissed(func(channels.Missed) { send(map[string]string{"kind": "reload"}) }))
	if err != nil {
		return err
	}
	<-ctx.Done() // the browser went away
	return sub.Stop(context.Background())
}
```

```rust
fn stream(chat_id: i64, send: impl Fn(TabEvent) + Send + Sync + Clone + 'static) -> tinystore::Result<Subscription> {
    let on_missed = send.clone();
    events
        .under(chat_id)
        .on_missed(move |_| on_missed(TabEvent::Reload))
        .subscribe(move |m| send(TabEvent::Message(m.value)))
}
```

`subscribe` returns once the store has the subscription. Every message
published after that reaches it, in order (contract CHAN-01). The handler
gets one message at a time, never two at once.

## A tab that falls behind

A phone on a slow network cannot take events as fast as a busy chat
publishes them. The chat does not wait for it:

```scene
code ts
  const sub = await events.under(chatId).subscribe(m => send(m.value), {
    onMissed: () => send({ kind: 'reload' }),
  })
end
actor worker "Reply worker" at 8,50  icon=cpu
actor chan   "chat-events"  at 45,50 icon=radio
actor laptop "Laptop tab"   at 88,22 icon=laptop
actor phone  "Phone tab"    at 88,80 icon=smartphone
link worker chan
link chan laptop
link chan phone
set phone "on a slow network"

step "The worker publishes replies. The channel keeps the newest messages of the chat's branch, and each tab reads at its own pace." line=1
  send worker -> chan "reply 41"
  send chan -> laptop "reply 41" color=ok
  & send chan -> phone "reply 41" color=ok

step "A burst arrives while the phone is stuck. The worker never waits for it: publish returns as soon as the store has the message." line=1
  send worker -> chan "replies 42…1100"
  send chan -> laptop "42…1100" color=ok
  set phone "1,059 behind"

step "The oldest messages leave the branch's log before the phone reads them. Its next call is onMissed, with exactly how many it lost." line=2
  send chan -> phone "missed 35" color=bad

step "The tab reloads the chat from the database, which has every message, and goes on listening." line=2
  send phone -> chan "reload"
  set phone "up to date"
```

A publisher never waits for a subscriber, and a subscriber that falls behind
is told exactly how many messages it missed before its next one (contract
CHAN-02). A channel is not a log: the database is. So a missed event costs
one reload, never a wrong screen.

The same holds when the connection to the store drops. The SDK subscribes
again by itself, and the next call is `onMissed` with the reason
`reconnect`, because messages may have been lost meanwhile.

## One summary per chat at a time

When a chat passes a hundred messages, the chat asks the model for a summary
of it. Two replies finishing at once must not start two summaries of the same
chat, so the job holds a lock on the chat's id:

```ts
const started = await summaries.tryHold(chatId, async lease => {
	const summary = await model.summarize(await history(chatId))
	const fence = lease.fence.toString().padStart(40, '0')
	await db.exec`update chats set summary = ${summary}, summary_fence = ${fence}
		where id = ${chatId} and summary_fence < ${fence}`
})
if (!started.acquired) return // another worker is writing this chat's summary
```

```python
async with summaries.try_hold(chat_id) as lease:
    if lease is None:
        return  # another worker is writing this chat's summary
    summary = await model.summarize(await history(chat_id))
    fence = f"{lease.fence:040d}"
    await db.exec(
        "update chats set summary = ?, summary_fence = ? where id = ? and summary_fence < ?",
        summary, fence, chat_id, fence,
    )
```

```go
started, err := summaries.TryHold(ctx, chatID, func(ctx context.Context, lease *locks.Lease) error {
	summary, err := model.Summarize(ctx, history(ctx, chatID))
	if err != nil {
		return err
	}
	fence := fmt.Sprintf("%040s", lease.Fence)
	_, err = db.Exec(ctx, `update chats set summary = ?, summary_fence = ? where id = ? and summary_fence < ?`,
		summary, fence, chatID, fence)
	return err
})
if err == nil && !started {
	return nil // another worker is writing this chat's summary
}
```

```rust
let started = summaries.try_hold(chat_id, |lease| -> Result<(), AppError> {
    let summary = model.summarize(&history(chat_id)?)?;
    let fence = format!("{:040}", lease.fence().get());
    db.exec(sql!(
        "update chats set summary = ?, summary_fence = ? where id = ? and summary_fence < ?",
        summary,
        &fence,
        chat_id,
        &fence
    ))?;
    Ok(())
})?;
if started.is_none() {
    return Ok(()); // another worker is writing this chat's summary
}
```

The migration adds the two columns:

```sql title="migrations/0004_summaries.sql"
alter table chats add column summary text;
alter table chats add column summary_fence text not null default '';
```

`tryHold` runs the function only if nobody holds the key, and returns at once
otherwise. `hold` would wait for the key instead, ten seconds unless `wait`
says otherwise. While the function runs, the SDK renews the lease every third
of its `ttl`, and the key is let go when the function returns or throws.

## A worker that stalls

The summary takes a while, and a worker can freeze in the middle: a long
garbage-collection pause, a laptop lid closed on a development machine, a
virtual machine moved to another host. Its lease runs out, and another worker
takes the key. Then the first one wakes up:

```scene
code ts
  const started = await summaries.tryHold(chatId, async lease => {
    const summary = await model.summarize(await history(chatId))
    const fence = lease.fence.toString().padStart(40, '0')
    await db.exec`update chats set summary = ${summary}, summary_fence = ${fence}
      where id = ${chatId} and summary_fence < ${fence}`
  })
end
actor a    "Worker A"  at 10,22 icon=cpu
actor b    "Worker B"  at 10,80 icon=cpu
actor lock "summaries" at 48,50 icon=lock
actor db   "chat.db"   at 88,50 icon=database
link a lock
link b lock
link a db
link b db
set db "fence: none"

step "Worker A takes the lock on chat 7 and gets fence 41. It starts asking the model." line=1-2
  send a -> lock "tryHold 7"
  send lock -> a "fence 41" color=ok

step "Worker A freezes for a minute. It cannot renew its lease, so after 30 seconds the lease runs out." line=2
  mark a down
  say lock "lease 41 ran out"

step "Worker B takes the free key and gets fence 42, larger than every fence given before. It writes its summary." line=3-5
  send b -> lock "tryHold 7"
  send lock -> b "fence 42" color=ok
  send b -> db "update, fence 42"
  set db "fence: 42"

step "Worker A wakes up and writes its stale summary with fence 41. The where clause compares fences, so the update changes no row." line=4-5
  send a -> db "update, fence 41" color=bad
  say db "0 rows"
```

Every lease the store gives has a fence larger than every earlier one, across
restarts too (contract LOCK-02). A lease over a connection lasts its `ttl`
from its last renewal, and one that ran out is never renewed (contract
LOCK-05). The fence is what makes this safe: the lock makes two writers at
once rare, and the fence makes the late one harmless. Anything that keeps the
largest fence it has seen can refuse a late write, as the `where` clause does
here.

The fence is a 128-bit number. Padded to 40 digits, it compares as text in
the same order as the number, so SQLite can compare it.

A Rust program runs the lock in the store's own process, where a holder
cannot freeze apart from the store, so its lease ends only when its function
does. `maxHold` bounds a lease in every language, for work that must never
hold a key longer than a set time.

## Why this design?

**Why channels keep nothing on the disk.** A typing indicator, "new reply
in chat 7", "this tab should reload": these are worth something now and
nothing in a minute. Writing them to a file would cost a sync per event and
fill the disk with what nobody reads again. What must not be lost belongs in
the database or in a queue, and an event only says "look again".

**Why a slow subscriber loses messages instead of slowing everyone.** The
other choice is to make the publisher wait until every subscriber caught up.
Then one phone on a train slows down every reply in the chat, and a frozen
tab stops the worker. TinyStore keeps a bounded log per branch, lets each
subscriber read at its own pace, and tells the slow one what it lost, with
an exact count.

**Why locks live in memory.** One process owns the store, and every holder
reaches the store through it, so that process sees every lease and every
waiter. Giving and renewing a lease is a change in memory, with no disk sync.
After a restart nobody holds anything, which is right: the threads that held
the keys are gone. A lease kept on disk would make a restarted store wait
out the `ttl` of holders that no longer exist.

**Why a fence and not just a lock.** No lock can stop a process that froze
while holding it: when it wakes, it does not know that its time ran out.
Only the thing it writes to can refuse it, and only if it can tell old from
new. The fence gives it that number, as Martin Kleppmann's essay on
distributed locks describes. A payment provider that compares nothing still
needs an idempotency key: a lock makes double work rare, never impossible.

## Limits and defaults

| What                          | Limit                                                |
|-------------------------------|------------------------------------------------------|
| a message                     | 1 MiB                                                |
| a branch's log                | its newest 1,024 messages or 4 MiB                   |
| every channel's messages      | 64 MiB of the store's memory                         |
| a store's subscriptions       | 10,000                                               |
| a connection's pending output | 8 MiB                                                |
| `wait` of `hold`              | 10 seconds unless set                                |
| a lease over a connection     | its `ttl` from its last renewal, renewed every third |
| `maxHold`                     | none unless set                                      |

## See also

- [Replies in the background](replies.md): the worker that publishes the
  events.
- [Channels API reference](../reference/bun/channels.md) and
  [Locks API reference](../reference/bun/locks.md): every call of both.
