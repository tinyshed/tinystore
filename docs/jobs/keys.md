# Keys

Give a job a key to find it later: to show it, change it, cancel it, or make
sure it is enqueued only once. A key names one job of a queue at a time.
Without a key, a job can't be found, which is fine for most background work.

## Schedule a message, then change or cancel it

```ts
const later = store.jobs.queue<Draft>('send-later')
const key = `chat:${chatId}:${draft.id}`

await later.enqueue(draft, { at: nineAm, key })
await later.update(key, edited, { at: tenAm }) // ConflictError: it is being sent, or was sent
await later.cancel(key)                        // false: it was already sent

const page = await later.scan({ prefix: `chat:${chatId}:` }) // "3 scheduled messages"
```

```python
later = store.jobs.queue("send-later", Draft)
key = f"chat:{chat_id}:{draft.id}"

await later.enqueue(draft, at=nine_am, key=key)
await later.update(key, edited, at=ten_am)  # ConflictError: it is being sent, or was sent
await later.cancel(key)                     # False: it was already sent

page = await later.scan(prefix=f"chat:{chat_id}:")  # "3 scheduled messages"
```

```go
later, err := jobs.OpenQueue[Draft](ctx, queues, "send-later")
key := fmt.Sprintf("chat:%d:%s", chatID, draft.ID)

err = later.Enqueue(ctx, draft, jobs.At(nineAM), jobs.Key(key))
err = later.Update(ctx, key, edited, jobs.At(tenAM)) // ErrConflict: it is being sent, or was sent
cancelled, err := later.Cancel(ctx, key)            // false: it was already sent

page, err := later.Scan(ctx, jobs.Query{Prefix: fmt.Sprintf("chat:%d:", chatID)})
```

The scheduled message lives in the queue until it is sent. There is no
separate table of scheduled messages to keep in sync with the queue.

- `update` changes the value of a job that is still waiting, and its time if
  you give one. A job that is running or already done can't be changed, and
  `update` fails with a conflict.
- `cancel` removes the job and tells you whether there was one. If the job is
  running, its handler is told to stop: its context is cancelled in Go, its
  `job.signal` aborts in Bun, and its task is cancelled in Python.
- `scan` lists jobs whose keys start with a prefix, a page at a time. End the
  prefix with a separator: `chat:4` would also find the jobs of chat 42.

## Enqueue the same key again

| The key's job is | `enqueue(at: 9:00)` |
|---|---|
| waiting until 10:00 | moves it to 9:00. A key's job can move earlier, never later |
| waiting until 8:00 | changes nothing: the job stays at 8:00 |
| running | asks for one more run after this one, at 9:00 |
| failed | starts it again at 9:00, with its attempts reset |
| not there | creates a new job at 9:00 |

An `enqueue` of a waiting key adds nothing and keeps the old value. This is
how a key stops duplicates: a user who taps "send" twice enqueues one job.

When a job runs and the data it works on changes, enqueue its key again. The
job then runs once more after the current run, so the handler sees the
change. Several such enqueues during one run still mean only one more run.

## Run a key only once

```ts
const pushes = store.jobs.queue<Push>('pushes', { keepDone: '1h' })

await pushes.enqueue(push, { key: `${messageId}:${memberId}` })
```

```python
pushes = store.jobs.queue("pushes", Push, keep_done="1h")

await pushes.enqueue(push, key=f"{message_id}:{member_id}")
```

```go
pushes, err := jobs.OpenQueue[Push](ctx, queues, "pushes", jobs.KeepDone(time.Hour))

err = pushes.Enqueue(ctx, push, jobs.Key(messageID+":"+memberID))
```

Normally, a key is forgotten when its job is done, and the next `enqueue` of
the key creates a new job. With `keepDone`, the queue remembers the keys of
finished jobs for that long, and an `enqueue` of such a key adds nothing.
Even if the code that enqueues the push runs twice, nobody gets the push twice.

## Find failed jobs

```ts
const failed = await emails.scan({ state: 'failed' }) // the latest failures first
for (const job of failed.items) {
	console.log(job.key, job.error)
}
```

```python
failed = await emails.scan(state="failed")  # the latest failures first
for job in failed.items:
    print(job.key, job.error)
```

```go
failed, err := emails.Scan(ctx, jobs.Query{State: jobs.Failed}) // the latest failures first
for _, job := range failed.Entries {
	fmt.Println(job.Key, job.Err)
}
```

A failed job is kept for 7 days (`keepFailed`), with the error of its last
attempt. Fix the cause, then `enqueue` its key again to restart it, or
`update` it with a corrected value. `scan` with the failed state also lists
failed jobs that have no key.

## Limits and defaults

| | |
|---|---|
| A key | 1 to 1,024 bytes of text |
| Done keys kept | not kept, unless you set `keepDone` |
| Failed jobs kept | 7 days |
| A `scan` page | 1,000 jobs or 4 MiB of values |

## See also

- [Watching a job](watching.md): read where a job is and follow it until it
  ends.
- [Jobs and your data](your-data.md): a job as an alarm for a row of your
  database.
- [jobs/README.md](../../jobs/README.md): the full contract of keys.
