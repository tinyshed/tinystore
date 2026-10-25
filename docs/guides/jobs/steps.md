# Steps

A step is a part of a job whose answer is kept across the job's attempts. An
agent that searched, then asked a model, then failed while saving does not
search or ask again on its retry: the steps it finished return their kept
answers, and only what is left runs.

```ts
agents.work(async (question, run) => {
	const found = await run.step('search', () => search(question.text))
	const answer = await run.step('answer', () => model.answer(question.text, found))
	await reply(question.chat, answer)
})
```

```python
async def answer(question: Question, run: tinystore.Run) -> None:
    found = await run.step("search", lambda: search(question.text))
    reply_text = await run.step("answer", lambda: model.answer(question.text, found))
    await reply(question.chat, reply_text)


agents.work(answer)
```

```go
err = agents.Work(ctx, func(ctx context.Context, q Question, run *jobs.Run) error {
	found, err := jobs.Step(ctx, run, "search", func(ctx context.Context) ([]Doc, error) { return search(ctx, q.Text) })
	if err != nil {
		return err
	}
	answer, err := jobs.Step(ctx, run, "answer", func(ctx context.Context) (string, error) { return model.Answer(ctx, q.Text, found) })
	if err != nil {
		return err
	}
	return reply(ctx, q.Chat, answer)
})
```

```rust
let worker = agents.work(|question: Question, run| -> Result<(), AppError> {
    let found: Vec<Doc> = run.step("search", || search(&question.text))?;
    let answer: String = run.step("answer", || model.answer(&question.text, &found))?;
    reply(&question.chat, &answer)
})?;
```

## How steps behave

- `run.step(name, fn)` runs `fn` once for the job and keeps its answer as
  JSON. The attempt after a retry, a lost worker or a restart gets the answer
  back without running `fn` again (contract JOB-06).
- A step whose attempt ended before its answer was kept runs again, so what
  `fn` does outside the store should bear doing twice: pass an idempotency
  key to a payment provider, use `on conflict do nothing` in SQL. A metrics
  [ingest sent again](../metrics/ingesting.md#an-ingest-sent-again) changes
  nothing.
- A name is a step's within its job; number steps in a loop, `tool:1`,
  `tool:2`.
- A job that ends, done, failed or cancelled, takes its steps with it, and a
  repeat's next time starts without them.
- Go's is `jobs.Step(ctx, run, name, fn)`, a function, because the answer's
  type is the step's.

## Retry, snooze and fail

A handler's answer decides what happens to the job:

| The handler                 | The job                                                          |
|-----------------------------|------------------------------------------------------------------|
| returns                     | is done                                                          |
| throws, or returns an error | runs again after its backoff; after `attempts` it fails for good |
| returns `run.retry('10m')`  | runs again in ten minutes, counted as an attempt                 |
| returns `run.snooze('10m')` | runs again in ten minutes, not counted                           |
| returns `run.fail(reason)`  | fails for good now, with the reason as its error                 |

```ts
pushes.work(async (push, run) => {
	const answer = await provider.send(push)
	if (answer.unknownToken) return run.fail('the provider no longer knows this token')
	if (answer.retryAt) return run.snooze(answer.retryAt)
})
```

```python
async def send(push: Push, run: tinystore.Run) -> tinystore.Outcome | None:
    answer = await provider.send(push)
    if answer.unknown_token:
        return run.fail("the provider no longer knows this token")
    if answer.retry_at:
        return run.snooze(answer.retry_at)
    return None
```

```go
err = pushes.Work(ctx, func(ctx context.Context, p Push, run *jobs.Run) error {
	answer, err := provider.Send(ctx, p)
	switch {
	case err != nil:
		return err
	case answer.UnknownToken:
		return run.Fail("the provider no longer knows this token")
	case !answer.RetryAt.IsZero():
		return run.SnoozeUntil(answer.RetryAt)
	}
	return nil
})
```

| Option          | Default                                                           |
|-----------------|-------------------------------------------------------------------|
| `attempts`      | 10                                                                |
| `backoff`       | from 1 s, doubling up to 1 h, a tenth longer or shorter at random |
| `timeout`       | 1 minute per run                                                  |
| a step's answer | 1 MiB of JSON                                                     |

## See also

- [Jobs](README.md): the engine's overview.
- [What a crash does](../../handbook/chat/replies.md#what-a-crash-does) in
  the Handbook, with a scene.
