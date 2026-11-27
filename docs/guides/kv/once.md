# Once

`once` runs a function once per key and keeps its answer: a payment
requested twice by a flaky network charges once, and the second request gets
the same receipt. With a short `keep`, it is a memo that many callers share:
the dashboard's panels are computed once for everyone who asks at once.

```ts
const charges = store.once<Receipt>('charges', { keep: '1d' })

const receipt = await charges.run(request.headers.get('idempotency-key')!, () =>
	stripe.charge(order, { idempotencyKey: order.id }),
)
```

```python
charges = store.once("charges", Receipt, keep="1d")

receipt = await charges.run(request.headers["idempotency-key"], lambda: stripe.charge(order, idempotency_key=order.id))
```

```go
charges, err := kv.OpenOnce[Receipt](store, "charges", kv.Keep(24*time.Hour))

receipt, err := charges.Run(ctx, r.Header.Get("Idempotency-Key"), func(ctx context.Context) (Receipt, error) {
	return stripe.Charge(ctx, order, order.ID)
})
```

```rust
let charges = store.once::<Receipt>("charges").keep(Duration::from_hours(24)).open()?;

let receipt = charges.run(&idempotency_key, || stripe.charge(&order, &order.id))?;
```

## What `run` does

| When `run(key, fn)` is called | It                                                                     |
|-------------------------------|------------------------------------------------------------------------|
| the key has a kept answer     | returns it, and runs nothing                                           |
| the key is running elsewhere  | waits for that run, in this process or another, and returns its answer |
| the key is free               | runs `fn`, keeps its answer for `keep`, and returns it                 |
| `fn` throws                   | keeps nothing: the next `run` runs again                               |
| `fn`'s answer is over 1 MiB   | returns it to this run and the runs that waited, and keeps nothing     |

A run of the same key inside its own run, awaited or blocking, is `invalid`,
because it would wait for itself (contract DUR-07).

## A memo

```ts
const panels = store.once<Panel[]>('panels', { keep: '30s' })
const data = await panels.run(`dashboard:${id}`, () => computePanels(id))
```

```python
panels = store.once("panels", list[Panel], keep="30s")
data = await panels.run(f"dashboard:{id}", lambda: compute_panels(id))
```

```go
panels, err := kv.OpenOnce[[]Panel](store, "panels", kv.Keep(30*time.Second))
data, err := panels.Run(ctx, "dashboard:"+id, func(ctx context.Context) ([]Panel, error) { return computePanels(ctx, id) })
```

```rust
let panels = store.once::<Vec<Panel>>("panels").keep(Duration::from_secs(30)).open()?;
let data = panels.run(&format!("dashboard:{id}"), || compute_panels(&id))?;
```

Ten users who open the dashboard in the same second share one computation,
and the next 30 seconds answer from what it kept.

Each kept answer is a write to `kv.db`, synced like any other. An answer over
1 MiB still reaches the users who waited for it, but the next run computes it
again. A run waits for the one under way however long it takes, so a memo of
a slow query that callers may give up on belongs in the program's memory.

## An answer kept while a new one is made

A memo with only `keep` makes every caller wait the whole build again once
`keep` ends, and has nothing to answer with when the build fails. Give it a
second term, `fresh`, shorter than `keep`: an answer younger than `fresh` is
answered as it is, and an older one is answered at once while one new answer
is made in the background. A build that fails leaves the kept answer where it
is until `keep` ends, so a source that is down still gets the last good one.

```ts
const books = store.once<GradeBook>('cabinet-books', { fresh: '10m', keep: '12h' })

const { value, age, stale } = await books.fetch(String(userId), () => cabinet.book(userId))
const book = await books.run(String(userId), () => cabinet.book(userId)) // the value alone
await books.under(courseId).clear() // a branch and every branch under it, in one commit
```

```python
books = store.once("cabinet-books", GradeBook, fresh="10m", keep="12h")

answer = await books.fetch(str(user_id), lambda: cabinet.book(user_id))  # answer.value, .age, .stale
book = await books.run(str(user_id), lambda: cabinet.book(user_id))
await books.under(course_id).clear()
```

```go
books, err := kv.OpenOnce[GradeBook](store, "cabinet-books", kv.Fresh(10*time.Minute), kv.Keep(12*time.Hour))

answer, err := books.Fetch(ctx, strconv.Itoa(userID), func(ctx context.Context) (GradeBook, error) { return cabinet.Book(ctx, userID) })
// answer.Value, answer.Age, answer.Stale
err = books.Under(courseID).Clear(ctx)
```

```rust
let books = store.once::<GradeBook>("cabinet-books").fresh(Duration::from_mins(10)).keep(Duration::from_hours(12)).open()?;

let cabinet = cabinet.clone(); // the build may outlive the call, so it owns what it uses
let answer = books.fetch(user_id, move || cabinet.book(user_id))?; // answer.value, answer.age, answer.stale
books.under(course_id).clear()?;
```

| The kept answer            | What the caller gets            | What is built                                |
|----------------------------|---------------------------------|----------------------------------------------|
| younger than `fresh`       | it, as it is                    | nothing                                      |
| between `fresh` and `keep` | it, at once, with `stale: true` | one new answer in the background             |
| past `keep`, or none       | the new answer                  | one, which every caller of the key waits for |

- One build of a key runs in the background at a time, across every client
  of the store: the caller whose call found the answer stale and no build
  under way runs its function there, and the callers beside it get the stale
  answer and run nothing (contract KV-16). On a node that copies `kv.db`, it
  is one a node.
- Its answer replaces the kept one and starts both terms again. Its failure
  changes nothing kept, reaches no caller, and holds off the next build of
  the key for a tenth of `fresh`, a second at least and a minute at most, so
  that a source that is down is not asked by every call. The pause is kept in
  memory and forgotten at a restart (contract KV-17).
- `age` is how old the answer is on the store's clock, and `stale` says it is
  older than `fresh`; without `fresh`, `fetch` gives the age and `stale` is
  always false (contract KV-18). The store keeps when an answer ends, not when
  it was made, so a handle opened with another `keep` than the one that
  wrote an answer reads its age shifted by the difference, within `[0, keep]`.
- `clear` forgets every answer of its branch and of the branches under it in
  one commit (contract KV-19). A build in the background under way meanwhile
  keeps nothing, and neither does one whose stale answer was deleted or
  reached `keep` while it ran: the next call builds the answer and waits for
  it.
- In Rust the build runs on the store's threads, so its function is
  `Send + 'static`: `fresh` opens a `Fresh`, whose `run` and `fetch` take
  such a function, while `Once` keeps its own. Builds run on threads the
  store keeps for them, four at once at most, the next waiting for one, so a
  slow source never holds the threads the store's other calls run on. Closing
  the store waits for the builds under way and drops those waiting. Over the
  protocol, in TypeScript,
  Python and Go, the build runs in the program that asked, beside its caller:
  a promise, a task or a goroutine nothing awaits, whose failure is never
  thrown. A store closed or a connection lost meanwhile ends it with nothing
  kept.
- The store counts `tinystore_once_builds_total`,
  `tinystore_once_build_failures_total` and `tinystore_once_stale_total` by
  `name` in its [own metrics](../metrics/self-metrics.md).
- Not built yet: waiting a bounded time for the new answer before taking the
  stale one.

## What it does not promise

`once` runs a key at most once while its answer is kept. It cannot make an
effect happen exactly once: if the process dies after the charge and before
the answer is kept, the next `run` charges again. Pass the same key to the
payment provider too, so that its own deduplication covers that gap. A
declined card that must not be charged again is an answer to keep, not an
error to throw.

## Async Rust

With the `async` feature, `aio`'s `once` takes an async function: the closure
gives a future, and `run` awaits it. See
[Rust with async](../../handbook/start/rust-async.md).

```rust
use std::time::Duration;
use tinystore::Options;
use tinystore::aio::Store;

let store = Store::open("data", Options::default())?;
let charges = store.once::<Receipt>("charges").keep(Duration::from_hours(24)).open()?;

// stripe.charge is an async fn here: the closure gives its future, and run awaits it
let receipt = charges.run(&idempotency_key, || stripe.charge(&order, &order.id)).await?;
```

A run that waits for the run under way holds no thread. The runs of the
blocking API and of `aio` wait for one another.

`aio`'s answers opened with `fresh` also take `spawn`, where a build in the
background runs, since the library runs no async runtime of its own and your
build's future may need yours; opening without it is `invalid`:

```rust
let books = store
    .once::<GradeBook>("cabinet-books")
    .fresh(Duration::from_mins(10))
    .keep(Duration::from_hours(12))
    .spawn(|build| drop(tokio::spawn(build)))
    .open()?;
let answer = books.fetch(user_id, move || cabinet.book(user_id)).await?;
```

A build dropped before its end, by a runtime shutting down, keeps nothing and
lets its key go.

On a node that copies `kv.db` from its home, a run claims its key at the
home, which waits for the run under way there. Your run waits for the home's
answer on a thread that TinyStore starts for the call, not on the store's
threads, so many runs of a busy key don't stop your other calls.

If you drop the future of a run:

- while it waits for another run, it stops waiting, and nothing changes;
- while its function runs, the key is let go and nothing is kept, so the next
  `run` runs the function again (contract KV-11);
- after its function answered, the answer is kept all the same, and the key is
  let go after it is written.

## Limits and defaults

| What                      | Value                                        |
|---------------------------|----------------------------------------------|
| `keep`                    | 1 day unless given                           |
| `fresh`                   | none unless given; shorter than `keep`       |
| the pause after a failure | a tenth of `fresh`, between 1 s and 1 minute |
| a kept answer             | 1 MiB at most                                |

## See also

- [KV](README.md): the engine's overview.
- [Requests sent twice](../../handbook/chat/sessions-and-limits.md#requests-sent-twice)
  in the Handbook, with a scene.
