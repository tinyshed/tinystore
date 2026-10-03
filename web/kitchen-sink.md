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
bun add tinystore
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

| Engine | File | Writer |
|---|---|---|
| kv | `kv.db` | its own |
| jobs | `jobs.db`, or the application's SQL file | shared when joined |

A table without one:

| | |
|---|---|
| Retention | `30 days` |
| Snapshot timeout | `5 seconds` |

## An image

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../.github/assets/logo-dark.svg">
  <img src="../.github/assets/logo-light.svg" width="64" alt="TinyStore">
</picture>

---

The end.
