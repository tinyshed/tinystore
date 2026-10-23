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

An id is taken while its job is scheduled, waiting, running or failed, and
while `dedupe` keeps it once the job is done.

| The id's job is               | `add`            | `set`                                        | `update`           | `cancel`                          |
|-------------------------------|------------------|----------------------------------------------|--------------------|-----------------------------------|
| none                          | adds it: `true`  | adds it                                      | nothing: `false`   | `false`                           |
| scheduled, or waiting         | nothing: `false` | replaces its value and time                  | changes it: `true` | removes it: `true`                |
| running                       | nothing: `false` | one run more after this one, with this value | nothing: `false`   | tells its handler to stop: `true` |
| failed, kept for `keep`       | nothing: `false` | starts it again, its attempts from one       | nothing: `false`   | removes it: `true`                |
| done, while `dedupe` keeps it | nothing: `false` | adds it again                                | nothing: `false`   | `false`                           |

`add`, `update` and `cancel` answer rather than throw: a message being sent
says it is too late to edit, and the program shows that.

A `set` of a running job waits for the run under way to end, whether it is
done, fails, retries or snoozes, and then starts the job anew: with the set's
value, at the time the set names or else at once, its attempts counted from
one and the steps of the run before gone. A job that repeats and is set
without a time is the exception: its run under way goes on through its
retries, and the set's run is the repeat's next.

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

With `dedupe`, a done job keeps its id taken for that long, so `add` of the
same id adds nothing; `set` arms it again.

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

In Rust, a job whose value no longer reads as the queue's type, kept failed
after the type changed, makes a page or a `get` that meets it fail `corrupt`,
and the error names the job's id. The same queue opened as
`store.queue::<serde_json::Value>(name)` reads it, and `cancel` removes it.

| What   | Limit                                        |
|--------|----------------------------------------------|
| an id  | 1 to 1,024 bytes                             |
| a page | 1,000 jobs or 4 MiB, 100 unless `limit` says |

## See also

- [Jobs](README.md): the engine's overview.
- [Watching a job](watching.md): a job's state as it changes.
