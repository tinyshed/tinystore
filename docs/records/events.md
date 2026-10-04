# Events

An event is a record of something that happened: a sign-up, a click, a
payment. Events are stored like log lines, with a name instead of a message,
so you can count and search them with the same queries. Use the logger for
events you can afford to lose in a burst, and `append` for events that must
all be kept.

## Log an event

```ts
log.event('user.created', { userId: 42, plan: 'pro' })
```

```python
logging.info("user created", extra={"user_id": 42, "plan": "pro"})
```

```go
logger.Info("user created", "userId", 42, "plan", "pro")
```

`event` writes a record with the given name and attributes, through the same
buffer as the logger's lines. In Python and Go, an event through the logger is
a log line named `log`. Use `append` to give it a name of its own.

An event logger usually shouldn't print to the console. A view event per
request would fill your program's log, so turn the console off:
`store.records.logger('views', { console: 'off' })` in Bun,
`console="off"` in Python and `console.Off` in Go.

## Append records that must be kept

```ts
await store.records.append({
	stream: 'web',
	name: 'click',
	context: { session: sessionId },
	attrs: { element: 'buy', x: 812 },
})
```

```python
await store.records.append({
    "stream": "web",
    "name": "click",
    "context": {"session": session_id},
    "attrs": {"element": "buy", "x": 812},
})
```

```go
err = logs.Append(ctx, records.Record{
	At:      time.Now(),
	Stream:  "web",
	Name:    "click",
	Context: []records.Field{records.String("session", sessionID)},
	Attrs:   []records.Field{records.String("element", "buy"), records.Int("x", 812)},
})
```

`append` writes all of its records in one transaction, or none of them, and
returns after they are saved. A `scan` finds them at once. A record without a
time gets the current time.

Use the context for who produced the record, such as a session, a device or a
host, and the attributes for what happened. The store compresses repeated
contexts, so a session's context costs little even on many records.

## Events from other clocks

A record's time must be within the store's window: no older than the
retention, 14 days by default, and no more than ten minutes ahead of the
store's clock. A record outside the window is rejected (`ErrTooOld`,
`ErrTooNew`), because it would never be read, or would keep its data on disk
long after the retention.

Events from browsers or devices come with clocks you don't control. Give such
a record the time when you received it, and keep the device's own time as an
attribute.

## Read the fields of a record

```ts
import { fields } from '@tinyshed/tinystore'

const page = await store.records.scan({ since: '1h', names: ['click'] })
const { element, x } = fields(page.items[0].attrs) // { element: 'buy', x: 812 }
```

```python
page = await store.records.scan(since="1h", names=["click"])
attrs = tinystore.fields(page.items[0].attrs)  # {'element': 'buy', 'x': 812}
```

```go
page, err := logs.Scan(ctx, records.Query{Since: time.Hour, Names: []string{"click"}})
for _, field := range page.Records[0].Attrs {
	fmt.Println(field.Key, string(field.Value)) // element "buy", x 812
}
```

A record keeps each value as the JSON it was written with, as key and value
pairs in their original order. `fields` turns them into an object, the way
`JSON.parse` or `json.loads` would read them.

## Limits and defaults

|                 |                                                                    |
|-----------------|--------------------------------------------------------------------|
| A record        | 256 KiB; 128 context fields and 128 attributes                     |
| An `append`     | up to 4 MiB of records                                             |
| A record's time | from the retention cutoff to 10 minutes ahead of the store's clock |

## See also

- [Reading](reading.md): filter records by name, stream and fields.
- [records/README.md](../../records/README.md): the full contract of `append`.
