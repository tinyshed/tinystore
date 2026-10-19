# Watching a job

A program can ask where a job is, how many jobs are ahead of it, and how far
it got, and follow it as it changes: "3 ahead", then "thinking", then done.
There is no status table to keep in step with the queue: the queue knows.

```ts
const job = await videos.get(id) // { id, value, state, at, attempt, ahead, progress, error, … } | undefined

for await (const job of videos.watch(id)) {
	send({ state: job.state, ahead: job.ahead, progress: job.progress }) // waiting, 3 ahead … running, 0.4 … done
}

videos.work(async (video, run) => {
	await transcode(video, done => run.setProgress(done)) // shown to get and watch
})
```

```python
job = await videos.get(id)  # Job(id, value, state, at, attempt, ahead, progress, …) | None

async for job in videos.watch(id):
    send(job.state, job.ahead, job.progress)


async def convert(video: Video, run: tinystore.Run) -> None:
    await transcode(video, run.set_progress)


videos.work(convert)
```

```go
job, found, err := videos.Get(ctx, id)

for job, err := range videos.Watch(ctx, id) {
	if err != nil {
		return err
	}
	send(job.State, job.Ahead, job.Progress)
}

err = videos.Work(ctx, func(ctx context.Context, v Video, run *jobs.Run) error {
	return transcode(ctx, v, func(done float64) { run.SetProgress(done) })
})
```

```rust
let job = videos.get(&id)?;

for job in videos.watch(&id) {
    let job = job?;
    send(job.state, job.ahead, job.progress);
}
```

## What a job says

| Field      | Means                                                         |
|------------|---------------------------------------------------------------|
| `state`    | `scheduled`, `waiting`, `running`, `failed` or `done`         |
| `at`       | when it runs next                                             |
| `ahead`    | the jobs that run before a waiting one, counted up to 10,000  |
| `attempt`  | this run's number, the first being 1                          |
| `progress` | what the handler set with `setProgress`, any JSON up to 4 KiB |
| `error`    | why its last run failed                                       |
| `lastRun`  | when the last finished run started and ended                  |

`get` returns nothing for an id with no job. A done job without `dedupe` is
gone, so `get` returns nothing for it too.

## How a watch behaves

- `watch` yields the job as it is, then again each time it changes, and ends
  with it: `done`, `failed`, or `cancelled`, a state only a watcher sees.
- A watch whose connection drops watches again on the next one; a job that
  ended meanwhile ends the watch without its last state.
- Progress lives in memory, ten updates a second at most: a run that a
  restart ends starts its progress again from nothing.

Stream the states to a browser over server-sent events, and a user sees the
queue move.

## See also

- [Jobs](README.md): the engine's overview.
- [Show the reply's progress](../../handbook/chat/replies.md#show-the-replys-progress)
  in the Handbook.
