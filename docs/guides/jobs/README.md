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
  and its stale lease settles nothing (contract JOB-05).
- A step runs once per job, across its attempts (contract JOB-06).
- `concurrency` and `rate` count every worker of every process.

## In these guides

| Page                  | Covers                                             |
|-----------------------|----------------------------------------------------|
| Ids                   | `id`: add once, `set`, `update`, `cancel`, `get`   |
| Schedules and cron    | `every`, `cron` with a time zone, `store.schedule` |
| Concurrency and rate  | limits in all and per group, starts per second     |
| Watching a job        | `get`, `watch`, `ahead`, progress                  |
| Steps                 | answers kept across attempts                       |
| Jobs in your database | `db.queue` and transactions                        |
| Testing               | `runDue` and a clock you move                      |

New to Jobs? [Replies in the background](../../handbook/chat/replies.md) in
the Handbook writes a chat's replies with a queue, steps and a watch, and
[How a job lives](../../handbook/inside/how-a-job-lives.md) explains leases
and crashes.

## Reference

[Bun and Node](../../reference/bun/jobs.md) ·
[Python](../../reference/python/jobs.md) ·
[Go](../../reference/go/jobs.md) ·
[Rust](../../reference/rust/jobs.md)
