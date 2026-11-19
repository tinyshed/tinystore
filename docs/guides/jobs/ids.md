# Ids

An id names one job of a queue, so that a program can find it, change it,
cancel it, or add it once however many times it is asked. Most jobs need
none. A message scheduled for later, a reminder a user edits, a check that
moves each time a device pings: those get an id.

```ts
const later = store.queue<Draft>('later')

const added = await later.add(draft, { id: `chat:${chatId}:${draft.id}`, at: nineAm }) // false: a second tap on "send"
const edited = await later.update(`chat:${chatId}:${draft.id}`, draft, { at: tenAm }) // false: too late, it ran
const cancelled = await later.cancel(`chat:${chatId}:${draft.id}`) // false: there was none
const job = await later.get(`chat:${chatId}:${draft.id}`) // { id, value, state, at, attempt, … } | undefined
```

```python
later = store.queue("later", Draft)

added = await later.add(draft, id=f"chat:{chat_id}:{draft.id}", at=nine_am)  # False: a second tap on "send"
edited = await later.update(f"chat:{chat_id}:{draft.id}", draft, at=ten_am)  # False: too late, it ran
cancelled = await later.cancel(f"chat:{chat_id}:{draft.id}")  # False: there was none
job = await later.get(f"chat:{chat_id}:{draft.id}")  # Job(id, value, state, at, attempt, …) | None
```

```go
later, err := jobs.OpenQueue[Draft](store, "later")

id := fmt.Sprintf("chat:%d:%s", chatID, draft.ID)
added, err := later.Add(ctx, draft, jobs.ID(id), jobs.At(nineAM)) // false: a second tap on "send"
edited, err := later.Update(ctx, id, draft, jobs.At(tenAM))      // false: too late, it ran
cancelled, err := later.Cancel(ctx, id)                           // false: there was none
job, found, err := later.Get(ctx, id)
```

```rust
let later = store.queue::<Draft>("later").open()?;

let id = format!("chat:{chat_id}:{}", draft.id);
let added: bool = later.id(&id).at(nine_am).add(&draft)?; // false: a second tap on "send"
let edited: bool = later.id(&id).at(ten_am).update(&draft)?; // false: too late, it ran
let cancelled: bool = later.cancel(&id)?; // false: there was none
let job = later.get(&id)?;
```

## Four calls, one table

An id is taken while its job is scheduled, waiting or running, and while
`dedupe` keeps it once the job is done. A job that failed for good is kept
for the queue's `keep`, a week, so that you can look at it, but it doesn't
hold its id: the next `add` starts a new job in its place.

| The id's job is               | `add`            | `set`                                        | `update`           | `cancel`                          |
|-------------------------------|------------------|----------------------------------------------|--------------------|-----------------------------------|
| none                          | adds it: `true`  | adds it                                      | nothing: `false`   | `false`                           |
| scheduled, or waiting         | nothing: `false` | replaces its value and time                  | changes it: `true` | removes it: `true`                |
| running                       | nothing: `false` | one run more after this one, with this value | nothing: `false`   | tells its handler to stop: `true` |
| failed, kept for `keep`       | adds it: `true`  | starts it again, its attempts from one       | nothing: `false`   | removes it: `true`                |
| done, while `dedupe` keeps it | nothing: `false` | adds it again                                | nothing: `false`   | `false`                           |

`add`, `update` and `cancel` answer rather than throw: a message being sent
says it is too late to edit, and the program shows that.

An `add` in the place of a failed job starts a new job, its attempts from
one. Until its first run ends, its `error` is the failed one's, so a `get`
still says why the last one failed (contract JOB-03).

A job waiting for a retry is not running, so `update` changes it too. The
answers its steps kept were for the old value, so they go with it, and the
next attempt runs every step again with the new one (contract JOB-04). Its
attempts go on counting.

A `set` of a running job waits for the run under way to end, however it
ends. Then the job starts anew with the set's value, at the set's time or at
once, its attempts counted from one and the earlier run's steps gone. A
repeating job set without a time is the exception: its run under way goes on
through its retries, and the set's run is the repeat's next.

## Debounce and throttle

```ts
await indexing.set(`note:${id}`, { noteId: id }, { delay: '5s' }) // debounce: each edit moves it 5 s on
await reports.add({ team }, { id: `report:${team}`, delay: '1m' }) // throttle: the first call stands
```

```python
await indexing.set(f"note:{id}", Index(note_id=id), delay="5s")  # debounce: each edit moves it 5 s on
await reports.add(Report(team=team), id=f"report:{team}", delay="1m")  # throttle: the first call stands
```

```go
err = indexing.Set(ctx, fmt.Sprint("note:", id), Index{NoteID: id}, jobs.Delay(5*time.Second))           // debounce
_, err = reports.Add(ctx, Report{Team: team}, jobs.ID("report:"+team), jobs.Delay(time.Minute)) // throttle
```

```rust
indexing.id(&format!("note:{id}")).delay(Duration::from_secs(5)).set(&Index { note_id: id })?; // debounce
reports.id(&format!("report:{team}")).delay(Duration::from_mins(1)).add(&Report { team })?; // throttle
```

## Add it, or bring it to the call: upsert

`add` of a taken id changes nothing, so a changed `every` never reaches a
job that is already there. `set` makes the job new, its attempts and time
included, so calling it on every request keeps pushing the run on. `upsert`
adds the job when the id is free, and otherwise changes only what differs:

```ts
const polls = store.queue<{ group: string }>('polls')
const changed = await polls.upsert(`group:${g}`, { group: g }, { every: '3m' }) // true: added or changed; false: it already was so
```

```python
polls = store.queue("polls", Poll)
changed = await polls.upsert(f"group:{g}", Poll(group=g), every="3m")  # True: added or changed
```

```go
polls, err := jobs.OpenQueue[Poll](store, "polls")
changed, err := polls.Upsert(ctx, "group:"+g, Poll{Group: g}, jobs.Every(3*time.Minute))
```

```rust
let polls = store.queue::<Poll>("polls").open()?;
let changed: bool = polls.id(&format!("group:{g}")).every(Duration::from_mins(3)).upsert(&Poll { group: g })?;
```

| The id's job is               | `upsert`                                                      |
|-------------------------------|---------------------------------------------------------------|
| none, or failed               | adds it, as `add` does: `true`                                |
| scheduled, or waiting         | gives it the call's value, group and repeat where they differ |
| running                       | its run goes on, and its next run takes what differs          |
| done, while `dedupe` keeps it | nothing: `false`, as `add`                                    |

- Its time, attempts and steps stay as they are; `at` and `delay` count
  only when it adds the job (contract JOB-17).
- A changed repeat moves the job to the new repeat's next time. A job that
  is due now stays due: it runs now, and after that by the new repeat.
- A changed value drops the steps kept for the old one, as `update` does.
- It answers `false` when the job already was what the call says, and then
  writes nothing, so it is cheap enough to call on every request.
- A running job that ends with its run has no next run to take the change:
  `upsert` answers `false` for it.
- It needs an id, and works in a transaction as `add` does.

## Look now

"Look now" requests that arrive in a burst should become one job, and one
that arrives while the job runs should get a run that sees it. `set` does
both, with no id window:

```ts
await works.set(`now:${course}`, { course }) // many in a row: one job; one during a run: one run more after it
```

```python
await works.set(f"now:{course}", Work(course=course))
```

```go
err = works.Set(ctx, fmt.Sprint("now:", course), Work{Course: course})
```

```rust
works.set(format!("now:{course}"), &Work { course })?;
```

`add` would drop a request made during the run, since the running job holds
its id.

## Once per hour

```ts
const pushes = store.queue<Push>('pushes', { dedupe: '1h' })
await pushes.add(push, { id: `digest:${userId}` }) // a second add within the hour after it ran adds nothing
```

```python
pushes = store.queue("pushes", Push, dedupe="1h")
await pushes.add(push, id=f"digest:{user_id}")
```

```go
pushes, err := jobs.OpenQueue[Push](store, "pushes", jobs.Dedupe(time.Hour))
_, err = pushes.Add(ctx, push, jobs.ID(fmt.Sprint("digest:", userID)))
```

```rust
let pushes = store.queue::<Push>("pushes").dedupe(Duration::from_hours(1)).open()?;
pushes.id(&format!("digest:{user_id}")).add(&push)?;
```

## Once a day, in a user's time zone

`dedupe` also takes a calendar: the id stays taken until the local day,
week or month in which the job was done ends, in a time zone. A job can have
its own `dedupe`, which wins over its queue's:

```ts
const crawl = store.queue<{ group: string }>('schedule-fetch', { dedupe: { calendar: 'day', timeZone: 'Europe/Moscow' } })
await crawl.add({ group: g }, { id: `crawl:${g}` }) // false until Moscow's midnight after it was done
await crawl.add({ group: g }, { id: `now:${g}`, dedupe: '10m' }) // this job's own
```

```python
crawl = store.queue("schedule-fetch", Crawl, dedupe=tinystore.Dedupe(calendar="day", time_zone="Europe/Moscow"))
await crawl.add(Crawl(group=g), id=f"crawl:{g}")
await crawl.add(Crawl(group=g), id=f"now:{g}", dedupe="10m")
```

```go
crawl, err := jobs.OpenQueue[Crawl](store, "schedule-fetch", jobs.DedupeCalendar(tinystore.Day, "Europe/Moscow"))
_, err = crawl.Add(ctx, Crawl{Group: g}, jobs.ID("crawl:"+g))
_, err = crawl.Add(ctx, Crawl{Group: g}, jobs.ID("now:"+g), jobs.Dedupe(10*time.Minute))
```

```rust
let day = Dedupe::calendar(Calendar::Day, "Europe/Moscow");
let crawl = store.queue::<Crawl>("schedule-fetch").dedupe(day).open()?;
crawl.id(&format!("crawl:{g}")).add(&Crawl { group: g })?;
crawl.id(&format!("now:{g}")).dedupe(Duration::from_mins(10)).add(&Crawl { group: g })?;
```

- "Once a day" means once in each local day, not once every 24 hours: a job
  done at 23:59 Moscow time holds its id for a minute.
- A day is 23 or 25 hours long on the days the clock moves (contract
  JOB-18).
- The calendar counts from when the job was done, as a span does. A job that
  failed holds no id, and a repeating job is never done, so `dedupe` does
  nothing to either.
- A zone that doesn't exist and a job's own `dedupe` of zero are `invalid`.

## Keep a failed job, or not

A job that failed for good stays for its queue's `keep`, a week, with its
value and its error, so that you can look at it and `set` it again. A job
can have its own `keep`, and `'0s'` keeps it not at all:

```ts
await pushes.add({ to, text }, { keep: '0s' }) // fails for good: gone at once; its watcher sees failed and the error
await reports.add(report, { id: `report:${id}`, keep: '30d' }) // this job's own, beside the queue's
```

```python
await pushes.add(Push(to=to, text=text), keep="0s")
await reports.add(report, id=f"report:{id}", keep="30d")
```

```go
_, err = pushes.Add(ctx, Push{To: to, Text: text}, jobs.Keep(0))
_, err = reports.Add(ctx, report, jobs.ID("report:"+id), jobs.Keep(30*24*time.Hour))
```

```rust
pushes.keep(Duration::ZERO).add(&Push { to, text })?;
reports.id(&format!("report:{id}")).keep(Duration::from_hours(24 * 30)).add(&report)?;
```

- A job's own `keep` wins over its queue's. A queue's `keep` of zero keeps
  nothing either.
- A job kept not at all leaves as a done one does: `get` finds nothing, and
  its watcher sees `failed` with the error last (contract JOB-20).
- A failed job is gone for `get`, pages and `counts` once its keep has
  passed; maintenance deletes its row within the minute after.
- Gone means unreadable through the store. jobs.db doesn't overwrite deleted
  rows, so the bytes stay in the file's free pages until SQLite reuses them.
  For text that must never rest on disk, put a reference in the value, such
  as the message's id, and read the text in the handler from where it lives.

## Pages of jobs

```ts
const page = await later.list({ prefix: `chat:${chatId}:`, limit: 100 }) // { jobs, next }
const failed = await emails.list({ state: 'failed' }) // the latest failures first
```

```python
page = await later.list(prefix=f"chat:{chat_id}:", limit=100)  # Page(jobs, next)
failed = await emails.list(state="failed")
```

```go
page, err := later.List(ctx, jobs.Prefix(fmt.Sprintf("chat:%d:", chatID)), jobs.Limit(100)) // jobs.Page[Draft]{Jobs, Next}
failed, err := emails.List(ctx, jobs.Failed)
```

A prefix is text: `chat:4` matches chat 42 too, so end a prefix with a
separator.

With no prefix a page lists every job the queue holds: those with ids in the
order of their ids, then those without in the order they run, then those
without that failed, the last failed first. A state narrows any of them, and
`failed` with no prefix lists every failed job, the last failed first. A page
of one state reads on past the jobs in others, so it holds its `limit` unless
it is the last, which has no `next` (contract JOB-14).

A job whose value no longer reads as the queue's type, or its schema, makes
a page or a `get` that meets it fail `corrupt`, and the error names the job
by its id, or says it has none. A walk, `all`, gives that error in the job's
place and goes on to the next job. In Rust and Go the walk yields it and
moves on. In TypeScript and Python the walk's next step throws it, and the
same walk iterated again goes on past it:

```ts
const walk = emails.all()
for (;;) {
	try {
		for await (const job of walk) await resend(job.value)
		break
	} catch (err) {
		if (!(err instanceof CorruptError)) throw err
		log.warn(err.message) // and the loop takes up the same walk again
	}
}
```

```python
walk = emails.all()
while True:
    try:
        async for job in walk:
            await resend(job.value)
        break
    except tinystore.CorruptError as err:
        log.warning(err)  # and the loop takes up the same walk again
```

```go
for job, err := range emails.All(ctx) {
	if errors.Is(err, tinystore.ErrCorrupt) {
		log.Print(err)
		continue
	}
	// ...
}
```

```rust
for job in emails.all(Filter::default()) {
    match job {
        Err(error) if error.kind() == ErrorKind::Corrupt => tracing::warn!("{error}"),
        job => resend(&job?.value)?,
    }
}
```

The same queue opened without a type, or as `serde_json::Value` in Rust,
reads such a job as it is.

## Cancel a job without an id

A job added without an id shows in a page and in `all`. `cancel` takes the
job a page gave, as it takes an id:

```ts
for await (const job of emails.all({ state: 'scheduled' })) {
	if (job.value.to === 'typo@example.com') await emails.cancel(job)
}
```

```python
async for job in emails.all(state="scheduled"):
    if job.value.to == "typo@example.com":
        await emails.cancel(job)
```

```go
for job, err := range emails.All(ctx, jobs.Scheduled) {
	if err == nil && job.Value.To == "typo@example.com" {
		_, err = emails.CancelJob(ctx, job)
	}
}
```

```rust
for job in emails.all(Filter::default().state(State::Scheduled)) {
    let job = job?;
    if job.value.to == "typo@example.com" {
        emails.cancel(&job)?;
    }
}
```

- A job with an id is cancelled by its id. One without is cancelled by where
  it was listed: its queue, its time and a number the store gave it, which no
  other job ever gets, so the cancel never takes another job, one added at the
  same time included (contract JOB-15).
- It answers `true` when the job was there. It answers `false` when the job
  ran, was retried or run again for its repeat since it was listed, or was
  cancelled already. List it again to cancel the retry.
- Go has no overloading, so the call is `CancelJob`, beside `Cancel(ctx, id)`.
- The job keeps where it was listed out of sight. In TypeScript a copy, as
  `{ ...job }`, loses it, and its cancel is `invalid`; cancel the job the page
  gave.
- `cancel` of a job works in a transaction too. `update`, `get` and `watch`
  take an id alone.

## How many jobs are in each state

```ts
const counts = await emails.counts() // { scheduled: 120, waiting: 8, running: 2, failed: 3 }
```

```python
counts = await emails.counts()  # JobCounts(scheduled=120, waiting=8, running=2, failed=3)
```

```go
counts, err := emails.Counts(ctx) // jobs.Counts{Scheduled: 120, Waiting: 8, Running: 2, Failed: 3}
```

```rust
let counts = emails.counts()?; // Counts { scheduled: 120, waiting: 8, running: 2, failed: 3 }
```

Every state is read from one snapshot (contract JOB-16). `scheduled` is due
later, `waiting` is due and no handler has it yet, `running` is in a
handler's hands, and `failed` waits for the queue's `keep`. A done job has
left the queue and is not counted. A count reads no value: it walks the rows
due later and the failed ones, and takes the rest from a total the queue
keeps, so a million due jobs cost nothing more. From a shell,
`tinystore jobs ./data counts emails` prints the same.

| What   | Limit                                        |
|--------|----------------------------------------------|
| an id  | 1 to 1,024 bytes                             |
| a page | 1,000 jobs or 4 MiB, 100 unless `limit` says |

## See also

- [Jobs](README.md): the engine's overview.
- [Watching a job](watching.md): a job's state as it changes.
