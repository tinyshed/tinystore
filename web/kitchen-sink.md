# Kitchen sink

Every element a page of the docs can hold, drawn the way the site draws it.
The site builds this file at /docs/kitchen-sink and lists it nowhere: it is
where a change to the styles shows everything it touches at once.

## Text

A paragraph with `inline code`, **bold**, _emphasis_, a link to
[another page](../docs/metrics/README.md#ask-how-much), one to
[a file of the repository](../kv/kv.go) and one
[elsewhere](https://github.com/tinyshed/tinystore).

- A list item
- Another, with a nested list:
  - one
  - two

1. A numbered item
2. Another

> A quotation, which is not a callout.

### A third-level heading

Text under it.

## Code

Three languages, two files each:

```ts title="metrics.ts"
export const requests = store.metrics.counter('http_requests_total')
```

```ts title="routes.ts"
import { requests } from './metrics.ts'

requests.with({ route: '/users' }).inc()
```

```python title="metrics.py"
requests = store.metrics.counter("http_requests_total")
```

```python title="routes.py"
from metrics import requests

requests.labels(route="/users").inc()
```

```go title="metrics.go"
requests := store.Counter("http_requests_total")
```

```go title="routes.go"
requests.With("route", "/users").Inc()
```

A command for each language:

```sh for=bun
bun add @tinyshed/tinystore
```

```sh for=python
pip install tinyshed-tinystore
```

```sh for=go
go get github.com/tinyshed/tinystore
```

One block of one language, and plain text:

```sql
select id, body from notes where author = ?
```

```text
a block of plain text, kept as it is written
```

## Callouts

> [!NOTE]
> **Counters**
> `increase` is for counters. It includes resets.

> [!TIP]
> A tip, labelled by its kind.

> [!IMPORTANT]
> Something to read before going on.

> [!WARNING]
> Something that can go wrong.

> [!CAUTION]
> Something that loses data.

## Tables

A table with a header:

| Engine | File                                     | Writer             |
|--------|------------------------------------------|--------------------|
| kv     | `kv.db`                                  | its own            |
| jobs   | `jobs.db`, or the application's SQL file | shared when joined |

A table without one:

|                  |             |
|------------------|-------------|
| Retention        | `30 days`   |
| Snapshot timeout | `5 seconds` |

## An image

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../.github/assets/logo-dark.svg">
  <img src="../.github/assets/logo-light.svg" width="64" alt="TinyStore">
</picture>

---

The end.

## Scenes

A scene plays a short story beside the code it runs: a ```scene fence that
GitHub shows as its text.

```scene
code ts
  const charges = store.once<Receipt>('charges', { keep: '1d' })

  app.post('/pay', async req => {
    const key = req.headers['idempotency-key']
    return charges.run(key, () => stripe.charge(req.body.order, key))
  })
end
actor buyer  "Buyer"       at 8,50  icon=user
actor one    "Request 1"   at 36,22 icon=server
actor two    "Request 2"   at 36,80 icon=server
actor kv     "kv.db"       at 64,50 icon=database
actor stripe "Stripe"      at 92,50 icon=credit-card
link buyer one
link buyer two
link one kv
link two kv
link one stripe
set kv "charges: —"
set stripe "charged: 0"

step "The buyer double-clicks “Pay”. Two requests with the same idempotency key reach your API at once." line=3-4
  send buyer -> one "POST /pay"
  & send buyer -> two "POST /pay"

step "Request 1 asks charges.run for the key. Nobody holds it, so it starts the charge." line=5
  send one -> kv "run key-7f"
  set kv "key-7f: running"
  send one -> stripe "charge $40"

step "Request 2 asks for the same key. It is already running, so Request 2 waits for that answer instead of charging again." line=5
  send two -> kv "run key-7f"
  say two "waiting…"

step "Stripe charges the card once. charges.run keeps the receipt under the key for a day." line=5
  set stripe "charged: 1"
  send stripe -> one "receipt" color=ok
  send one -> kv "keep receipt"
  set kv "key-7f: receipt"

step "Request 2 gets the same receipt. Both clicks are answered, and the card was charged once." line=5
  send kv -> two "receipt" color=ok
  send one -> buyer "200 receipt" color=ok
  & send two -> buyer "200 receipt" color=ok
  say stripe "1 charge" color=ok
```

A scene with no code:

```scene
actor app  "Your app" at 12,50 icon=user
actor core "Core"     at 50,50 icon=cpu
actor disk "kv.db"    at 88,50 icon=database
link app core
link core disk

step "The app writes."
  send app -> core "set"
  send core -> disk "commit"

step "The disk answers."
  send disk -> app "ok" color=ok
```
