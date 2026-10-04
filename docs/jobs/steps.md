# Steps

A step saves the result of one part of a job. If the job fails later and runs
again, the step returns its saved result instead of running again. Use steps
for jobs that call a model and its tools, so that a retry doesn't pay for the
same search or the same model call twice.

## Keep the result of each step

```ts
await questions.work(async job => {
	const hits = await job.step('search', () => search(job.value.text))
	const answer = await job.step('answer', () => model.answer(job.value.text, hits))
	await reply(job.value.chatId, answer)
})
```

```python
async def answer_question(job: tinystore.Job[Question]) -> None:
    hits = await job.step("search", lambda: search(job.value.text))
    answer = await job.step("answer", lambda: model.answer(job.value.text, hits))
    await reply(job.value.chat_id, answer)


await questions.work(answer_question)
```

```go
err = questions.Work(ctx, func(ctx context.Context, job jobs.Job[Question]) error {
	hits, err := jobs.Step(ctx, job, "search", func(ctx context.Context) ([]Hit, error) {
		return search(ctx, job.Value.Text)
	})
	if err != nil {
		return err
	}
	answer, err := jobs.Step(ctx, job, "answer", func(ctx context.Context) (string, error) {
		return model.Answer(ctx, job.Value.Text, hits)
	})
	if err != nil {
		return err
	}
	return reply(ctx, job.Value.ChatID, answer)
})
```

Suppose the model call fails. The job is retried, the handler runs from the
top, and `step('search')` returns the saved hits at once. Only the model call
runs again. If the worker's process dies between the steps, the next worker
continues the same way.

A step's result is saved as JSON next to the job, in the same file. In Go,
`Step` is a function and not a method, because a Go method can't have its own
type parameter.

## Name steps in a loop

An agent that calls tools in a loop gives each step a unique name within the
run, for example by numbering them:

```ts
for (let turn = 1; !done; turn++) {
	const reply = await job.step(`model:${turn}`, () => model.next(history))
	for (const [i, call] of reply.toolCalls.entries()) {
		history.push(await job.step(`tool:${turn}:${i}`, () => runTool(call)))
	}
	done = reply.done
}
```

```python
turn = 1
while not done:
    reply = await job.step(f"model:{turn}", lambda: model.next(history))
    for i, call in enumerate(reply.tool_calls):
        history.append(await job.step(f"tool:{turn}:{i}", lambda: run_tool(call)))
    done = reply.done
    turn += 1
```

```go
for turn := 1; !done; turn++ {
	reply, err := jobs.Step(ctx, job, fmt.Sprintf("model:%d", turn), func(ctx context.Context) (Reply, error) {
		return model.Next(ctx, history)
	})
	if err != nil {
		return err
	}
	for i, call := range reply.ToolCalls {
		result, err := jobs.Step(ctx, job, fmt.Sprintf("tool:%d:%d", turn, i), func(ctx context.Context) (ToolResult, error) {
			return runTool(ctx, call)
		})
		if err != nil {
			return err
		}
		history = append(history, result)
	}
	done = reply.Done
}
```

On a retry, every finished step returns its saved result in the same order,
and the loop continues from the first step that didn't finish.

## What a step guarantees

- **A step runs at least once.** If the attempt ends before the result is
  saved, the step runs again in the next attempt. Whatever a step does outside
  the store, such as charging a card, should be safe to do twice, or carry an
  idempotency key.
- **Only the current attempt can save a step.** If a worker stalls, its lease
  runs out and another worker takes the job, the stalled worker's results are
  not saved.
- **Steps belong to one run.** When the job is done, failed for good or
  cancelled, its steps are deleted with it. A repeating job's next run starts
  without saved steps.
- Retries, snoozes and lost workers keep the steps, because they are the same
  run.

## Limits and defaults

|                 |                                       |
|-----------------|---------------------------------------|
| A step's name   | 1 to 256 bytes, unique within the run |
| A step's result | 1 MiB of JSON                         |

A result that can't be encoded as JSON fails with an invalid argument error
(`ErrInvalid`, `InvalidError`).

## See also

- [Watching a job](watching.md): report progress from a long handler.
- [jobs/README.md](../../jobs/README.md): the full contract of steps.
