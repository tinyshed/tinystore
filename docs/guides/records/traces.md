# Traces

One request touches many places: the HTTP handler logs to `api`, a job it
adds logs to `worker`, an audit event goes to `audit`. A trace ties them
together: every line logged and event appended inside a traced call carries
its trace id, and one query reads them all back, in order, across every
stream.

```ts
const traceId = request.headers.get('traceparent')?.split('-')[1] ?? randomTraceId()

await withTrace({ traceId, spanId }, async () => {
	log.info('checkout started', { cart: cart.id }) // carries traceId, after any await too
	await audit.append('order.created', { order: order.id })
})
```

```python
with tinystore.trace(trace_id=trace_id, span_id=span_id):
    log.info("checkout started", cart=cart.id)  # carries trace_id, after any await too
    await audit.append("order.created", {"order": order.id})
```

```go
ctx = records.WithTrace(ctx, traceID, spanID)
log.InfoContext(ctx, "checkout started", "cart", cart.ID) // the context carries it
err = audit.Append(ctx, "order.created", records.Fields{"order": order.ID})
```

```rust
let _request = tracing::info_span!("request", trace_id = %trace_id, span_id = %span_id).entered();
tracing::info!(cart = cart.id, "checkout started"); // the span's trace
```

| Language   | The trace travels in                                  |
|------------|-------------------------------------------------------|
| TypeScript | `withTrace`, through every `await` inside it          |
| Python     | a `contextvars` variable, `with tinystore.trace(…)`   |
| Go         | the `context.Context`: use `log.InfoContext(ctx, …)`  |
| Rust       | a `tracing` span with `trace_id` and `span_id` fields |

The ids are a `traceparent` header's: 32 hex digits for the trace, 16 for the
span. An id that is not hex of that length is no trace, as W3C's
`traceparent` ignores it. An event given its own ids keeps them.

## Reading one request

```ts
const steps = await store.events().list({ since: '24h', traceId })
for (const e of steps.events) console.log(e.at, e.stream, e.name, e.message ?? '', e.fields)
```

```python
steps = await store.events().list(since="24h", trace_id=trace_id)
for e in steps.events:
    print(e.at, e.stream, e.name, e.message or "", e.fields)
```

```go
steps, err := events.List(ctx, records.Query{Since: 24 * time.Hour, TraceID: traceID})
```

```rust
let steps = store.all_events()?.list().since(Duration::from_hours(24)).trace_id(trace).page()?;
```

```text
16:42:24.094  api     log            checkout started  { cart: 7 }
16:42:24.101  audit   order.created                    { order: 41 }
16:42:25.310  worker  log            receipt sent      { order: 41 }
```

A trace id rarely repeats, so the store finds its blocks through a bloom
filter of each block's traces without reading the others: one request out of
millions is cheap to find.

## Across processes

A job carries the trace into the worker that runs it if the program puts it
in the job and opens a trace around the work:

```ts
await emails.add({ order: order.id, traceId })
emails.work(async job => withTrace({ traceId: job.value.traceId }, () => sendReceipt(job.value.order)))
```

```python
await emails.add(Email(order=order.id, trace_id=trace_id))


async def send(job):
    with tinystore.trace(trace_id=job.value.trace_id):
        await send_receipt(job.value.order)
```

```go
_, err = emails.Add(ctx, Email{Order: order.ID, TraceID: traceID})
// in the worker:
ctx = records.WithTrace(ctx, job.Value.TraceID, "")
```

```rust
emails.add(&Email { order: order.id, trace_id: trace_id.to_string() })?;
// in the worker:
let _span = tracing::info_span!("job", trace_id = %job.value.trace_id).entered();
```

The worker's lines then read back with the request's. A span id may be left
out.

## See also

- [Records](README.md): the engine's overview.
- [Reading](reading.md): every condition of a query.
