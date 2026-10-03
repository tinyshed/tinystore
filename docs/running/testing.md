# Testing

Give each test its own store in a temporary directory. Also give the store a
clock that the test controls, so that expiry, retries and schedules happen
exactly when the test says, without sleeping.

## A store per test

```ts
import { afterAll, beforeAll, expect, test } from 'bun:test'
import { mkdtempSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { open, type Store } from '@tinyshed/tinystore'

let store: Store
beforeAll(async () => {
	store = await open(mkdtempSync(join(tmpdir(), 'app-')), { private: true })
})
afterAll(() => store.close())

test('a code works once', async () => {
	const codes = store.kv.bucket('codes', 'int')
	await codes.set('K7Q2', 42)
	expect(await codes.take('K7Q2')).toBe(42)
	expect(await codes.take('K7Q2')).toBeUndefined()
})
```

```python
import pytest
import tinystore


@pytest.fixture
async def store(tmp_path):
    async with tinystore.open(tmp_path, private=True) as store:
        yield store


async def test_a_code_works_once(store):
    codes = store.kv.bucket("codes", int)
    await codes.set("K7Q2", 42)
    assert await codes.take("K7Q2") == 42
    assert await codes.take("K7Q2") is None
```

```go
func TestACodeWorksOnce(t *testing.T) {
	ctx := t.Context()
	store, err := tinystore.Open(ctx, t.TempDir(), tinystore.Options{})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { store.Close(context.Background()) })

	state, _ := kv.Open(ctx, store, kv.Options{})
	codes, _ := kv.OpenBucket[int64](ctx, state, "codes")
	_ = codes.Set(ctx, "K7Q2", 42)
	if v, found, _ := codes.Take(ctx, "K7Q2"); !found || v != 42 {
		t.Fatalf("took %d, %v", v, found)
	}
}
```

In Bun and Python, `private` starts a server for the test alone, connected
through its stdin and stdout. Tests in different files can run in parallel,
because each has its own directory and its own server.

## Control time

```ts
await using store = await open(dir, { private: true, clock: new Date('2026-10-03T09:00:00Z') })
const reminders = store.jobs.queue<Reminder>('reminders')

await reminders.enqueue({ userId: 42 }, { after: '1h' })
await reminders.work(remind, { untilIdle: true }) // nothing is due yet

await store.clock.advance('1h')
await reminders.work(remind, { untilIdle: true }) // runs the reminder
```

```python
start = datetime(2026, 10, 3, 9, tzinfo=UTC)
async with tinystore.open(tmp_path, private=True, clock=start) as store:
    reminders = store.jobs.queue("reminders", Reminder)

    await reminders.enqueue(Reminder(user_id=42), after="1h")
    await reminders.work(remind, until_idle=True)  # nothing is due yet

    await store.clock.advance("1h")
    await reminders.work(remind, until_idle=True)  # runs the reminder
```

```go
now := time.Date(2026, 10, 3, 9, 0, 0, 0, time.UTC)
store, err := tinystore.Open(ctx, t.TempDir(), tinystore.Options{
	Manual: true,
	Clock:  func() time.Time { return now },
})
queues, err := jobs.Open(ctx, store, jobs.Options{})
reminders, err := jobs.OpenQueue[Reminder](ctx, queues, "reminders")

err = reminders.Enqueue(ctx, Reminder{UserID: 42}, jobs.After(time.Hour))
err = reminders.Work(ctx, remind, jobs.UntilIdle()) // nothing is due yet

now = now.Add(time.Hour)
err = reminders.Work(ctx, remind, jobs.UntilIdle()) // runs the reminder
```

The store runs on a clock that the test moves. Move it forward, and keys
expire, jobs become due and retention removes old records, without waiting.

- **`clock`** starts a private server on a test's clock, at the time you give.
  It needs `private`, because a shared sidecar runs on the system's time.
  `store.clock.advance` moves the clock forward by a duration,
  `store.clock.set` moves it to a later time, and `store.clock.now` reads it.
  A clock never moves back: an earlier time fails with an invalid error.
- **`Clock`** in Go replaces the store's clock for every engine. Any function
  that returns a time works. The test moves it between calls, as `now` above.
- **`Manual`** in Go turns off all background work. Call each engine's
  `Maintain` when the test wants expired keys deleted, records sealed or old
  jobs removed, and `Flush` on metrics instead of waiting for the interval.
- **`untilIdle`** (`until_idle` in Python, `UntilIdle` in Go) makes `work`
  run every due job and return. Use it after you move the clock: a `work` loop
  that is already waiting for a job doesn't notice that the clock moved.

> [!NOTE]
> **Times your program writes**
> The clock moves only the store's time. A time that your program writes, such
> as a log line's or a sample's, still comes from your program's clock. After
> you move the store's clock far ahead, the store can refuse such a time as too
> old.

## See also

- [Expiry](../kv/expiry.md): testing expiry without waiting.
- [Go, Bun and Python](../languages.md): private servers.
