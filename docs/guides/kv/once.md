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

An option `once` doesn't know is refused when you open it, naming the
option, before anything is sent (contract KV-20). In TypeScript a typo like
`{ keeps: '1h' }` throws
``InvalidError: kv once grade-books: no field `keeps`; did you mean `keep`?``.
Python raises `TypeError` for an unknown keyword, and in Go and Rust an
option that doesn't exist doesn't compile.

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

With only `keep`, every caller waits for the whole build again when `keep`
ends. If the build fails, there is no answer to return. Set a second term,
`fresh`, that is shorter than `keep`. An answer younger than `fresh` is
returned as it is. An older answer is returned at once, and one new answer
is built in the background. If that build fails, the kept answer stays until
`keep` ends. So when the source is down, callers still get the last good
answer.

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

| The kept answer            | What the caller gets                         | What is built                                     |
|----------------------------|----------------------------------------------|---------------------------------------------------|
| younger than `fresh`       | the kept answer, as it is                    | nothing                                           |
| between `fresh` and `keep` | the kept answer, at once, with `stale: true` | one new answer, in the background                 |
| older than `keep`, or none | the new answer                               | one new answer, and every caller of the key waits |

- Only one background build per key runs at a time, across all clients of
  the store. The first caller that finds the answer stale, when no build is
  running, runs its function in the background. Callers at the same time get
  the stale answer and run nothing (contract KV-16). On a node that copies
  `kv.db`, there is one build per node.
- The new answer replaces the kept one and restarts both terms. If the build
  fails, nothing kept changes and no caller sees the error. The store then
  waits before the next build of that key: a tenth of `fresh`, at least one
  second and at most one minute. This way a source that is down isn't called
  on every request. The store keeps this pause in memory, so a restart
  forgets it (contract KV-17).
- `age` is how old the answer is, by the store's clock. `stale` is true when
  the answer is older than `fresh`. Without `fresh`, `fetch` still returns
  the age, and `stale` is always false (contract KV-18). The store saves when
  an answer expires, not when it was made. So if you open a handle with a
  different `keep` from the one that wrote the answer, its age is off by the
  difference, clamped to `[0, keep]`.
- `clear` deletes every answer in its branch and in the branches under it,
  in one commit (contract KV-19). If a background build is running at that
  moment, it keeps nothing. The same is true for a build whose stale answer
  was deleted, or reached `keep`, while it ran. In both cases the next call
  builds the answer and waits for it.
- In Rust, the build runs on the store's threads, so its function must be
  `Send + 'static`. That is why `.fresh(…)` opens a separate `Fresh` handle,
  whose `run` and `fetch` accept such a function. `Once` keeps its own
  signature. The store runs at most four builds at once, on threads it keeps
  for them, and the next build waits for a free one. So a slow source never
  blocks the threads that the store's other calls run on. When the store
  closes, it waits for the running builds and drops the waiting ones.
- Over the protocol, in TypeScript, Python and Go, the build runs in your
  program, next to the caller. It is a promise, a task or a goroutine that
  nothing awaits, and its failure is never thrown. If the store closes or
  the connection drops during the build, the build ends and keeps nothing.
- The store counts `tinystore_once_builds_total`,
  `tinystore_once_build_failures_total` and `tinystore_once_stale_total` per
  `name` in its [own metrics](../metrics/self-metrics.md).
- Not built yet: waiting a limited time for the new answer before returning
  the stale one.

## Encrypted answers

A cache is where people put what they'd never put in a table: a student's
grades, a bank statement, a lab result. By default `once` writes its answers
to `kv.db` as they are. Open it with `encrypted` and every answer is sealed
with the store's encryption key, exactly as an
[encrypted bucket](encrypted.md) seals its values: the same key file, the
same older keys, the same `reseal`.

```ts
const books = store.once<GradeBook>('grade-books', { fresh: '10m', keep: '1h', encrypted: true })

const book = await books.run(String(userId), () => cabinet.book(userId))
const moved = await books.reseal() // after a key rotation: answers sealed again with the new key
```

```python
books = store.once("grade-books", GradeBook, fresh="10m", keep="1h", encrypted=True)

book = await books.run(str(user_id), lambda: cabinet.book(user_id))
moved = await books.reseal()
```

```go
books, err := kv.OpenOnce[GradeBook](store, "grade-books", kv.Encrypted(), kv.Fresh(10*time.Minute), kv.Keep(time.Hour))

book, err := books.Run(ctx, strconv.Itoa(userID), func(ctx context.Context) (GradeBook, error) { return cabinet.Book(ctx, userID) })
moved, err := books.Reseal(ctx)
```

```rust
let books = store.once::<GradeBook>("grade-books").encrypted().fresh(Duration::from_mins(10)).keep(Duration::from_hours(1)).open()?;

let book = books.run(user_id, move || cabinet.book(user_id))?;
let moved = books.reseal()?;
```

- The file holds nothing readable without the key, and the key isn't
  sealed: put the secret in the answer, never in the key (contracts SEC-03,
  KV-21).
- A name remembers whether it is encrypted. Opening `grade-books` without
  `encrypted` after it was opened with it, or the other way round, fails
  with `invalid`: `the name holds encrypted once's answers, not once's
  answers`. To move a plain once to an encrypted one, use a new name. The
  old answers expire with their `keep`.
- `reseal` seals again, with the store's current key, every answer an older
  key sealed, and returns how many. On answers opened without `encrypted` it
  fails with `invalid`. Key rotation works as for buckets: see
  [encrypted buckets](encrypted.md).
- A read-only connection can't open an encrypted once: it may read only
  what the file shows without the key.
- On a node that copies `kv.db`, a run's answer crosses the link to the home
  sealed, and a kept answer comes back sealed. A copy and its home need the
  same key.

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

In `aio`, a handle opened with `fresh` also needs `spawn`, which says where
a background build runs. The library has no async runtime of its own, and
your build's future may need yours. Opening without `spawn` fails with
`invalid`:

```rust
let books = store
    .once::<GradeBook>("cabinet-books")
    .fresh(Duration::from_mins(10))
    .keep(Duration::from_hours(12))
    .spawn(|build| drop(tokio::spawn(build)))
    .open()?;
let answer = books.fetch(user_id, move || cabinet.book(user_id)).await?;
```

If a build is dropped before it ends, for example because the runtime shuts
down, it keeps nothing and releases its key.

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
| `keep`                    | 1 day unless set                             |
| `fresh`                   | none unless set, and shorter than `keep`     |
| `encrypted`               | off unless set                               |
| the pause after a failure | a tenth of `fresh`, between 1 s and 1 minute |
| a kept answer             | 1 MiB at most                                |

## See also

- [KV](README.md): the engine's overview.
- [Requests sent twice](../../handbook/chat/sessions-and-limits.md#requests-sent-twice)
  in the Handbook, with a scene.
