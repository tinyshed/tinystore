# Records API for Bun and Node

Every public class, function and type of the Records engine in `@tinyshed/tinystore`, generated from its source. The [Records guide](../../guides/records/README.md) explains how to use them, and the [Python](../python/records.md), [Go](../go/records.md) and [Rust](../rust/records.md) pages list the same API.

## Level

```ts
type Level = 'debug' | 'info' | 'warn' | 'error' | number
```

A level a logger logs at: one of its four names, or slog's number between them.

## EventLevel

```ts
type EventLevel = 'trace' | 'debug' | 'info' | 'warn' | 'error' | 'fatal'
```

An event's level: what loggers and the programs whose lines are kept write.

## Fields

```ts
type Fields = Record<string, unknown> | readonly (readonly [string, unknown])[]
```

What happened, or what it happened in: an object, or its pairs in their order.

## Event

```ts
interface Event {
    /** its time, which the file keeps to the nanosecond */
    at: Date
    stream: string
    /** what its append named it by: its stream holds one event of an id */
    id?: string
    /** what happened; `log` for a logger's line */
    name: string
    level?: EventLevel
    message?: string
    /** each value as JSON.parse reads it, an integer past 2^53 a bigint */
    fields: Record<string, unknown>
    context: Record<string, unknown>
    /** 32 hex digits */
    traceId?: string
    /** 16 hex digits */
    spanId?: string
}
```

An event as a read gives it.

## NewEvent

```ts
interface NewEvent {
    name: string
    /**
     * what names it in its stream, a webhook's delivery or a job's key, 1 to 128 bytes:
     * an append of an id the stream holds writes nothing, and the first event of an id stands
     */
    id?: string
    fields?: Fields
    /** its time: now unless given, from what the stream keeps to ten minutes ahead */
    at?: Date
    context?: Fields
    level?: EventLevel | number
    message?: string
    /** a trace's 32 hex digits, with its span's 16: the current trace's unless given */
    traceId?: string
    spanId?: string
}
```

An event to append: its name, what happened, and what it carries beside.

## AppendOptions

```ts
type AppendOptions = Omit<NewEvent, 'name' | 'fields'>
```

## EventsOptions

```ts
interface EventsOptions {
    /** how long the stream keeps its events: its own, kept in the file; 14 days until one says */
    keep?: Duration
}
```

## Query

```ts
interface Query {
    /** the span before now, '1h'; or from and to */
    since?: Duration
    /** the range's start, included */
    from?: Date
    /** the range's end, excluded */
    to?: Date
    /** any of these streams, for every stream's read */
    streams?: readonly string[]
    /** any of these names: ['log'] is the lines */
    names?: readonly string[]
    /** this level and worse; an event without a level is not found */
    minLevel?: EventLevel
    traceId?: string
    /** each key with this value, by the JSON it was written as, in fields or in context */
    where?: Record<string, unknown>
    /** this text in the message, the name or a value, its case ignored */
    search?: string
    /** newest first; oldest first without it */
    newest?: boolean
    /** the events of a page: 1000, and 10,000 at most */
    limit?: number
    /** a page's next, with the rest of the query as it was */
    after?: string
}
```

What a read finds: every condition given holds; one given as undefined is InvalidError.

## Counted

```ts
type Counted = Omit<Query, 'newest' | 'limit' | 'after'>
```

A query of every event it finds, for what counts them.

## Page

```ts
interface Page {
    events: Event[]
    /** the page after it; the last page has none */
    next?: string
}
```

## Top

```ts
interface Top {
    value: unknown
    count: number
}
```

## Stats

```ts
interface Stats {
    samples: number
    sum: number | bigint
    min: number | bigint
    max: number | bigint
    avg: number
}
```

A field's numbers in a row: a sum of integers exact, a bigint past 2^53.

## Row

```ts
interface Row {
    /** where the row's span begins, counted from 1970 in UTC */
    at: Date
    count: number
    by: Record<string, unknown>
    stats: Record<string, Stats>
    distinct: Record<string, number>
}
```

## Aggregate

```ts
interface Aggregate extends Counted {
    /** the span of a row: the range is one without it */
    every?: Duration
    /** rows of a calendar in timeZone, in place of every: 'day' begins each at a local midnight */
    calendar?: Calendar
    /** the IANA zone a calendar's rows are in: 'Europe/Moscow', 'UTC' */
    timeZone?: string
    /** keys whose every value has rows of its own */
    by?: readonly string[]
    /** fields whose numbers a row sums */
    stats?: readonly string[]
    /** fields whose different values a row counts */
    distinct?: readonly string[]
}
```

## StreamInfo

```ts
interface StreamInfo {
    name: string
    keep: DurationText
    /** its oldest event, or where its keep begins while it holds older */
    retainedFrom: Date
    newest: Date
}
```

## PollBatch

```ts
interface PollBatch {
    events: Event[]
    /** where the next poll begins, kept once these events are safe */
    cursor: string
    /** the events keep removed before the cursor reached them */
    expired: number
}
```

## TailQuery

```ts
type TailQuery = Pick<Query, 'streams' | 'names' | 'minLevel' | 'traceId' | 'where' | 'search'>
```

What a tail finds: a query's conditions, and no range, order or limit.

## TailBatch

```ts
interface TailBatch {
    events: Event[]
    /** the events that left the store's hold before the tail read them */
    missed: number
}
```

A tail's batch: the events written since the last, and how many it missed.

## Damage

```ts
interface Damage {
    /** a head row holds events not packed yet, a segment packed ones */
    part: 'head row' | 'segment'
    id: number
    stream: string
    /** its events' oldest and newest times */
    first: Date
    last: Date
    /** what broke */
    error: string
}
```

A head row or a segment that no longer reads, as `damaged` lists it and `drop` takes it.

## Events

```ts
class Events extends Reads {
    readonly stream: string
}
```

A stream of events: written with `append`, which waits for the file, and read back.

### Events.append

```ts
append(name: string, fields?: Fields, options?: AppendOptions): Promise<boolean>
append(events: readonly NewEvent[]): Promise<number>
```

Writes an event, or a list of them whole or not at all: it returns once
they are written, and a read finds them at once. An event whose id the
stream holds, or an earlier event of the list has, is written as
nothing: one event answers whether it was written, a list how many of
its events were. A refused event names its place in the list and its name.

### Events.appendLines

```ts
appendLines(lines: readonly ConsoleLine[]): Promise<void>
```

A logger's lines, each an event of this stream.

### Events.lines

```ts
lines(): Lines
```

A writer of another program's output into this stream: each line it is
given becomes an event at the time it arrived, a stack trace's or a JSON
value's lines one, and a write never waits.

    const out = store.events('worker').lines()
    child.stdout.on('data', chunk => out.write(chunk))
    child.on('close', () => out.end())

## AllEvents

```ts
class AllEvents extends Reads {}
```

Every stream's events, to read: `store.events()`.

### AllEvents.streams

```ts
streams(): Promise<StreamInfo[]>
```

Every stream with events, by name, with what it keeps, where that begins and its newest event's time.

### AllEvents.poll

```ts
poll(cursor?: string, options?: {
        limit?: number
    }): Promise<PollBatch>
```

Every event once, a batch a call, from a cursor the program keeps, the
oldest event kept without one: `{ events, cursor, expired }`. With no
event past the cursor, the batch is empty and its cursor the one given.

### AllEvents.damaged

```ts
damaged(): Promise<Damage[]>
```

What reads met that no longer reads, which `drop` removes: each head
row and segment with its stream and its events' times. What the store
has not met since it opened is not listed.

### AllEvents.drop

```ts
drop(damage: Damage): Promise<void>
```

Removes a head row or a segment that `damaged` listed, so that the
reads over it work again; what still reads is refused.

    for (const damage of await store.events().damaged()) await store.events().drop(damage)

## Lines

```ts
class Lines {}
```

A writer of another program's output into a stream, as `events.lines()`
begins it: each line becomes an event, and a write never waits. It sends
whole lines, so that what it has no room for, `dropped` counts as lines.

### Lines.dropped

```ts
get dropped(): number
```

The lines dropped: past what the writer holds, and, once it ended, by the store.

### Lines.write

```ts
write(chunk: Uint8Array | string): void
```

Takes a piece of output, cut anywhere; it never waits.

### Lines.end

```ts
end(): Promise<void>
```

Ends the writer: what it holds goes, a last line without its newline
too, and it returns once the store wrote them.

## StoreLoggerOptions

```ts
interface StoreLoggerOptions extends Omit<LoggerOptions, 'level'> {
    /** how long the stream keeps its events, as store.events takes it */
    keep?: Duration
    /**
     * the least level kept and printed, every level when absent, or each
     * apart: `{ store: 'info', console: 'warn' }` keeps every line from info
     * and prints warnings and errors. LOG_LEVEL wins over the level kept, and
     * LOG_CONSOLE_LEVEL over the level printed
     */
    level?: Level | Levels
}
```

What a store's logger is told: a logger's options, how long its stream keeps, and the levels it keeps and prints.

## Levels

```ts
interface Levels {
    /** the least level kept; LOG_LEVEL and the config `log` win over it */
    store?: Level
    /** the least level printed; LOG_CONSOLE_LEVEL wins over it */
    console?: Level
}
```

The least levels a store's logger keeps and prints, each part left out as
the option is: no store keeps every level, and no console prints what is
kept, following the config `log`'s changes of it.

```ts
{ level: 'info' }                               // keeps info+, prints info+
{ level: { store: 'info', console: 'warn' } }   // keeps info+, prints warn+
{ level: { console: 'warn' } }                  // keeps every level, prints warn+
```

## LoggerOptions

```ts
interface LoggerOptions {
    /** the lines held between writes; past it a line is dropped and counted: 1024 */
    buffer?: number
    /** the least level a line is printed at, every level when absent */
    level?: Level
    /** how a line is written as it is logged: pretty on a terminal and JSON otherwise when absent */
    console?: ConsoleFormat
    /** how a pretty line shows its time: 'clock' when absent */
    time?: ConsoleTime
    /** leaves the stream out of a pretty line; the record keeps it */
    hideStream?: boolean
    /** where the console lines go: process.stderr when absent */
    to?: ConsoleOut
    /**
     * secrets whose fields are hidden, in the store and on the console, at any
     * depth: a key hides its value when some of its words in a row, written
     * together, are a name's, so 'api key' hides api_key and apiKey; `secrets`
     * holds the usual ones
     */
    redact?: readonly string[]
    /** leaves a URL's password in a value as it is, which is otherwise hidden */
    keepUrlPasswords?: boolean
    /**
     * changes each field's value before it is hidden, kept and shown, and
     * undefined leaves the field out: return the value it was given for a
     * field it leaves as it is
     */
    replace?: (key: string, value: unknown) => unknown
    /**
     * where LOG_LEVEL, LOG_CONSOLE_LEVEL, LOG_FORMAT and LOG_TIME are read,
     * each of which wins over its option: `env('APP')` reads APP_LOG_LEVEL
     * and the rest, and false reads nothing; the bare names when absent
     */
    env?: EnvLayer | false
    /** adds where each line was logged: source={"function":…,"file":…,"line":…} */
    source?: boolean
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
`store.logger(stream)` in its place keeps them too.

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
const log = store.logger('api')
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

The logger of the same stream, buffer and console, its context these
fields besides its own: a key it had already takes the new value where
it stood, as an object spread does, where it prints, in what is kept
and for a query's `where`.

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

## secrets

```ts
const secrets: readonly string[]
```

The names `redact` takes to hide the usual secrets, as Go's console.Secrets and Python's SECRETS.

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
