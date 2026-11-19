# Testing jobs

A test of a job should not wait for its time, and should not leave a worker
running after it ends. A test's store runs on a clock the test moves, and
`runDue` runs what is due, then returns.

```ts
import { openTemp } from '@tinyshed/tinystore/testing'

await using store = await openTemp({ clock: new Date('2026-10-09T09:00:00Z') })
const reminders = store.queue<Reminder>('reminders')

await reminders.add({ userId: 42, text: 'Call mom' }, { delay: '1h' })
await reminders.runDue(remind) // 0: nothing is due yet
await store.clock.advance('1h')
await reminders.runDue(remind) // 1: it ran
```

```python
from tinystore.testing import open_temp

async with open_temp(clock=datetime(2026, 10, 9, 9, tzinfo=UTC)) as store:
    reminders = store.queue("reminders", Reminder)

    await reminders.add(Reminder(user_id=42, text="Call mom"), delay="1h")
    await reminders.run_due(remind)  # 0: nothing is due yet
    await store.clock.advance("1h")
    await reminders.run_due(remind)  # 1: it ran
```

```go
store := tinystoretest.Open(t, tinystore.Clock(time.Date(2026, 10, 9, 9, 0, 0, 0, time.UTC)))
reminders, err := jobs.OpenQueue[Reminder](store, "reminders")

_, err = reminders.Add(ctx, Reminder{UserID: 42, Text: "Call mom"}, jobs.Delay(time.Hour))
err = reminders.RunDue(ctx, remind) // nothing is due yet
err = store.Clock().Advance(ctx, time.Hour)
err = reminders.RunDue(ctx, remind) // it ran
```

```rust
let clock = Arc::new(TestClock::new(start));
let store = tinystore::testing::open_temp(Options { clock: Some(clock.clone()), background: false, ..Options::default() })?;
let reminders = store.queue::<Reminder>("reminders").open()?;

reminders.delay(Duration::from_hours(1)).add(&reminder)?;
reminders.run_due(remind)?; // 0: nothing is due yet
clock.advance(Duration::from_hours(1));
reminders.run_due(remind)?; // 1: it ran
```

## What to test

- **That a job is added.** `get` an id, or `list` a prefix, right after the
  call under test.
- **That the handler does its work.** Call `runDue` with the real handler and
  check its effect: the row, the email in a fake mailer.
- **Retries.** A handler that throws on its first attempt: advance the clock
  past the backoff and `runDue` again. Steps it finished do not run twice.
- **Schedules.** Advance the clock to the cron's next time and `runDue` the
  schedule.

## A store for each test

`openTemp` (`open_temp` in Python and Rust, `tinystoretest.Open` in Go)
opens a store in a new, empty directory under the system's temporary one,
named `tinystore-` and a random part. Its close removes the directory
(contract STORE-12): `await using` in Bun and Node, `async with` in Python,
`t.Cleanup` in Go, the drop in Rust. `store.dir` (`store.dir()` in Rust)
gives the path for a test that looks at its files.

- It opens embedded, with no process and no socket, the fastest way.
  `{ private: true }` (`tinystore.Private()` in Go) tests a child instead.
  Every other option of `open` passes through.
- A file still held a second after the close fails the close, `in use` in
  Bun, Node and Python, `io` in Rust, and the test in Go: the test leaked a
  handle. In Rust, a drop says nothing; call `close()` for the error.
- Rust needs the crate's `testing` feature, in `[dev-dependencies]`.

## How it behaves

- `runDue` runs what is due when it starts and what falls due meanwhile, then
  returns how many ran. It never waits for a later job.
- The clock moves every engine together: a key's `ttl`, a quota's windows and
  a log line's time follow it too. It never moves backwards.
- A test's store is the test's alone, so tests run in parallel without
  sharing a sidecar. A store opened with `clock` is private or embedded,
  never a shared sidecar. A Rust test passes `background: false` and runs
  each engine's maintenance itself.

## See also

- [Jobs](README.md): the engine's overview.
- [Test on a clock you move](../../handbook/chat/keeping-it.md#test-on-a-clock-you-move)
  in the Handbook.
