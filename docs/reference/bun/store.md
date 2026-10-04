# Store API for Bun and Node

Every public class, function and type of the store in `@tinyshed/tinystore`, generated from its source. The [Store guide](../../languages.md) explains how to use them, and the [Python](../python/store.md) and [Go](../go/store.md) pages list the same API.

## OpenOptions

```ts
interface OpenOptions {
    /**
     * A server of this process's own, a child on stdin and stdout that lives
     * and dies with it, rather than the directory's shared sidecar: tests,
     * scripts, one process alone.
     */
    private?: boolean
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
}
```

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
    readonly kv: Kv
    readonly jobs: Jobs
    readonly blobs: Blobs
    readonly records: Records
    readonly metrics: Metrics
    readonly clock: Clock
}
```

The store in a directory, as another process serves it. Close it once, as
a Go program closes its tinystore.Store: `await using store = await open(dir)`.

### Store.status

```ts
status(): Promise<Status>
```

What the server this store reaches is: its version, the protocol the
connection speaks, the engines it serves and what the connection may do.
A client newer than its server learns here what it may ask for; a call
past it is UnimplementedError, naming the server's version.

### Store.sql

```ts
sql(name: string, options?: SqlOptions): Promise<Database>
```

Opens a database of the application's own, sql/&lt;name>.db. The first open
in the server, on an admin connection, applies its migrations; every
later one checks them against what the file applied. Without migrations
it opens the file as it is, an empty one if there is none, and checks
nothing.

### Store.backup

```ts
backup(path: string): Promise<void>
```

Writes a backup of the whole store to a zip at path while the store keeps
working, as `tinystore backup` does: every engine's file, with its size
and checksum, which `tinystore restore` checks. The zip is written beside
path and renamed into place once whole, so a backup that fails leaves no
zip. It needs an admin connection; a remote server sends the zip over it.

### Store.close

```ts
close(): Promise<void>
```

Ingests the instruments' last values and hands over the loggers' lines,
then closes the connection. A private child is waited for until it has
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
type Code = 'invalid' | 'limit' | 'closed' | 'in_use' | 'conflict' | 'corrupt' | 'too_old' | 'too_new' | 'suspended' | 'outcome_unknown' | 'permission' | 'unimplemented' | 'cancelled' | 'unavailable' | 'internal' | 'protocol' | 'unauthenticated'
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

## TinystoreError

```ts
class TinystoreError extends Error {
    readonly code: Code
    readonly what: What
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
type DurationText = Spelled<Units>
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

<!-- Generated by task reference from sdk/js/src/store.ts, sdk/js/src/clock.ts, sdk/js/src/errors.ts, sdk/js/src/handles.ts, sdk/js/src/cancel.ts, sdk/js/src/time.ts, sdk/js/src/wire/codec.ts. Edit the doc comments there, not this file. -->
