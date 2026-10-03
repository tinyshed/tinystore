# Traces

Attach a trace id to everything a request logs, then read every record of
that request with one query. TinyStore doesn't depend on a tracing library,
and its trace and span ids are the same bytes as OpenTelemetry's.

## Log with a trace

```ts
import { withTrace } from 'tinystore'

await withTrace({ traceId, spanId }, async () => {
	log.info('charging card')
	await store.records.append({ stream: 'payments', name: 'charge', attrs: { amount: 900 } })
	log.info('charged')
})
```

```python
with tinystore.trace(trace_id, span_id):
    logging.info("charging card")
    await store.records.append({"stream": "payments", "name": "charge", "attrs": {"amount": 900}})
    logging.info("charged")
```

```go
ctx = records.WithTrace(ctx, traceID, spanID)

logger.InfoContext(ctx, "charging card")
err = logs.Append(ctx, records.Record{Stream: "payments", Name: "charge",
	Attrs: []records.Field{records.Int("amount", 900)}})
logger.InfoContext(ctx, "charged")
```

Every line logged inside the trace, and every appended record that has no
trace of its own, takes the trace and span ids. Bun carries the trace through
`AsyncLocalStorage`, Python through `contextvars`, and Go through the context,
so the trace follows your code across `await` and function calls.

In Go, log with the `…Context` methods of `slog`, such as `InfoContext`, so
that the handler sees the context.

## Read one request

```ts
for await (const record of store.records.all({ since: '24h', traceId })) {
	console.log(record.stream, record.name, record.body)
}
```

```python
async for record in store.records.all(since="24h", trace_id=trace_id):
    print(record.stream, record.name, record.body)
```

```go
for record, err := range logs.All(ctx, records.Query{Since: 24 * time.Hour, TraceID: traceID}) {
	if err != nil {
		return err
	}
	fmt.Println(record.At, record.Stream, record.Name)
}
```

Each block of records has a filter of the trace ids it contains, so a query by
trace reads only the few blocks that can hold the request.

## Ids

A trace id is 16 bytes and a span id is 8 bytes, as in OpenTelemetry. Pass
them as bytes, or as hex text in Bun and Python. If you already use
OpenTelemetry, pass its current trace and span ids, and your logs and traces
share ids.

## See also

- [Reading](reading.md): every filter of a query.
- [records/README.md](../../records/README.md): the full contract of traces.
