# Metrics

Metrics stores exact time-series samples. Use gauges for values that move in
either direction and counters for values that only grow.

## Count something

Create an instrument once and keep it. A counter is a handle: its labels pick
a series, and each increment adds one.

```ts title="metrics.ts"
export const requests = store.metrics.counter('http_requests_total')
```

```ts title="routes.ts"
import { requests } from './metrics.ts'

export function listUsers(): User[] {
	requests.with({ route: '/users' }).inc()
	return users
}
```

```python title="metrics.py"
requests = store.metrics.counter("http_requests_total")
```

```python title="routes.py"
from metrics import requests

def list_users() -> list[User]:
    requests.labels(route="/users").inc()
    return users
```

```go title="metrics.go"
requests := store.Counter("http_requests_total")
```

```go title="routes.go"
http.HandleFunc("GET /users", func(w http.ResponseWriter, r *http.Request) {
	requests.With("route", "/users").Inc()
	json.NewEncoder(w).Encode(users)
})
```

The counter holds its total in memory, and every 15 seconds the engine stores
that value as one sample. A restart starts the total from zero again. That is
a reset, and `increase` accounts for it.

## Read it back

`read` returns the exact samples of every series whose labels match, bit for
bit as they were written.

```ts title="read.ts"
const lastHour = await store.metrics.read({
	name: 'cpu',
	match: { host: 'web-1' },
	since: '1h',
})
```

```python title="read.py"
last_hour = await store.metrics.read(
    name="cpu",
    match={"host": "web-1"},
    since="1h",
)
```

```go title="read.go"
lastHour, err := store.Read(ctx, metrics.Range{
	Name:  "cpu",
	Match: metrics.Labels{"host": "web-1"},
	Since: time.Hour,
})
```

`match` takes labels by exact equality, every one at once. `where` goes beyond
it: one of several values, none of them, or a prefix. Regular expressions are
not supported.

## Aggregate exactly

`aggregate` splits the range into buckets of `width` and returns one value for
each bucket that has data.

```ts title="aggregate.ts"
const traffic = await store.metrics.aggregate({
	name: 'http_requests_total',
	since: '24h',
	width: '1h',
	op: 'increase',
})
```

```python title="aggregate.py"
traffic = await store.metrics.aggregate(
    name="http_requests_total",
    since="24h",
    width="1h",
    op="increase",
)
```

```go title="aggregate.go"
traffic, err := store.Aggregate(ctx, metrics.AggregateRequest{
	Range: metrics.Range{Name: "http_requests_total", Since: 24 * time.Hour},
	Width: time.Hour,
	Op:    metrics.AggregateIncrease,
})
```

> [!NOTE]
> **Counters**
> `increase` is for counters. It includes resets, so restarting a process does
> not make the counter jump backwards.

## Limits and defaults

Store options set the capacities. A request can only lower them.

| | |
|---|---|
| Retention | `30 days` |
| Instrument flush | `15 seconds` |
| Registered series | `100,000` |
| Labels per series | `128 pairs` |
| Output samples per query | `100,000` |
| Snapshot timeout | `5 seconds` |

The engine's contract, every limit and what it guarantees, is
[metrics/README.md](../../metrics/README.md); the arithmetic of an aggregate is
[the aggregate contract](../aggregate-contract.md).
