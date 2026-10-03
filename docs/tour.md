# A tour

This tour builds the backend of a small AI chat, one engine at a time:
messages in SQL, plan limits and idempotency in KV, replies written in the
background by jobs, attachments in blobs, logs in records and response times
in metrics. Each step takes a few lines, and everything lives in one
directory.

## The plan

```text
POST /messages ─┬─ KV       run once per idempotency key, check the user's quota
                ├─ SQL      insert the message
                ├─ Blobs    store its attachment
                └─ Jobs     enqueue the AI reply
reply worker   ─┬─ Jobs     search, then answer, each step kept across retries
                ├─ Metrics  time the model call
                └─ Records  log what happened
browser        ─── Jobs     watch the reply: queued, thinking, done
```

## 1. Open everything

```ts
import { open } from 'tinystore'

await using store = await open('./data')

const db = await store.sql('chat', { migrations: './migrations' })
const quota = store.kv.quota('ai', { session: '100/5h', weekly: '300/7d' })
const sends = store.kv.once<{ id: number }>('sends')
const replies = store.jobs.queue<{ messageId: number }>('replies', { maxRunning: 4 })
const files = store.blobs.bucket('attachments', { maxSize: 20 << 20 })
const log = store.records.logger('chat')
const modelTime = store.metrics.timer('model_call_ms')
```

```python
async with tinystore.open("./data") as store:  # the rest of the tour runs inside this block
    db = await store.sql("chat", migrations="./migrations")
    quota = store.kv.quota("ai", session="100/5h", weekly="300/7d")
    sends = store.kv.once("sends", dict)
    replies = store.jobs.queue("replies", dict, max_running=4)
    files = store.blobs.bucket("attachments", max_size=20 << 20)
    logging.basicConfig(handlers=[store.records.handler("chat")], level=logging.INFO)
    model_time = store.metrics.timer("model_call_ms")
```

```go
// errors left out
store, err := tinystore.Open(ctx, "./data", tinystore.Options{})
db, err := sqldb.Open(ctx, store, "chat", migrations, nil)

state, err := kv.Open(ctx, store, kv.Options{})
quota, err := kv.OpenQuota(ctx, state, "ai",
	kv.Window("session", 100, 5*time.Hour), kv.Window("weekly", 300, 7*24*time.Hour))
sends, err := kv.OpenOnce[Sent](ctx, state, "sends")

queues, err := jobs.Open(ctx, store, jobs.Options{})
replies, err := jobs.OpenQueue[Reply](ctx, queues, "replies", jobs.MaxRunning(4))

objects, err := blobs.Open(ctx, store, blobs.Options{})
files, err := blobs.OpenBucket(ctx, objects, "attachments", blobs.MaxSize(20<<20))

logs, err := records.Open(ctx, store, records.Options{})
log := slog.New(logs.Handler("chat"))

stats, err := metrics.Open(ctx, store, metrics.Options{})
modelTime := stats.Timer("model_call_ms")
```

Six engines, one directory. In Bun and Python, everything above runs in one
sidecar that all your processes share.

## 2. Accept a message

```ts
async function postMessage(userId: number, text: string, idempotencyKey: string) {
	return sends.run(idempotencyKey, async () => {
		const usage = await quota.allow(userId)
		if (!usage.ok) {
			throw new LimitReached(usage.retryAfter)
		}
		const { lastId } = await db.exec`insert into messages (user_id, text, created_at)
			values (${userId}, ${text}, ${Date.now()})`
		await replies.enqueue({ messageId: lastId }, { key: `message:${lastId}` })
		log.info('message received', { userId, messageId: lastId })
		return { id: lastId }
	})
}
```

```python
async def post_message(user_id: int, text: str, idempotency_key: str) -> dict:
    async def send() -> dict:
        usage = await quota.allow(user_id)
        if not usage.ok:
            raise LimitReached(usage.retry_after)
        done = await db.exec(
            "insert into messages (user_id, text, created_at) values (?, ?, ?)", user_id, text, int(time.time() * 1000)
        )
        await replies.enqueue({"message_id": done.last_id}, key=f"message:{done.last_id}")
        logging.info("message received", extra={"user_id": user_id, "message_id": done.last_id})
        return {"id": done.last_id}

    return await sends.run(idempotency_key, send)
```

```go
func (a *app) postMessage(ctx context.Context, userID int64, text, idempotencyKey string) (Sent, error) {
	return a.sends.Run(ctx, idempotencyKey, func(ctx context.Context) (Sent, error) {
		usage, err := a.quota.Allow(ctx, userID)
		if err != nil {
			return Sent{}, err
		}
		if !usage.OK {
			return Sent{}, limitReached(usage.RetryAfter)
		}
		res, err := a.db.Exec(ctx, `insert into messages (user_id, text, created_at) values (?, ?, ?)`,
			userID, text, a.store.Now())
		if err != nil {
			return Sent{}, err
		}
		id, _ := res.LastInsertId()
		err = a.replies.Enqueue(ctx, Reply{MessageID: id}, jobs.Key(fmt.Sprint("message:", id)))
		a.log.Info("message received", "userId", userID, "messageId", id)
		return Sent{ID: id}, err
	})
}
```

A lot happens in these lines:

- **Once**: a client that retries the request with the same idempotency key
  gets the same answer, and the message is inserted only once. Two retries
  that arrive together wait for each other.
- **Quota**: the use counts in the 5-hour and the weekly window, or in
  neither. A rejected request throws, and `once` keeps nothing, so the client
  can try again later.
- **SQL**: the insert returns after it is on disk, and inserts from many users
  share one disk sync.
- **Jobs**: the reply is queued on disk. If the server crashes now, the reply
  is still written after the restart.

In Go, you can also commit the message and its job in one transaction. See
[Jobs and your data](jobs/your-data.md).

## 3. Write the reply in the background

```ts
await replies.work(async job => {
	const message = await db.one<Message>`select * from messages where id = ${job.value.messageId}`
	const hits = await job.step('search', () => search(message!.text))
	job.progress({ step: 'thinking' })
	const answer = await job.step('answer', () => modelTime.measure(() => model.answer(message!.text, hits)))
	await db.exec`insert into replies (message_id, text) values (${message!.id}, ${answer}) on conflict do nothing`
	log.info('reply sent', { messageId: message!.id })
}, { workers: 4 })
```

```python
async def reply(job: tinystore.Job[dict]) -> None:
    message = await db.one(Message, "select * from messages where id = ?", job.value["message_id"])
    hits = await job.step("search", lambda: search(message.text))
    job.progress({"step": "thinking"})

    async def answer() -> str:
        with model_time.measure():
            return await model.answer(message.text, hits)

    text = await job.step("answer", answer)
    await db.exec("insert into replies (message_id, text) values (?, ?) on conflict do nothing", message.id, text)
    logging.info("reply sent", extra={"message_id": message.id})


await replies.work(reply, workers=4)
```

```go
err = replies.Work(ctx, func(ctx context.Context, job jobs.Job[Reply]) error {
	message, _, err := sqldb.One[Message](ctx, db, `select * from messages where id = ?`, job.Value.MessageID)
	if err != nil {
		return err
	}
	hits, err := jobs.Step(ctx, job, "search", func(ctx context.Context) ([]Hit, error) {
		return search(ctx, message.Text)
	})
	if err != nil {
		return err
	}
	job.Progress(map[string]string{"step": "thinking"})
	answer, err := jobs.Step(ctx, job, "answer", func(ctx context.Context) (string, error) {
		defer modelTime.Since(time.Now())
		return model.Answer(ctx, message.Text, hits)
	})
	if err != nil {
		return err
	}
	_, err = db.Exec(ctx, `insert into replies (message_id, text) values (?, ?) on conflict do nothing`,
		message.ID, answer)
	log.Info("reply sent", "messageId", message.ID)
	return err
}, jobs.Workers(4))
```

- **Steps**: if the model call fails, the job is retried, and `search`
  returns its saved result instead of running again.
- **Max running**: at most four replies are written at once, across all your
  processes, so a burst of messages doesn't overload the model.
- **At least once**: if the worker crashes after the insert, the job runs
  again, and `on conflict do nothing` keeps the reply single.
- **Timer**: every model call is measured.

## 4. Show the reply's progress

```ts
for await (const entry of replies.watch(`message:${messageId}`)) {
	events.send({ state: entry.state, ahead: entry.ahead, progress: entry.progress })
}
```

```python
async for entry in replies.watch(f"message:{message_id}"):
    await events.send({"state": entry.state, "ahead": entry.ahead, "progress": entry.progress})
```

```go
for entry, err := range replies.Watch(ctx, fmt.Sprint("message:", messageID)) {
	if err != nil {
		return err
	}
	events.Send(entry.State, entry.Ahead, entry.Progress)
}
```

Stream the entries to the browser over server-sent events, and the user sees
"3 messages ahead", then "thinking", then the reply. There is no status table:
the queue knows where each job is.

## 5. Store attachments

```ts
await files.of('messages', messageId).put(file.name, file.stream(), { contentType: file.type, size: file.size })
```

```python
await files.of("messages", message_id).put(upload.filename, await upload.read(), content_type=upload.content_type)
```

```go
obj, err := files.Of("messages", messageID).Put(ctx, header.Filename, part,
	blobs.ContentType(header.Header.Get("Content-Type")), blobs.Size(header.Size))
```

The file streams to disk without filling memory, appears whole or not at all,
and is limited to 20 MiB by the bucket. Deleting a conversation deletes its
files with `files.of('messages', id).clear()`.

## 6. Look inside

```sh
tinystore status ./data                       # every engine's size, and who serves the store
tinystore logs ./data -f --level warn         # follow the warnings as they happen
claude mcp add chat -- tinystore mcp ./data   # let an AI agent read the store
```

```ts
const slowest = await store.metrics.aggregate({ name: 'model_call_ms_max', since: '1h', width: '5m', op: 'max' })
```

```python
slowest = await store.metrics.aggregate(name="model_call_ms_max", since="1h", width="5m", op="max")
```

```go
slowest, err := stats.Aggregate(ctx, metrics.AggregateRequest{
	Range: metrics.Range{Name: "model_call_ms_max", Since: time.Hour},
	Width: 5 * time.Minute,
	Op:    metrics.AggregateMax,
})
```

## What you didn't write

This backend has no Redis, no queue service, no object storage, no log
shipper and no metrics server. It also has no `expires_at` columns, no
cleanup cron jobs, no status table for replies, no retry loop and no
deduplication table. Each of those is a feature of an engine instead.

The whole state of the application is one directory. Copy it with a
[backup](running/backups.md), and you have copied everything.

## Where to go next

- [Quotas](kv/quotas.md), [Once](kv/once.md) and [Steps](jobs/steps.md): the
  features this tour used, in depth.
- [Jobs and your data](jobs/your-data.md): commit a message and its job
  together.
- [Durability](concepts/durability.md): what each call guarantees.
