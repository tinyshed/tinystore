# Store API for Bun and Node

Every public class, function and type of the store in `@tinyshed/tinystore`, generated from its source. The [Store guide](../../handbook/start/directory.md) explains how to use them, and the [Python](../python/store.md), [Go](../go/store.md) and [Rust](../rust/store.md) pages list the same API.

## OpenOptions

```ts
interface OpenOptions {
    /**
     * A server of this process's own, a child on stdin and stdout that lives
     * and dies with it, rather than the directory's shared sidecar: tests,
     * scripts, one process alone.
     */
    private?: boolean
    /**
     * The core inside this process, loaded from its library, rather than a
     * server beside it: no process and no socket.
     */
    embedded?: boolean
    /**
     * The core for `embedded`: under Bun the C library, TINYSTORE_LIBRARY when
     * absent, and under Node its addon, tinystore.node, TINYSTORE_ADDON when
     * absent; then this platform's package's.
     */
    library?: string | undefined
    /**
     * Measures the store into its own metrics, under names that start with
     * `tinystore_`: its commits and their waits, its files, its memory and
     * the machine it runs on, which `{ system: false }` leaves out. Off
     * unless asked for; a sidecar found running keeps its own.
     */
    selfMetrics?: SelfMetrics
    /**
     * The metrics series the store holds at most: a new series past them is
     * a LimitError. 100,000 unless given; a sidecar found running keeps its own.
     */
    maxSeries?: number
    /** The tinystore binary: TINYSTORE_BIN, this platform's package, or PATH's when absent. */
    binary?: string
    /**
     * How long a sidecar this process starts stays once its last connection
     * has gone: 30 s unless given, 0 for ever. One found running keeps its own.
     */
    idle?: Duration
    /**
     * Runs a private server on a test's clock, starting at this time, which
     * `store.clock` moves forward instead of a test waiting: keys expire, jobs
     * come due and records age at once. Needs `private`.
     */
    clock?: Time
    /**
     * How far a commit of the store's files goes before it returns. Unless
     * given, kv, jobs, files, search and every database commit `'full'`, and
     * records and metrics `'os'`; given, every file commits so. `'os'` does
     * not wait for the disk, so a write returns in microseconds, survives this
     * program's crash, and may be lost with the last others to a power loss.
     * It is the store's, set by whoever opens it first: a sidecar found
     * running with another, or with none when this asks one, is refused. A
     * database may say its own, `store.database('audit', { durability: 'full' })`.
     */
    durability?: Durability
    /**
     * The file of the store's encryption key, which seals the values of its
     * encrypted buckets: 64 hex digits, as `openssl rand -hex 32` writes, kept
     * apart from the store's data. The store reads it and never makes it.
     * Without it the key is `encryption.key` in the store's directory, which the
     * store makes when the first encrypted bucket opens. A sidecar found
     * running keeps the key it was started with.
     */
    encryptionKeyFile?: string
    /**
     * Makes the store a node: it keeps a log of each file it writes, serves
     * its files to other nodes, and copies the files of the node it takes
     * from, reading them here and passing their writes to their home. A draft,
     * as its names are:
     *
     *     const store = await open('./data', { embedded: true, node: { from: ['eu.internal:7444'] } })
     */
    node?: NodeOptions | undefined
}
```

## NodeOptions

```ts
interface NodeOptions {
    /** where this node takes the files it copies, `host:port`, tried in order */
    from?: string[] | undefined
    /** where other nodes take files from this one, `host:port` */
    serve?: string | undefined
    /** the files it copies, as `kv.db` or `sql/app.db`; every file but its own records and metrics when absent */
    files?: string[] | undefined
    /** what it says to the node it takes from, and asks of the nodes that take from it; embedded alone */
    token?: string | undefined
    /** the file of that token, for a server this process starts */
    tokenFile?: string | undefined
    /** a write of a file this node writes answers once a node copying it holds the commit */
    synchronous?: boolean | undefined
    /** the nodes, by address or id, a synchronous commit waits for one of; any node copying the file when absent */
    synchronousWith?: string[] | undefined
    /**
     * this node is the home of the catalog, which names each file's home: one
     * node of a network, chosen by a person; the others then write each file
     * they make once their claim of its name won, and need no `files`
     */
    catalog?: boolean | undefined
    /** where other nodes reach this one, `host:port`, when `serve` listens on every interface */
    advertise?: string | undefined
    /** plain TCP on links beyond this machine, which anyone on the way reads; without it such a link takes TLS */
    insecure?: boolean | undefined
}
```

What a store's node is: whom it copies, and where it serves.

## Status

```ts
interface Status {
    /** its version: v0.1.0, or (devel) for one built from a checkout */
    server: string
    protocol: number
    /** the engines it serves */
    engines: string[]
    /** admin may change a schema and drop records; data reads and writes */
    capability: 'admin' | 'data'
}
```

What a server is, as its WELCOME said it to this store's connection.

## ConnectOptions

```ts
interface ConnectOptions {
    /** a line of the server's tokens file, admin or data */
    token: string
    tls?: TlsOptions
}
```

## Store

```ts
class Store implements AsyncDisposable {
    readonly metrics: Metrics
    readonly clock: Clock
}
```

The store in a directory, as another process serves it. Close it once, as
a Go program closes its tinystore.Store: `await using store = await open(dir)`.

### Store.events

```ts
events(): AllEvents
events(stream: string, options?: EventsOptions): Events
```

A stream of events by name, made with its first event: events appended
whole and read back by time and by what they hold. Without a name, every
stream, to read.

    const audit = store.events('audit', { keep: '365d' })
    await audit.append('role.changed', { user: 42, role: 'admin' })
    const page = await store.events().list({ since: '1h', minLevel: 'warn' })

### Store.logger

```ts
logger(stream: string, options?: StoreLoggerOptions): Logger
```

A logger of the stream: each line printed as it is logged and kept
within a second, and a call never waits; what its buffer has no room
for is dropped and counted. The store's close hands over what it holds.
Its options are the config `log`'s, whose level a change of any process
sets for every logger of the store once each has its config open:

    const log = store.logger('api', { redact: secrets })
    log.info('signed in', { user: 42 })
    const settings = await store.config('log', [{ level: live('info') }])
    await settings.update({ level: 'debug' })

### Store.bucket

```ts
bucket<T = unknown>(name: string, options?: BucketOptions): Bucket<T>
```

A bucket of values by key, JSON unless `type` says otherwise:

    const sessions = store.bucket<Session>('sessions', { idle: '30d' })
    const codes = store.bucket<number>('login-codes', { ttl: '15m', type: 'int' })
    const seen = store.bucket('stripe-events', { ttl: '7d' })   // keys alone: a set

### Store.counters

```ts
counters(name: string, options?: CountersOptions): Counters
```

Numbers by key that only add up: `store.counters('login-attempts', { ttl: '15m' })`.

### Store.rateLimit

```ts
rateLimit(name: string, options: RateLimitOptions): RateLimit
```

A smooth rate of requests a key: `store.rateLimit('api', { rate: '100/s', burst: 20 })`.

### Store.quota

```ts
quota<W extends string>(name: string, windows: Record<W, Rate>): Quota<W>
```

Uses by key in named windows, each counted from a key's first use, all
of them or none: `store.quota('ai', { daily: '100/1d', weekly: '300/7d' })`.

### Store.index

```ts
index<T = unknown>(name: string, options?: IndexOptions): SearchIndex<T>
```

Documents of one type set by id and found again by their words, by
vectors the program's model gives them, or by both:

    const articles = store.index<Article>('articles', { fields: { title: 2, body: 1 } })
    const { hits } = await articles.search('как испечь хлеб')

Opening an index with other settings than it holds changes it, and
never waits for the work a change needs.

### Store.reindex

```ts
reindex<T = unknown>(name: string, options?: IndexOptions): Promise<SearchIndex<T>>
```

Opens an index as `index` does and reads it again beside itself in the
background: a change `rebuild: 'manual'` left waiting, or its documents
numbered again. It answers once the rebuild began; `ready()` waits for it.

### Store.dropIndex

```ts
dropIndex(name: string): Promise<boolean>
```

Drops an index and every document it holds, in the background; says whether there was one.

### Store.indexes

```ts
indexes(): Promise<IndexInfo[]>
```

Every index the store holds: its documents and vectors, when it was last opened, and a rebuild under way.

### Store.channel

```ts
channel<T = unknown>(name: string): Channel<T>
```

Messages of one type that every subscriber listening when one is
published receives, in memory and at most once:
`const chat = store.channel<Chat>('chat'); await chat.under(roomId).publish(message)`.

### Store.lock

```ts
lock(name: string, options?: LockOptions): Lock
```

Keys held one holder at a time, across every program of the store:
`await store.lock('invoices').hold(invoice.id, lease => charge(invoice, lease.fence))`.

### Store.semaphore

```ts
semaphore(name: string, options: SemaphoreOptions): Semaphore
```

Keys held by up to `limit` at a time: `store.semaphore('uploads', { limit: 3 })`.

### Store.once

```ts
once<T = unknown>(name: string, options?: OnceOptions): Once<T>
```

A function run once a key, its JSON answer kept a day unless `keep` says: `store.once<Receipt>('charges')`.

### Store.queue

```ts
queue<S extends StandardSchemaV1>(name: string, options: QueueOptions & {
        schema: S
    }): Queue<StandardSchemaV1.InferOutput<S>>
queue<T = unknown>(name: string, options?: QueueOptions): Queue<T>
```

A queue of jobs of one type, JSON, run in the order of their time; a
Standard Schema, zod's or valibot's, gives it its type and checks each
value before a handler gets it:

    const emails = store.queue<Email>('emails', { attempts: 20, backoff: { initial: '5s', max: '30m' } })
    const refreshes = store.queue('refreshes', { schema: Refresh, concurrency: { total: 8, group: 2 } })

### Store.schedule

```ts
schedule(name: string, when: ScheduleWhen, handler: (run: Run) => unknown, options?: ScheduleOptions): Schedule
```

A repeat the code owns: one repeating job under name, and handler run at
each of its times, started at once as a queue's work is. The code's
repeat replaces the one kept each time the program opens it.

    store.schedule('cleanup', { cron: '10 3 * * *', timeZone: 'Europe/Berlin' }, async run => {
      await purgeDeletedBefore(daysBefore(run.at, 30))
    })

### Store.tx

```ts
tx<T>(fn: (tx: Tx) => T | Promise<T>): Promise<T>
```

Runs fn as one transaction over the handles it takes in with
`tx.with(handle)`, and gives back what fn returns. Reads go at once;
writes commit together when fn returns, after a check that nothing it
read has changed, and fn runs again when something did, five times at
most, so it does nothing else that must happen once.

    await store.tx(async tx => {
      const left = (await tx.with(stock).get(sku)) ?? 0
      if (left < 1) throw new SoldOut()
      await tx.with(stock).set(sku, left - 1)
    })

### Store.config

```ts
config<D extends object>(name: string, layers: readonly [D, ...NoInfer<Layer<D>>[]], options?: ConfigOptions<Value<D>>): Promise<Config<D>>
```

A program's settings as one typed value: the declaration first, every
setting with its default, then objects, files and the environment, each
over the ones before it, and a live setting's change over them all.

    const config = await store.config('app', [
      { port: 8080, db: { url: secret(String) }, limits: { rps: live(100) } },
      file('app.yaml'),
      env('APP', { vars: { port: 'PORT' } }),
    ])

It follows every change, whoever makes it, until the store closes; the
same config opened again in the process shares its one stream.

### Store.backup

```ts
backup(path: string, options?: BackupOptions): Promise<Backup>
```

A backup of every engine of the store as one zip, made while the store
keeps working, written beside `path` and renamed once whole, so that a
backup that fails leaves no zip. A file of the program's goes in only
when `include` names it, and the store's encryption key only when
`includeEncryptionKey` asks; `tinystore restore` puts it back into an empty
directory, every byte checked. Through a server, it takes an admin's
token.

    const backup = await store.backup('backup-2026-10-13.zip', { include: ['secret.key'] })
    if (backup.keyLeftOut) console.warn('keep encryption.key apart: the backup opens with it alone')

### Store.status

```ts
status(): Promise<Status>
```

What the server this store reaches is: its version, the protocol the
connection speaks, the engines it serves and what the connection may do.
A client newer than its server learns here what it may ask for; a call
past it is UnimplementedError, naming the server's version.

### Store.files

```ts
files(name: string, options?: FilesOptions): Files
```

A named set of files by path, written whole and replaced whole:

    const avatars = store.files('avatars')
    const reports = store.files('reports', { ttl: '24h', maxFileSize: '2GiB' })

### Store.database

```ts
database(name: string, options?: DatabaseOptions): Promise<Database>
```

The application's database name, sql/&lt;name>.db, once its migrations
are applied and checked: a migration that fails, or one changed after
it was applied, is told here, at the start. Without migrations the file
opens as it is, an empty one when there is none.

    const db = await store.database('app', { migrations: `${import.meta.dir}/migrations` })

### Store.close

```ts
close(): Promise<void>
```

Stops every worker, each once its handlers under way have answered,
ingests the instruments' last values, hands over the loggers' lines and
cancels every config's stream, then closes the connection. A private child is waited for until it has
exited, so that the directory is free once this returns; the directory's
sidecar goes once it has been idle.

### Store.[Symbol.asyncDispose]

```ts
[Symbol.asyncDispose](): Promise<void>
```

## open

```ts
function open(dir: string, options?: OpenOptions): Promise<Store>
```

Opens the store in a directory through its sidecar, found through SERVE or
started, or through a private child when asked. It returns once the server
has answered, so that a directory that cannot be served fails here.

## connect

```ts
function connect(url: string, options: ConnectOptions): Promise<Store>
```

Connects to a remote server: tls:// checks the server's certificate before
the token leaves; tcp:// sends the token in the clear, for a network its
operator trusts.

## Durability

```ts
type Durability = 'full' | 'os'
```

How far a commit goes before it returns, for the files of a store or of
one database:

    'full'   synced to the disk: it survives a crash of the operating
             system and a power loss. What kv, jobs, files, search and
             every database are unless told.
    'os'     written to the operating system, synced a little later: it
             survives the program's crash, and a power loss may take the
             last commits, never the file. What records and metrics are
             unless told.

## SelfMetrics

```ts
type SelfMetrics = boolean | {
    system?: boolean
}
```

What a store measures of itself: `true` its commits, files and memory and
the machine it runs on, `{ system: false }` the same without the machine.

## Link

```ts
class Link {}
```

Gives calls a connection, dialling again once the last one ended, and runs
a call so that a lost connection costs it what it must: a call whose
REQUEST had not left goes again, so does a read that had, and a write that
had is OutcomeUnknownError.

### Link.connection

```ts
connection(): Promise<Connection>
```

The connection a call goes on.

### Link.ready

```ts
ready(): Connection | undefined
```

The connection a call goes on, when one is there to take it without waiting.

### Link.run

```ts
run<T>(idempotence: Idempotence, attempt: (connection: Connection) => Promise<T>, signal?: AbortSignal): Promise<T>
```

Runs a call on the link's connection. build makes its REQUEST's body for
that connection, whose handles it may open; run sends it.

### Link.close

```ts
close(): Promise<void>
```

Closes the connection and every one to come.

## Clock

```ts
class Clock {}
```

The clock of a store opened with `{ private: true, clock }`. Moving it
forward expires keys, makes jobs due and ages records at once, as Go's
tinystore.Options.Clock does; a store on the system's time refuses it with
InvalidError.

    await using store = await open(dir, { private: true, clock: new Date('2026-10-03T09:00:00Z') })
    await store.clock.advance('1h')

### Clock.now

```ts
now(): Promise<Date>
```

The time the server's clock reads.

### Clock.advance

```ts
advance(by: Duration): Promise<Date>
```

Moves the clock forward by a while, and gives the time it then reads.

### Clock.set

```ts
set(to: Time): Promise<Date>
```

Sets the clock to a time, which may not be before its own, and gives it.

## Code

```ts
type Code = 'invalid' | 'not_found' | 'limit' | 'closed' | 'in_use' | 'conflict' | 'corrupt' | 'too_old' | 'too_new' | 'suspended' | 'outcome_unknown' | 'committed_locally' | 'lost' | 'permission' | 'unimplemented' | 'cancelled' | 'unavailable' | 'internal' | 'protocol' | 'unauthenticated'
```

What failed, as the server's error or the SDK's own says it. Each code is a
class of its own, so a caller catches `ConflictError` as a Go program asks
`errors.Is(err, tinystore.ErrConflict)`.

## What

```ts
type What = Record<string, string | Uint8Array>
```

The item a failure is about, by the names the server gives: a bucket and a
key, a queue and a key, a series' labels, a table and a constraint. A value
whose bytes are not UTF-8, as a key of bytes may be, comes as its bytes.

## Constraint

```ts
interface Constraint {
    kind: 'unique' | 'primaryKey' | 'foreignKey' | 'check' | 'notNull'
    /** the table, when SQLite names it */
    table: string | undefined
    /** the columns, when SQLite names them */
    columns: string[]
    /** a check's name, or its text when it has none; the index of a unique key on an expression */
    name: string | undefined
}
```

A constraint a database's write broke, as SQLite names it: a unique or a
primary key already held is a ConflictError, any other an InvalidError.

    if (err instanceof ConflictError && err.constraint?.columns.includes('email')) return 'that email is taken'

## TinystoreError

```ts
class TinystoreError extends Error {
    readonly code: Code
    readonly what: What
    readonly constraint: Constraint | undefined
}
```

## InvalidError

```ts
class InvalidError extends TinystoreError {}
```

The request cannot be done as asked.

## LimitError

```ts
class LimitError extends TinystoreError {
    readonly limit: string | undefined
    readonly wanted: number | undefined
    readonly bound: number | undefined
}
```

A bound: memory, size or count. Later, or smaller, it may go through. A
limit the store names says which, what the call would have taken of it,
and the bound: `decoded samples`, 120000, 100000.

## limits

```ts
const limits: Readonly<{
    readonly storeMemory: "store memory"
    readonly storeMemoryNow: "store memory, now"
    readonly matchedSeries: "matched series"
    readonly decodedBlocks: "decoded blocks"
    readonly fetchedBytes: "fetched bytes"
    readonly decodedSamples: "decoded samples"
    readonly outputSamples: "output samples"
    readonly outputBuckets: "output buckets"
    readonly recordBytes: "bytes of a record, a block's"
    readonly appendBytes: "bytes of records in one Append, a segment's; split it"
    readonly jobValueBytes: "bytes of a job's value"
    readonly stepBytes: "bytes of a step's answer"
    readonly objectBytes: "bytes of an object, the bucket's MaxSize"
    readonly rowBytes: "bytes of a call's rows"
}>
```

The names a LimitError's `limit` holds, as Go's constants and Python's
`tinystore.limits` spell them, to map a limit without matching its text.

```ts
if (err instanceof LimitError && err.limit === limits.decodedSamples) { … }
```

## NotFoundError

```ts
class NotFoundError extends TinystoreError {}
```

What the call needs is not there: a file to copy or rename.

## ClosedError

```ts
class ClosedError extends TinystoreError {}
```

The store, the handle or the connection closed.

## InUseError

```ts
class InUseError extends TinystoreError {}
```

A name is taken.

## ConflictError

```ts
class ConflictError extends TinystoreError {}
```

A condition or a version no longer holds: read again, then decide.

## CorruptError

```ts
class CorruptError extends TinystoreError {}
```

Stored bytes no longer read.

## TooOldError

```ts
class TooOldError extends TinystoreError {}
```

A time before its engine's window.

## TooNewError

```ts
class TooNewError extends TinystoreError {}
```

A time past its engine's window, ahead of the store's clock.

## SuspendedError

```ts
class SuspendedError extends TinystoreError {}
```

A metrics series in quarantine until its repair.

## OutcomeUnknownError

```ts
class OutcomeUnknownError extends TinystoreError {}
```

A write whose commit may or may not have happened: the connection was lost
while it was in flight, or the server's commit failed. Read what it wrote
before writing again.

## CommittedLocallyError

```ts
class CommittedLocallyError extends TinystoreError {}
```

A synchronous commit its home made that no copy it waits for held in time:
the write stands, and losing that home's disk would lose it.

## PermissionDeniedError

```ts
class PermissionDeniedError extends TinystoreError {}
```

The connection's capability does not allow it: a data token changing a schema.

## UnimplementedError

```ts
class UnimplementedError extends TinystoreError {}
```

A method the server does not have.

## CancelledError

```ts
class CancelledError extends TinystoreError {}
```

The call was cancelled by its signal.

## UnavailableError

```ts
class UnavailableError extends TinystoreError {}
```

The server is closing; the call was not run and may be sent on another connection.

## InternalError

```ts
class InternalError extends TinystoreError {}
```

A fault of the server's own.

## LostError

```ts
class LostError extends TinystoreError {}
```

A lease that ran out or was revoked at its maxHold while its function ran:
another holder may have run beside it. The function's work stands, and a
system that compares fences refused what came late.

## ProtocolError

```ts
class ProtocolError extends TinystoreError {}
```

A frame or a message that breaks the protocol; the connection ends with it.

## UnauthenticatedError

```ts
class UnauthenticatedError extends TinystoreError {}
```

A remote connection whose token the server does not know.

## codes

```ts
const codes: Code[]
```

The codes this SDK knows, as `messages.json` lists the server's.

## errorOf

```ts
function errorOf(code: string, message: string, what?: What): TinystoreError
```

The error a code names. A code this SDK does not know yet is an
`InternalError` keeping the code in its message.

## Page

```ts
interface Page<T, After> {
    items: T[]
    /** the place the next page begins after; undefined once the scan has ended */
    next: After | undefined
}
```

A page of a scan: its items, and where the next begins when more remain.

## withSignal

```ts
function withSignal<T>(signal: AbortSignal, fn: () => T): T
```

Runs fn under a signal: every call it makes, a kv get, a statement, a
scan, rejects once the signal aborts, its stream cancelled on the wire. A
call given a signal of its own, as work and put take one, keeps that one.

```ts
await withSignal(AbortSignal.timeout(2000), async () => {
  const notes = await app.all`select * from notes`
})
```

## DurationText

```ts
type DurationText = Offered | (Spelled<Units> & Record<never, never>)
```

A duration's text, '30d', '1h30m', '15m', '1s' or '250ms', which the type
checks as it is written, so that '1hr' or '90mins' does not compile. Text
from elsewhere, an environment's, is cast as Duration and checked when the
call runs.

## Duration

```ts
type Duration = number | DurationText
```

A length of time: milliseconds, or the text of one, such as '1h30m'.

## Time

```ts
type Time = Date | number
```

A moment: a Date, or unix milliseconds.

## Key

```ts
type Key = string | Uint8Array | number | bigint
```

A kv key's text: a str, a bin, or an integer, which is its decimal spelling.

<!-- Generated by task reference from sdk/js/src/store.ts, sdk/js/src/connection.ts, sdk/js/src/clock.ts, sdk/js/src/errors.ts, sdk/js/src/handles.ts, sdk/js/src/cancel.ts, sdk/js/src/time.ts, sdk/js/src/wire/codec.ts. Edit the doc comments there, not this file. -->
