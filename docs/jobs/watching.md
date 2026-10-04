# Watching a job

Ask a queue where a job is: how many jobs are ahead of it, whether it is
running, and how far it got. Then follow it until it ends. A video upload page
can show "17 videos ahead of yours", then a progress bar, then "done", without
a status table in your database.

## Show a job's place in the queue

```ts
const videos = store.jobs.queue<Video>('videos', { maxRunning: 2 })
await videos.enqueue(video, { key: video.id })

const entry = await videos.get(video.id)
entry?.state // 'waiting'
entry?.ahead // 17
```

```python
videos = store.jobs.queue("videos", Video, max_running=2)
await videos.enqueue(video, key=video.id)

entry = await videos.get(video.id)
entry.state  # "waiting"
entry.ahead  # 17
```

```go
videos, err := jobs.OpenQueue[Video](ctx, queues, "videos", jobs.MaxRunning(2))
err = videos.Enqueue(ctx, video, jobs.Key(video.ID))

entry, found, err := videos.Get(ctx, video.ID)
entry.State // jobs.Waiting
entry.Ahead // 17
```

`get` finds a job by its [key](keys.md). `ahead` counts the jobs that will run
before it, up to 10,000.

## Report progress from the handler

```ts
await videos.work(async job => {
	await transcode(job.value, { onProgress: done => job.progress(done) })
})
```

```python
async def run(job: tinystore.Job[Video]) -> None:
    await transcode(job.value, on_progress=job.progress)


await videos.work(run)
```

```go
err = videos.Work(ctx, func(ctx context.Context, job jobs.Job[Video]) error {
	return transcode(ctx, job.Value, func(done float64) { job.Progress(done) })
})
```

A progress report is any JSON value up to 4 KiB: a number, or an object such as
`{ step: 'upload', percent: 40 }`. It lives in memory until the attempt ends,
and it is never written to disk, so reporting often is cheap. The SDKs send at
most ten reports a second.

## Follow a job until it ends

```ts
for await (const entry of videos.watch(video.id)) {
	send(entry.state, entry.ahead, entry.progress) // waiting 17 … 3 … running 0.4 … done
}
```

```python
async for entry in videos.watch(video.id):
    await send(entry.state, entry.ahead, entry.progress)  # waiting 17 … 3 … running 0.4 … done
```

```go
for entry, err := range videos.Watch(ctx, video.ID) {
	if err != nil {
		return err
	}
	send(entry.State, entry.Ahead, entry.Progress) // waiting 17 … 3 … running 0.4 … done
}
```

`watch` returns the job's current state first, then a new entry each time the
job changes: its place in the queue, its state, its attempt, its progress or
its error. It ends after the entry in which the job is done, failed or
cancelled. Stream the entries to the browser over server-sent events or a
WebSocket, and you have a live status page.

A watcher reads the job at most five times a second, so a watcher behind a
busy queue sees the latest state instead of every change. A key that has no
job ends the watch at once.

## States

| State       | Meaning                                                                                 |
|-------------|-----------------------------------------------------------------------------------------|
| `waiting`   | in the queue; `ahead` says how many jobs run first                                      |
| `running`   | a handler has it; `progress` is its last report                                         |
| `failed`    | failed for good; `error` says why                                                       |
| `done`      | finished, while the queue remembers its key ([`keepDone`](keys.md#run-a-key-only-once)) |
| `cancelled` | cancelled; only a watch shows this state, as its last entry                             |

In Go, the states are `jobs.Waiting`, `jobs.Running`, `jobs.Failed`,
`jobs.Done` and `jobs.Cancelled`.

## Limit how many jobs run at once

`maxRunning` (`MaxRunning`, `max_running`) limits how many jobs of a queue run
at the same time, across every worker of the store. With `maxRunning: 2`, two
videos are transcoded at once, even if ten processes call `work`. The queue
counts running jobs on the server's single writer, so two workers can never
both take the last free place.

## Cancel a running job

```ts
await videos.cancel(video.id) // the handler's job.signal aborts
```

```python
await videos.cancel(video.id)  # the handler's task is cancelled
```

```go
cancelled, err := videos.Cancel(ctx, video.ID) // the handler's context ends with jobs.ErrCancelled
```

The handler should stop when it is cancelled: pass `job.signal` in Bun, or the
context in Go, to the work it does. Whatever the handler returns after a
cancel changes nothing. A watch of the job ends with the `cancelled` state.

## Limits and defaults

|                         |                                        |
|-------------------------|----------------------------------------|
| Jobs counted in `ahead` | up to 10,000                           |
| A progress report       | 4 KiB of JSON                          |
| A watcher's reads       | at most 5 per second                   |
| Running jobs per queue  | unlimited, unless you set `maxRunning` |

## See also

- [Steps](steps.md): keep the results of long steps across retries.
- [jobs/README.md](../../jobs/README.md): the full contract of `get` and
  `watch`.
