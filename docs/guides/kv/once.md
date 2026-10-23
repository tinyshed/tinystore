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

A run of the same key inside its own run is `invalid`, because it would wait
for itself (contract KV-11).

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

Each kept answer is a write to `kv.db`, synced like any other, and a value
there holds at most 1 MiB: a larger answer still reaches the users who waited
for it, but the next run computes it again. A run waits for the one under way
however long it takes, so a memo of a slow query that callers may give up on
belongs in the program's memory.

## What it does not promise

`once` runs a key at most once while its answer is kept. It cannot make an
effect happen exactly once: if the process dies after the charge and before
the answer is kept, the next `run` charges again. Pass the same key to the
payment provider too, so that its own deduplication covers that gap. A
declined card that must not be charged again is an answer to keep, not an
error to throw.

## See also

- [KV](README.md): the engine's overview.
- [Requests sent twice](../../handbook/chat/sessions-and-limits.md#requests-sent-twice)
  in the Handbook, with a scene.
