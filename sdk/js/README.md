<!--
This is the page npm shows. The headline, the sample and the engines are the
landing page's: task readme writes them between the landing: markers from
web/landing.md. Every link is absolute, because npm cannot follow a relative
one. Every call, by engine, is in docs/reference/bun.md.
-->

# TinyStore for Bun and Node

<!-- landing:headline -->

**A small storage runtime for applications.** SQL, key-value state, durable jobs, files, metrics and logs in one directory, with one lifecycle, one memory budget and one backup.

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

const db = await store.sql('app', {
	migrations: { '0001_users.sql': 'create table users (id integer primary key, name text)' },
})
await db.exec`insert into users (id, name) values (${42}, ${'Ada'})`

const sessions = store.kv.bucket<string>('sessions', { sliding: '30d' })
await sessions.of('42').set('token', 'abc123')

const emails = store.jobs.queue<{ to: string }>('emails')
await emails.enqueue({ to: 'ada@example.com' })

const files = store.blobs.bucket('files')
await files.put('avatars/42.png', Bun.file('avatar.png'))

const log = store.records.logger('api')
log.event('user.created', { userId: 42 })

const signups = store.metrics.counter('signups_total')
signups.inc()
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

|                                                                                   |                         |                                                                               |
|-----------------------------------------------------------------------------------|-------------------------|-------------------------------------------------------------------------------|
| [SQL](https://github.com/tinyshed/tinystore/blob/main/docs/sql/README.md)         | Relational state        | The application's own SQL databases: tables from structs, checked migrations. |
| [KV](https://github.com/tinyshed/tinystore/blob/main/docs/kv/README.md)           | Application state       | Current state: typed buckets, counters, expiry, versions.                     |
| [Jobs](https://github.com/tinyshed/tinystore/blob/main/docs/jobs/README.md)       | Durable background work | Work that runs at its time: retries, leases, repeats.                         |
| [Blobs](https://github.com/tinyshed/tinystore/blob/main/docs/blobs/README.md)     | Files and objects       | Files by path, checked when read whole.                                       |
| [Records](https://github.com/tinyshed/tinystore/blob/main/docs/records/README.md) | Logs and events         | Read by time, level and keys.                                                 |
| [Metrics](https://github.com/tinyshed/tinystore/blob/main/docs/metrics/README.md) | Time series             | Samples kept bit for bit, answered exactly.                                   |

<!-- /landing:engines -->

## Three ways to connect

```ts
import { connect, open } from '@tinyshed/tinystore'

await using store = await open('./data')                    // the directory's sidecar, shared with other processes
await using alone = await open('./data', { private: true }) // a server for this process alone, for tests and scripts
await using remote = await connect('tls://db.internal:7070', { token })
```

[The sidecar](https://github.com/tinyshed/tinystore/blob/main/docs/running/sidecar.md)
and [A remote server](https://github.com/tinyshed/tinystore/blob/main/docs/running/server.md)
explain each one.

## The command line

The package also installs the `tinystore` command. It shows what a store
contains and follows its logs while your program runs:

```sh
bunx @tinyshed/tinystore status ./data                          # each engine's size, and who serves the directory
bunx @tinyshed/tinystore logs ./data -f                         # the application's logs, as they arrive
claude mcp add tinystore -- bunx @tinyshed/tinystore mcp ./data # read-only access for an AI agent
```

See [The command line](https://github.com/tinyshed/tinystore/blob/main/docs/running/cli.md)
for every command.

## Documentation

- [Getting started](https://github.com/tinyshed/tinystore/blob/main/docs/getting-started.md): install it and write a first program
- [A tour](https://github.com/tinyshed/tinystore/blob/main/docs/tour.md): every engine on one page
- [The guides](https://github.com/tinyshed/tinystore/blob/main/docs/README.md): a page per feature, each example in Bun, Python and Go
- [Bun and Node API](https://github.com/tinyshed/tinystore/blob/main/docs/reference/bun.md): every call, by engine

## License

Apache-2.0.
