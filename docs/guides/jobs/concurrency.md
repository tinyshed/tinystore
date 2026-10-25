# Concurrency and rate

A queue can limit how many of its jobs run at once, in all and per group, and
how many start each second, in all and per group. The limits on running jobs
count every worker of every process that shares the store, so four web
processes with a worker each still send a model at most four requests at
once.

```ts
const refreshes = store.queue<Refresh>('refreshes', { concurrency: { total: 8, group: 2 } })
const telegram = store.queue<Message>('telegram', { rate: '30/s', groupRate: '1/s' })

await refreshes.add({ dataset: 3 }, { group: 'db:42' }) // at most 2 of db:42 at once
```

```python
refreshes = store.queue("refreshes", Refresh, concurrency=tinystore.Concurrency(total=8, group=2))
telegram = store.queue("telegram", Message, rate="30/s", group_rate="1/s")

await refreshes.add(Refresh(dataset=3), group="db:42")  # at most 2 of db:42 at once
```

```go
refreshes, err := jobs.OpenQueue[Refresh](store, "refreshes", jobs.Concurrency{Total: 8, Group: 2})
telegram, err := jobs.OpenQueue[Message](store, "telegram", jobs.Rate(30, time.Second), jobs.GroupRate(1, time.Second))

_, err = refreshes.Add(ctx, Refresh{Dataset: 3}, jobs.Group("db:42")) // at most 2 of db:42 at once
```

```rust
let refreshes = store.queue::<Refresh>("refreshes").concurrency(Concurrency::total(8).group(2)).open()?;
let telegram = store.queue::<Message>("telegram").rate(30, Duration::from_secs(1)).group_rate(1, Duration::from_secs(1)).open()?;

refreshes.group("db:42").add(&Refresh { dataset: 3 })?; // at most 2 of db:42 at once
```

## The limits

| Option                                | Means                                                        |
|---------------------------------------|--------------------------------------------------------------|
| `concurrency: 8`                      | at most 8 jobs of the queue run at once, across every worker |
| `concurrency: { total: 8, group: 2 }` | and at most 2 of each group                                  |
| `group: 1`                            | a group's jobs run one at a time, in their order             |
| `rate: '30/s'`                        | at most 30 jobs start in any second, retries among them      |
| `groupRate: '1/s'`                    | and at most 1 of each group                                  |
| a worker's `concurrency`              | how many this worker runs at once, under the queue's limit   |

Without `concurrency`, a worker runs one job at a time. A job added without a
`group` is bound by the total alone.

## Why groups

One customer's backlog of 10,000 refreshes should not hold back everyone
else. With `group: 2` and the customer's id as the group, that customer gets
two places at a time, and the other six go to other customers. A job of a
full group is set aside and claimed as soon as its group has room.

## How it behaves

- A claim counts the live leases of the queue, and of the group, in the same
  transaction that takes new ones, so two workers in two processes never both
  take the last place.
- A rate counts starts, not running jobs, and lives in memory: a restart
  forgets it. It is for an API's limit, such as a messenger's 30 messages a
  second.
- A group's rate is for each account's own limit at a provider: a messenger
  lets each bot send its own messages a second. Give each bot's messages
  its group. A job of a group past its rate waits for its group's turn,
  and the other groups' jobs start meanwhile.
- A rate is counted by the process that runs the worker. Two processes that
  work one queue each keep their own count, so together they may start up
  to twice the rate.

## See also

- [Jobs](README.md): the engine's overview.
- [How a job lives](../../handbook/inside/how-a-job-lives.md#limits-across-processes).
