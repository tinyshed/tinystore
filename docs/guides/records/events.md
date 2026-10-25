# Events

An event is a named fact a program appends: a role changed, a user signed
up, a page was viewed. Unlike a log line, `append` waits until the event is
written and loses none. Events live in named streams, each keeping its
events as long as it says.

```ts
const audit = store.events('audit', { keep: '365d' })

await audit.append('role.changed', { user: 42, role: 'admin' })
await audit.append('role.changed', { user: 42, role: 'admin' }, { at: changedAt, context: { actor: admin.id } })
await clicks.append(batch.map(click => ({ name: click.type, fields: click.props, at: click.time }))) // all or none
```

```python
audit = store.events("audit", keep="365d")

await audit.append("role.changed", {"user": 42, "role": "admin"})
await audit.append("role.changed", {"user": 42, "role": "admin"}, at=changed_at, context={"actor": admin.id})
await clicks.append([Event(name=click.type, fields=click.props, at=click.time) for click in batch])
```

```go
audit, err := records.OpenEvents(store, "audit", records.Keep(365*24*time.Hour))

_, err = audit.Append(ctx, "role.changed", records.Fields{"user": 42, "role": "admin"})
_, err = audit.Append(ctx, "role.changed", change, records.At(changedAt), records.Context(records.Fields{"actor": admin.ID}))
_, err = clicks.AppendMany(ctx, events) // []records.Event{{Name, Fields, At}}, all or none
```

```rust
let audit = store.events("audit").keep(Duration::from_hours(365 * 24)).open()?;

audit.append("role.changed", &RoleChanged { user: 42, role: "admin" })?;
audit.event("role.changed").at(changed_at).context(&Actor { actor: admin.id }).append(&change)?;
clicks.append_all(batch)?; // NewEvent::new(name).fields(&props).at(time), all or none
```

- `append` returns once the event is written, and a read finds it at once.
  A crash of the program loses none. A power loss may take the last ones,
  since the events' file commits as `os` unless the store opened `full`.
- Appends from many callers share a commit.
- A list is written whole or not at all, and a refused event names its place
  in the list and its name.
- A stream begins with its first event; nothing declares it. Its name is
  `[a-z0-9][a-z0-9_-]{0,63}`.

## What an event carries

| Field               | Meaning                                                                              |
|---------------------|--------------------------------------------------------------------------------------|
| `at`                | its time; kept to the nanosecond, and events are read in its order                   |
| `stream`            | the stream it was written to                                                         |
| `id`                | the id its append gave it; absent on an event without one                            |
| `name`              | what happened, `role.changed`; `log` for a logger's line                             |
| `level`             | `trace`, `debug`, `info`, `warn`, `error` or `fatal`; absent on an event without one |
| `message`           | a line's text; absent on an event without one                                        |
| `fields`            | what happened: any object JSON writes                                                |
| `context`           | what it happened in: who asked, which request                                        |
| `traceId`, `spanId` | 32 and 16 hex digits, as a `traceparent` header writes them                          |

A value comes back as it was spelled: `1.2300`, `-0`, the order of the keys
come back byte for byte. In TypeScript an integer past 2⁵³ reads as a
`bigint`, and a field whose value is `undefined` is left out, as
`JSON.stringify` leaves it out. `null` stays `null`.

`append` hides nothing: `redact` is a logger's. Leave out what may hold a
secret before appending.

## Time

`at` is now unless given. It is taken from as long ago as the stream keeps,
to ten minutes ahead of the store's clock:

```text
events audit: event 0 "x": more than 10 minutes ahead of the store's clock: too new
```

Events from clocks you do not own, a phone's, a browser's, are appended with
the time they were received, and keep their own time as a field:

```ts
await clicks.append('view', { page, clientAt: body.at }) // at: now, as the server saw it
```

```python
await clicks.append("view", {"page": page, "client_at": body.at})
```

```go
_, err = clicks.Append(ctx, "view", records.Fields{"page": page, "clientAt": body.At})
```

```rust
clicks.append("view", &View { page, client_at: body.at })?;
```

A phone set a year back cannot bury its events in last year's segments, and a
phone set ahead cannot write the future.

## An append sent again

A job step runs at least once. If a worker crashes after `append` and before
its step finishes, the next attempt appends the same event again. Give the
event an id, such as a webhook's delivery id, and the second append writes
nothing:

```ts
const deploys = store.events('deploys')
await deploys.append('push', { sha }, { id: delivery }) // true
await deploys.append('push', { sha }, { id: delivery }) // false: the stream holds this id
await deploys.append([{ name: 'push', id: delivery, fields: { sha } }]) // 0 written
```

```python
deploys = store.events("deploys")
await deploys.append("push", {"sha": sha}, id=delivery)  # True
await deploys.append("push", {"sha": sha}, id=delivery)  # False: the stream holds this id
await deploys.append([Event("push", {"sha": sha}, id=delivery)])  # 0 written
```

```go
deploys, err := records.OpenEvents(store, "deploys")
added, err := deploys.Append(ctx, "push", records.Fields{"sha": sha}, records.ID(delivery)) // true
added, err = deploys.Append(ctx, "push", records.Fields{"sha": sha}, records.ID(delivery))  // false: the stream holds this id
written, err := deploys.AppendMany(ctx, []records.Event{{Name: "push", ID: delivery, Fields: records.Fields{"sha": sha}}}) // 0
```

```rust
let deploys = store.events("deploys").open()?;
let added: bool = deploys.event("push").id(&delivery).append(&push)?; // true
let added: bool = deploys.event("push").id(&delivery).append(&push)?; // false: the stream holds this id
let written: usize = deploys.append_all([NewEvent::new("push").id(&delivery).fields(&push)])?; // 0
```

- One event's `append` returns whether it wrote the event, and a list's
  returns how many events it wrote. An event without an id is always
  written.
- The first event of an id stays. TinyStore compares only the id, so a later
  event with the same id writes nothing even if its fields differ.
- In a list, the first event of an id is written and later ones with the same
  id are not. The rest of the list is written whole, and a refused event
  still refuses the whole list.
- The stream keeps an id for as long as a read or a poll can still return its
  event. Once `keep` removes the event, its id can be used again.
- An id is 1 to 128 bytes. Each stream has its own ids, so one id in two
  streams makes two events.
- A read returns the id as `id` (`ID` in Go).

## How long a stream keeps

`keep` is the stream's own, kept in the file with it: a logger and an
`events` of one name share it, and so do two runs of a program.

- A stream nobody gave a `keep` keeps 14 days.
- An open that gives one sets it. While the store is open, giving another is
  `invalid`, naming both, so one part of a program never shortens what
  another keeps:

```text
events audit: keep 30d, where the stream is open with keep 365d: invalid
```

- An event older than its stream keeps is never read, whether or not it has
  left the file yet. Expiry removes whole segments, so there is no cleanup
  job and no delete that rewrites rows.

## Logger or `append`?

| You want                                          | Use                       |
|---------------------------------------------------|---------------------------|
| lines a developer reads, never slowing a request  | the [logger](logging.md)  |
| a fact that must not be lost: audit, billing      | `append`                  |
| analytics a program can afford to lose under load | `log.event(name, fields)` |

All three write the same events to the same streams, and every read finds
them all.

## Limits

| What            | Limit                                |
|-----------------|--------------------------------------|
| an event        | 256 KiB, and 128 keys in all         |
| an event's id   | 1 to 128 bytes                       |
| one `append`    | 4 MiB                                |
| an event's time | from `keep` ago to ten minutes ahead |
| `keep`          | 14 days unless the stream says       |

## See also

- [Records](README.md): the engine's overview.
- [Reading](reading.md): finding, counting and aggregating events.
