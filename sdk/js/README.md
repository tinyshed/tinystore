<!--
This is the page npm shows. The headline, the sample and the engines are the
landing page's: task readme writes them between the landing: markers from
web/landing.md. Every link is absolute, because npm cannot follow a relative
one. Every call, by engine, is in docs/reference/bun.md.
-->

# TinyStore for Bun and Node

<!-- landing:headline -->

**A small storage runtime for applications.** SQL, key-value state, durable jobs, files, search, logs and metrics in one directory, with one lifecycle, one memory budget and one backup.

<!-- /landing:headline -->

> **Release candidates first.** Until `v0.1.0`, the API may still change, and
> a file written by one release may not open in the next.

The SDK starts a small `tinystore` server next to your program, called the
sidecar, and talks to it over a local connection. The server binary comes with the
package, so there is nothing else to install. Go programs embed the same
engines, and every client of one directory sees the same data.

<!-- landing:sample -->

```ts
import { open } from '@tinyshed/tinystore'

await using store = await open('./data')

const db = await store.database('app', { migrations: './migrations' })
await db.exec`insert into users (id, name) values (${42}, ${'Ada'})`

const sessions = store.bucket<Session>('sessions', { idle: '30d' })
await sessions.set(digest(token), { userId: 42 })

const emails = store.queue<Email>('emails')
await emails.add({ to: 'ada@example.com' })

const avatars = store.files('avatars')
await avatars.put('42.png', Bun.file('avatar.png'))

store.logger('api').info('user created', { userId: 42 })
store.metrics.counter('signups_total').inc()
```

```sh
bun add @tinyshed/tinystore
```

<!-- /landing:sample -->

It runs on Bun 1.4 or later and Node 22 or later, with the same API. Bun runs
the package's TypeScript, and Node runs the JavaScript that the package
contains. With npm, pnpm or Yarn, install it as `npm install @tinyshed/tinystore`.

## Engines

<!-- landing:engines -->

|                                                                                                                   |                   |                                                                         |
|-------------------------------------------------------------------------------------------------------------------|-------------------|-------------------------------------------------------------------------|
| [SQL](https://github.com/tinyshed/tinystore/blob/main/docs/chat/messages.md)                                      | Relational state  | Your own SQLite databases: checked migrations, a query builder.         |
| [KV](https://github.com/tinyshed/tinystore/blob/main/docs/chat/sessions-and-limits.md)                            | Application state | Values with expiry and versions; counters, rate limits, quotas, `once`. |
| [Jobs](https://github.com/tinyshed/tinystore/blob/main/docs/chat/replies.md)                                      | Background work   | Work that runs at its time: retries, steps, schedules, limits.          |
| [Blobs](https://github.com/tinyshed/tinystore/blob/main/docs/chat/attachments.md)                                 | Files             | Files by path, written whole or not at all, checked when read.          |
| [Search](https://github.com/tinyshed/tinystore/blob/main/docs/chat/finding-old-messages.md)                       | Words and vectors | Indexes of documents or of your tables, by words, by meaning, or both.  |
| [Channels](https://github.com/tinyshed/tinystore/blob/main/docs/chat/live-updates.md)                             | Messages          | Messages to whoever listens now, sent from a transaction at its commit. |
| [Locks](https://github.com/tinyshed/tinystore/blob/main/docs/chat/live-updates.md#one-summary-per-chat-at-a-time) | Coordination      | Locks and semaphores with fences, across processes and connections.     |
| [Config](https://github.com/tinyshed/tinystore/blob/main/docs/chat/settings.md)                                   | Settings          | Files and the environment in layers; secrets; settings changed live.    |
| [Records](https://github.com/tinyshed/tinystore/blob/main/docs/chat/what-it-is-doing.md)                          | Logs and events   | Read back by time, level, fields and text; followed as they arrive.     |
| [Metrics](https://github.com/tinyshed/tinystore/blob/main/docs/chat/how-fast-it-is.md)                            | Time series       | Samples kept bit for bit, answered exactly.                             |
| [Backup](https://github.com/tinyshed/tinystore/blob/main/docs/chat/keeping-it.md)                                 | The whole store   | One checked zip of every engine, taken while it runs.                   |

<!-- /landing:engines -->

## Three ways to connect

```ts
import { connect, open } from '@tinyshed/tinystore'

await using store = await open('./data')                    // the directory's sidecar, shared with other processes
await using alone = await open('./data', { private: true }) // a server for this process alone, for tests and scripts
await using remote = await connect('tls://db.internal:7070', { token })
```

[One directory, many engines](https://github.com/tinyshed/tinystore/blob/main/docs/start/directory.md#four-ways-to-open-a-store)
explains each one.

## The command line

The package also installs the `tinystore` command. It shows what a store
contains and follows its logs while your program runs:

```sh
bunx @tinyshed/tinystore status ./data                          # each engine's size, and who serves the directory
bunx @tinyshed/tinystore logs ./data -f                         # the application's logs, as they arrive
claude mcp add tinystore -- bunx @tinyshed/tinystore mcp ./data # read-only access for an AI agent
```

See [Your first store](https://github.com/tinyshed/tinystore/blob/main/docs/start/first-store.md#look-inside)
for the commands at work.

## Documentation

- [Your first store](https://github.com/tinyshed/tinystore/blob/main/docs/start/first-store.md): install it and write a first program
- [The chat we build](https://github.com/tinyshed/tinystore/blob/main/docs/start/chat.md): the application the book builds
- [The documentation](https://github.com/tinyshed/tinystore/blob/main/docs/README.md): the book, each example in TypeScript, Python, Go and Rust
- [API reference](https://github.com/tinyshed/tinystore/blob/main/docs/reference/api.md): every call, by engine

## License

Apache-2.0.
