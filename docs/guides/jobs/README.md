# Jobs

The Jobs engine runs work later, on a schedule, or now but outside the
request that asked for it: an email in the background, a reminder at six, a
cleanup every night. A queue keeps its jobs in `jobs.db`, or in an
application's database so that a job commits with its rows. A worker runs
each job, retries it when it fails, and keeps the steps it finished.

```ts
const emails = store.queue<Email>('emails', { attempts: 5, concurrency: 4 })

await emails.add({ to: 'ada@example.com', subject: 'Welcome' }, { delay: '1m' })
emails.work(async (email, run) => {
	const body = await run.step('render', () => render(email))
	await smtp.send(email.to, body)
})
```

```python
emails = store.queue("emails", Email, attempts=5, concurrency=4)

await emails.add(Email(to="ada@example.com", subject="Welcome"), delay="1m")


async def send(email: Email, run: tinystore.Run) -> None:
    body = await run.step("render", lambda: render(email))
    await smtp.send(email.to, body)


emails.work(send)
```

```go
emails, err := jobs.OpenQueue[Email](store, "emails", jobs.Attempts(5), jobs.Concurrency{Total: 4})

_, err = emails.Add(ctx, Email{To: "ada@example.com", Subject: "Welcome"}, jobs.Delay(time.Minute))
err = emails.Work(ctx, func(ctx context.Context, email Email, run *jobs.Run) error {
	body, err := jobs.Step(ctx, run, "render", func(ctx context.Context) (string, error) { return render(email) })
	if err != nil {
		return err
	}
	return smtp.Send(ctx, email.To, body)
})
```

```rust
let emails = store.queue::<Email>("emails").attempts(5).concurrency(Concurrency::total(4)).open()?;

emails.delay(Duration::from_mins(1)).add(&Email { to: "ada@example.com".into(), subject: "Welcome".into() })?;
let worker = emails.work(|email: Email, run| -> Result<(), AppError> {
    let body: String = run.step("render", || render(&email))?;
    smtp.send(&email.to, &body)
})?;
```

## What it guarantees

- A job is on the disk before `add` returns, and survives any crash after
  (contract DUR-01).
- A job never runs before its time, and due jobs start in the order of their
  time (contract JOB-01).
- A job added in a transaction commits with its rows, or not at all
  (contract JOB-02).
- A job runs at least once: a run that ends before it is settled runs again,
  and its stale lease settles nothing, so an `update` or a `set` made once
  the lease ended runs (contract JOB-05).
- A step runs once per job, across its attempts (contract JOB-06).
- `concurrency` and `rate` count every worker of every process.

## In these guides

| Page                                      | Covers                                             |
|-------------------------------------------|----------------------------------------------------|
| [Ids](ids.md)                             | `id`: add once, `set`, `update`, `cancel`, `get`   |
| [Schedules and cron](schedules.md)        | `every`, `cron` with a time zone, `store.schedule` |
| [Concurrency and rate](concurrency.md)    | limits in all and per group, starts per second     |
| [Watching a job](watching.md)             | `get`, `watch`, `ahead`, progress                  |
| [Steps](steps.md)                         | answers kept across attempts                       |
| [Jobs in your database](your-database.md) | `db.queue` and transactions                        |
| [Testing](testing.md)                     | `runDue` and a clock you move                      |

New to Jobs? [Replies in the background](../../handbook/chat/replies.md) in
the Handbook writes a chat's replies with a queue, steps and a watch, and
[How a job lives](../../handbook/inside/how-a-job-lives.md) explains leases
and crashes.

## Async Rust

A Rust program that runs on tokio or another executor can await its queues
instead of blocking a thread. Turn on the `async` feature and open the store
from `tinystore::aio::Store`. A queue opens with the same builder, and each
call that waits returns a future. This section shows only Rust, because in
Bun, Node and Python every call is already awaited, and in Go a call blocks
its goroutine.

```rust
use tinystore::aio::Store;
use tinystore::aio::jobs::Run;

async fn send(email: Email, run: Run) -> Result<(), AppError> {
    let body: String = run.step("render", || render(&email)).await?;
    smtp.send(&email.to, &body).await
}

let store = Store::open("data", Options::default())?;
let emails = store.queue::<Email>("emails").attempts(5).open()?; // a plain call, as in blocking Rust

emails.delay(Duration::from_mins(1)).add(&email).await?;
let worker = tokio::spawn(emails.work(send).until(tokio::signal::ctrl_c()));
worker.await??; // after ctrl-c: the handlers under way finished, and their jobs are settled
```

Each async call has the name of its blocking twin:

| Blocking                                    | Async                                                   |
|---------------------------------------------|---------------------------------------------------------|
| `emails.add(&email)?`                       | `emails.add(&email).await?`                             |
| `emails.id(&id).delay(span).add(&email)?`   | `emails.id(&id).delay(span).add(&email).await?`         |
| `emails.get(&id)?`, `emails.list(&filter)?` | `emails.get(&id).await?`, `emails.list(&filter).await?` |
| `for job in emails.all(filter)`             | `while let Some(job) = all.next().await`                |
| `for job in emails.watch(&id)`              | `while let Some(job) = watch.next().await`              |
| `let worker = emails.work(send)?`           | `let worker = tokio::spawn(emails.work(send))`          |
| `worker.stop()`                             | `emails.work(send).until(signal)`                       |
| `emails.run_due(send)?`                     | `emails.run_due(send).await?`                           |
| `run.step("render", work)?`                 | `run.step("render", work).await?`                       |
| `imports.claim()?`, `claimed.settle(())?`   | `imports.claim().await?`, `claimed.settle(()).await?`   |

`all` and `watch` return a `Stream`. Call `next` on it from
`tokio_stream::StreamExt` or `futures::StreamExt`. A handler is an `async`
function that takes the job's value and its `Run` by value, so that a
multi-threaded runtime can move its future between threads.

### A worker is a future

`work` returns a worker at once and starts nothing. Spawn it on your runtime,
and its handlers run inside its future, as many at once as the worker's
concurrency. The loop that claims and settles jobs runs on one of the
store's own threads, so a hundred finished jobs still share one commit.

- `until(signal)` stops the worker gracefully. Once the signal is ready, it
  takes no more jobs, waits for the handlers under way, and returns after
  their jobs are settled. Jobs that it claimed and had not started go back
  to the queue, and their attempt is not counted (contract JOB-11).
- Dropping the worker, for example with `JoinHandle::abort`, stops it at
  once. Jobs that it had not started go back uncounted. A job whose handler
  was running runs again after its backoff, and its attempt is counted.
- When the store closes, every worker stops as `until` stops it, and its
  future returns `Ok(())`. Close the store with `store.close().await`.
  A blocking `close` on your executor's only thread can't poll the
  handlers, so it waits for the queue's timeout and ten seconds more.

A cancel of a running job makes `run.stopped()` return `true`, as in the
blocking API. The worker doesn't drop the handler's future, and what the
handler returns changes nothing.

### Where the calls run

`add`, `set`, `update`, `cancel` and `settle` queue their write at
`jobs.db`'s writer in the first poll, and the commit runs on the store's
threads. A write future that was polled once commits even if you drop it
(contract DUR-11). `get`, `list` and the reads of `all` and `watch` run on
the store's threads, and a dropped future stops its read (contract
STORE-09). No call commits, syncs the disk or waits on your executor's
thread (contract STORE-10).

A watch reads its job again each time the queue commits. Unlike the blocking
watch, it doesn't look again every minute, so a scheduled job that becomes
due shows up when a worker claims it.

Inside a transaction's closure, a queue's calls are the blocking ones, as
in blocking Rust:

```rust
let queue = emails.clone(); // the closure runs on the store's threads and owns what it takes
store.tx(move |tx| tx.with(&queue).id("welcome:42").add(&email)).await?;
```

[Rust with async](../../handbook/start/rust-async.md) in the Handbook
explains the async API's shape and what dropping a future does.

## Reference

[Bun and Node](../../reference/bun/jobs.md) ·
[Python](../../reference/python/jobs.md) ·
[Go](../../reference/go/jobs.md) ·
[Rust](../../reference/rust/jobs.md)
