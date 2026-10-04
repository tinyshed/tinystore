# Records API for Bun and Node

Every public class, function and type of the Records engine in `@tinyshed/tinystore`, generated from its source. The [Records guide](../../records/README.md) explains how to use them, and the [Python](../python/records.md) and [Go](../go/records.md) pages list the same API.

## Level

```ts
type Level = 'debug' | 'info' | 'warn' | 'error' | number
```

slog's levels: -4 debug, 0 info, 4 warn, 8 error, or any other it may name.

## Fields

```ts
type Fields = Record<string, unknown> | readonly (readonly [string, unknown])[]
```

Fields: keys and values, each value written as JSON. An object's keys keep
their order; an array of pairs may repeat a key.

## RecordInput

```ts
interface RecordInput {
    /** its time: a Date, or unix nanoseconds as a bigint; now when absent */
    at?: Date | bigint
    /** the namespace the application names: 'web', 'billing' */
    stream: string
    /** the event: 'log' for a log line */
    name: string
    level?: Level
    /** absent is none, which an empty body is not */
    body?: string | Uint8Array
    /** sixteen bytes, or their 32 hex digits */
    traceId?: Uint8Array | string | undefined
    /** eight bytes, or their 16 hex digits */
    spanId?: Uint8Array | string | undefined
    /** who produced it */
    context?: Fields
    /** what happened */
    attrs?: Fields
}
```

A record to append: when, where, what, and its fields.

## LogRecord

```ts
interface LogRecord {
    /** unix nanoseconds */
    at: bigint
    stream: string
    name: string
    level: number | undefined
    body: string | Uint8Array | undefined
    traceId: Uint8Array | undefined
    spanId: Uint8Array | undefined
    context: [key: string, json: string | Uint8Array][]
    attrs: [key: string, json: string | Uint8Array][]
}
```

A record as it was kept: a field's value is the JSON it was written as, spelled as it was given.

## fields

```ts
function fields(pairs: LogRecord['attrs']): Record<string, unknown>
```

A record's fields, its attrs or its context, as an object, each value read
as JSON.parse reads the JSON it was kept as; the last of a repeated key is
kept. The pairs keep what JSON.parse may not: 1.2300 as written, an integer
past 2^53 whole.

```ts
for await (const record of store.records.all({ since: '1h', streams: ['readers'] })) {
  const { page, from } = fields(record.attrs)
}
```

## RecordsQuery

```ts
interface RecordsQuery {
    /** the span before now the range covers: '1h', '15m', or milliseconds; or from and to */
    since?: Duration
    /** the range's start, included; open when absent */
    from?: Date | bigint
    /** the range's end, excluded; open when absent */
    to?: Date | bigint
    /** none or empty is every stream */
    streams?: string[]
    names?: string[]
    /** a record without a level does not match */
    minLevel?: Level
    traceId?: Uint8Array | string
    /** each a record must hold, by key and exact JSON spelling */
    attrs?: Fields
    context?: Fields
    /**
     * text a record's body or name holds, its case ignored: every record the
     * rest of the query leaves is read for it, so a search over much text ends
     * pages early at the budget, and next goes on
     */
    search?: string
    /** newest first; oldest first when absent */
    newest?: boolean
    /** the records a page holds: 1000, and 10000 at most */
    limit?: number
    /** what one read may open, fetch and decode; each narrows the server's */
    budget?: {
        blocks?: number
        bytes?: number
        records?: number
    }
    /** a page's next, which this page continues from; the rest of the query as it was */
    after?: string | undefined
}
```

The records in a range that meet every condition given.

```ts
{ since: '1h', minLevel: 'warn' }                 // the last hour's warnings
{ since: '1h', search: 'connection reset' }       // whose text holds it, case ignored
{ from: start, to: end, streams: ['api'] }
```

## Cursor

```ts
interface Cursor {
    segment: number
    row: number
}
```

Where a follower stands in the sealed segments: kept by the caller between follows.

## Damage

```ts
interface Damage {
    stream: string
    /** a sealed segment, dropped whole; or a head row */
    segment: number | undefined
    headRow: number | undefined
    /** the times it held, unix nanoseconds */
    from: bigint
    to: bigint
    /** the invariant its bytes broke */
    reason: string
}
```

A row that no longer reads, which a drop removes.

## Records

```ts
class Records {}
```

### Records.logger

```ts
logger(stream: string, options?: LoggerOptions): Logger
```

A logger of a stream: its calls return at once, and its lines reach the
server every second, as Go's slog handler's and Python's logging.Handler's do.

### Records.stop

```ts
stop(): Promise<void>
```

Hands over what every logger holds, as the store does when it closes.

### Records.append

```ts
append(records: RecordInput | readonly RecordInput[]): Promise<void>
```

Appends records in one transaction, all or none; a refused one names
itself as `call`, with its stream and name. A read sees them at once. A
record of no trace of its own takes the one withTrace runs the call in.

### Records.scan

```ts
scan(query?: RecordsQuery): Promise<Page<LogRecord, string>>
```

One page of the records a query matches, from one snapshot, in event-time
order; next, while a limit or a budget ended the page early, is passed back
as `after` with the same query. A page never splits a timestamp.

### Records.all

```ts
all(query?: Omit<RecordsQuery, 'after'>): AsyncGenerator<LogRecord>
```

Every record a query matches, a page at a time.

### Records.follow

```ts
follow(cursor?: Cursor, limit?: number): Promise<{
        records: LogRecord[]
        cursor: Cursor
        expired: number
    }>
```

The sealed records after a cursor, in the order they were sealed; the
cursor to follow from next, and the segments retention removed before
the cursor reached them. A record reaches a follower once it is sealed.

### Records.lines

```ts
lines(stream: string, options?: {
        buffer?: number
    }): LinesWriter
```

A writer of another program's output, pino's lines or a child's stdout,
that never waits: each line becomes a record of the stream as the server's
Lines makes it, a stack trace's lines joined and a JSON or logfmt line's
fields kept. What does not fit its buffer is dropped and counted.

### Records.damaged

```ts
damaged(): Promise<Damage[]>
```

The rows the server's records have met that no longer read.

### Records.drop

```ts
drop(damage: Damage): Promise<void>
```

Removes a damaged row, a repair an admin connection alone may make.

## LinesWriter

```ts
class LinesWriter {
    dropped: number
}
```

An upload of lines that stays open while it is written: write never waits,
holding what the stream's credit does not let go yet up to its buffer and
dropping past it. A connection lost drops what it held and the next write
uploads on the next connection.

### LinesWriter.write

```ts
write(chunk: string | Uint8Array): boolean
```

Takes a piece of output, cut anywhere. It returns at once.

### LinesWriter.end

```ts
end(): Promise<void>
```

Hands over what the writer holds, a line cut short included, and ends the upload.

## LoggerOptions

```ts
interface LoggerOptions {
    /** the lines held between writes; past it a line is dropped and counted: 1024 */
    buffer?: number
    /** the least level a line is kept at; every level when absent */
    level?: Level
    /** how a line is written as it is logged: pretty on a terminal and JSON otherwise when absent */
    console?: ConsoleFormat
    /** how a pretty line shows its time: 'clock' when absent */
    time?: ConsoleTime
    /** leaves the stream out of a pretty line; the record keeps it */
    hideStream?: boolean
    /** where the console lines go: process.stderr when absent */
    to?: ConsoleOut
    /** fields whose values are hidden, in the store and on the console, at any depth, the case ignored */
    redact?: readonly string[]
}
```

## ConsoleOut

```ts
interface ConsoleOut {
    write(text: string): unknown
    readonly isTTY?: boolean
}
```

Where console lines go: process.stdout, a file's stream, or any other writer.

## logger

```ts
function logger(stream: string, options?: LoggerOptions): Logger
```

A logger of the console alone, for a program that wants the logger and not
the records: its lines are written as a store's logger writes them, and
`store.records.logger(stream)` in its place keeps them too.

```ts
const log = logger('app', { redact: ['password'] })
const db = log.with({ module: 'db' })   // passed to what needs it
```

## Logger

```ts
class Logger {}
```

A logger of one stream. `info`, `warn` and the rest return at once; a line
goes to the console as it is logged, and becomes a record named `log`, its
message the body, the call's fields its attributes and the fields of `with`
its context, who is speaking.

```ts
const log = store.records.logger('api')
log.info('server started', { port: 3000 })
log.with({ requestId }).warn('slow request', { ms: 1200 })
log.event('user.created', { userId: 42 })   // a record named for the event
```

### Logger.dropped

```ts
get dropped(): number
```

The lines dropped since the logger began: a full buffer, or a write that
failed. Each is also said on stderr, at most once in ten minutes.

### Logger.debug

```ts
debug(message: string, attrs?: Fields): void
```

### Logger.info

```ts
info(message: string, attrs?: Fields): void
```

### Logger.warn

```ts
warn(message: string, attrs?: Fields): void
```

### Logger.error

```ts
error(message: string, attrs?: Fields): void
```

### Logger.event

```ts
event(name: string, attrs?: Fields): void
```

An event of the stream: a record named for it, with no level and no body.

### Logger.with

```ts
with(context: Fields): Logger
```

The logger of the same stream, buffer and console, its context these fields besides its own.

### Logger.flush

```ts
flush(): Promise<void>
```

Hands what the buffer holds to the server now, as the timer does every second.

### Logger.stop

```ts
stop(): Promise<void>
```

Hands over what the buffer holds and stops the timer; a later line starts it again.

## ConsoleFormat

```ts
type ConsoleFormat = 'pretty' | 'json' | 'off'
```

How a logger writes its lines as they are logged; without one, pretty on a terminal and JSON otherwise.

## ConsoleTime

```ts
type ConsoleTime = 'clock' | 'full' | 'off'
```

How a pretty line shows its time: `11:02:11.123`, `2026-10-02 11:02:11.123 +03:00` or none.

## Trace

```ts
interface Trace {
    traceId: Uint8Array | string
    spanId?: Uint8Array | string
}
```

A trace's 16 bytes and the span's 8, as bytes or hex.

## withTrace

```ts
function withTrace<T>(trace: Trace, fn: () => T): T
```

Runs fn in a trace: a logger's lines, and records appended without a trace
of their own, inside it and in what it awaits, take its trace and span.

```ts
await withTrace({ traceId, spanId }, async () => {
  log.info('charged')   // a record of that trace and span
})
```

<!-- Generated by task reference from sdk/js/src/records.ts, sdk/js/src/logger.ts, sdk/js/src/console.ts, sdk/js/src/trace.ts. Edit the doc comments there, not this file. -->
