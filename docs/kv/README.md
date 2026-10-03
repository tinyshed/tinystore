# KV

The KV engine stores your application's current state by key: sessions,
one-time codes, counters, rate limits, quotas, settings and idempotency keys.
Each bucket holds values of one type, keys can expire, and every write is
saved to disk before the call returns. KV keeps its data in `data/kv.db`.

## Open a bucket

```ts
type Session = { device: string; since: number }

const sessions = store.kv.bucket<Session>('sessions')
```

```python
from dataclasses import dataclass


@dataclass
class Session:
    device: str
    since: int


sessions = store.kv.bucket("sessions", Session)
```

```go
type Session struct {
	Device string
	Since  time.Time
}

state, err := kv.Open(ctx, store, kv.Options{}) // data/kv.db
sessions, err := kv.OpenBucket[Session](ctx, state, "sessions")
```

You give the value type once, when you open the bucket. Every call after that
reads and writes values of that type. In Go, open the KV engine once with
`kv.Open` and open all buckets from it.

## Read and write

```ts
await sessions.set('k7q2', { device: 'iPhone', since: Date.now() })

await sessions.get('k7q2')    // { device: 'iPhone', since: … }
await sessions.has('k7q2')    // true
await sessions.take('k7q2')   // the value, and the key is deleted
await sessions.get('k7q2')    // undefined
await sessions.delete('k7q2') // deleting a missing key is not an error
```

```python
await sessions.set("k7q2", Session(device="iPhone", since=now))

await sessions.get("k7q2")     # Session(device='iPhone', since=…)
await sessions.has("k7q2")     # True
await sessions.take("k7q2")    # the value, and the key is deleted
await sessions.get("k7q2")     # None
await sessions.delete("k7q2")  # deleting a missing key is not an error
```

```go
err = sessions.Set(ctx, "k7q2", Session{Device: "iPhone", Since: time.Now()})

s, found, err := sessions.Get(ctx, "k7q2")    // Session{Device: "iPhone", …}, true
found, err = sessions.Has(ctx, "k7q2")        // true
s, found, err = sessions.Take(ctx, "k7q2")    // the value, and the key is deleted
_, found, err = sessions.Get(ctx, "k7q2")     // false
err = sessions.Delete(ctx, "k7q2")            // deleting a missing key is not an error
```

`take` reads a key and deletes it in one step. If two requests take the same
key at the same moment, only one of them gets the value. This is how a
one-time code works.

A write returns after it is saved to disk. Writes from many requests at the
same time are committed together, so they share the cost of a disk sync.

## Group keys under an owner

```ts
const devices = store.kv.bucket<Device>('devices')

await devices.of(userId).set('iPhone', iphone)
await devices.of(userId).set('MacBook', macbook)

const page = await devices.of(userId).scan({ limit: 20 }) // { items, next }
await devices.of(userId).clear()                          // deletes every device of this user
```

```python
devices = store.kv.bucket("devices", Device)

await devices.of(user_id).set("iPhone", iphone)
await devices.of(user_id).set("MacBook", macbook)

page = await devices.of(user_id).scan(limit=20)  # Page(items, next)
await devices.of(user_id).clear()                # deletes every device of this user
```

```go
devices, err := kv.OpenBucket[Device](ctx, state, "devices")

err = devices.Of(userID).Set(ctx, "iPhone", iphone)
err = devices.Of(userID).Set(ctx, "MacBook", macbook)

page, err := devices.Of(userID).Scan(ctx, kv.Query{Limit: 20})
err = devices.Of(userID).Clear(ctx) // deletes every device of this user
```

`of` puts keys in a branch, like a folder. In Redis you would write a key such
as `user:42:devices:iPhone`. Here it is `devices.of(42).set('iPhone', …)`. You
don't create or delete branches: a branch exists while it contains keys.

You can nest branches with several owners, `of('tenant-7', 42)`. Branches never
overlap: `of(4)` doesn't contain the keys of `of(42)`.

`clear` deletes a branch and all branches under it. It works the same way for
ten keys and for ten million. Large branches are hidden immediately and
deleted in the background.

## List keys

```ts
let page = await sessions.scan({ limit: 100 })
while (page.next) {
	page = await sessions.scan({ limit: 100, after: page.next })
}

for await (const entry of sessions.all()) {
	console.log(entry.key, entry.value)
}
```

```python
page = await sessions.scan(limit=100)
while page.next:
    page = await sessions.scan(limit=100, after=page.next)

async for entry in sessions.all():
    print(entry.key, entry.value)
```

```go
page, err := sessions.Scan(ctx, kv.Query{Limit: 100})
for page.More {
	page, err = sessions.Scan(ctx, page.Next)
}

for entry, err := range sessions.All(ctx) {
	fmt.Println(entry.Key, entry.Value)
}
```

`scan` returns one page of the branch's own keys. `all` walks every page for
you. Neither holds a database snapshot between pages, so a long loop doesn't
block anything. Keys come back sorted by their bytes, so `"10"` comes before
`"9"`.

## Value types

| Value | Bun | Python | Go |
|---|---|---|---|
| JSON | `bucket<T>(name)` | a dataclass, `TypedDict` or model | any struct or other type |
| text | `bucket(name, 'string')` | `str` | `string` |
| bytes | `bucket(name, 'bytes')` | `bytes` | `[]byte` |
| integer | `'int'` or `'bigint'` | `int` | `int64` and other integers |
| float | `'float'` | `float` | `float64`, `float32` |
| boolean | `'bool'` | `bool` | `bool` |
| nothing, for a set of keys | `'none'` | `None` | `struct{}` |

In Bun, you can also pass a Standard Schema, such as a zod schema. The bucket
then checks every value it reads. Floats are stored bit for bit, including
`-0` and `NaN`. A value that JSON can't encode, such as `NaN` inside a JSON
object, is rejected when you write it.

## Keys

A key is a string, bytes or an integer. An integer is stored as its decimal
text, so `42` and `"42"` are the same key. This lets an integer id from your
database and the same id from a URL find the same value.

## In this section

- [Expiry](expiry.md): keys that delete themselves after a time, or slide
  forward while they are used.
- [Versions](versions.md): writes that only succeed if nobody changed the key
  since you read it.
- [Counters](counters.md): integers that you increment, with optional
  in-memory counting.
- [Sessions](sessions.md): a complete recipe for user sessions.
- [Rate limits](rate-limits.md): limit requests per second for each key.
- [Quotas](quotas.md): limits such as "100 per 5 hours and 300 per week".
- [Once](once.md): run a function only once per idempotency key.
- [Configs](configs.md): settings from defaults, files and environment
  variables, changeable at run time.
- [Transactions](transactions.md): several writes that succeed or fail
  together.

## Limits and defaults

| | |
|---|---|
| A key, including its branch | 1 KiB |
| A value | 1 MiB |
| A `scan` page | 100 keys by default, at most 1,000 keys or 4 MiB of values |
| A bucket name | `[a-z0-9][a-z0-9_-]{0,63}` |

Store larger values in [blobs](../blobs/README.md) and keep their keys in
KV.

## See also

- [kv/README.md](../../kv/README.md): the full contract of the KV engine.
- [design/kv.md](https://github.com/tinyshed/research/blob/main/tinystore/design/kv.md)
  in the research repository: why KV works this way, with measurements.
