# Records API for Rust

Every public type, trait and function of the Records engine in `tinystore::records` and `tinystore::console` and `tinystore::aio::records`, generated from its source. The [Records guide](../../guides/records/README.md) explains how to use them, and the [Bun and Node](../bun/records.md), [Python](../python/records.md) and [Go](../go/records.md) pages list the same API.

## Store

```rust
impl Store
```

### Store.logger

```rust
pub fn logger(&self, stream: &str) -> LoggerBuilder<'_>
```

A logger of the stream `stream`: a `tracing` layer that prints each
line as it is logged, and keeps it within a second without ever
making its caller wait.

```rust
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

let store = tinystore::Store::open("data", Default::default())?;
tracing_subscriber::registry().with(store.logger("api").layer()?).init();
tracing::info!(user = 42, "signed in");
```

### Store.events

```rust
pub fn events(&self, stream: &str) -> EventsBuilder<'_>
```

The stream of events `stream`, made the first time it opens: events
appended whole, and read back by time and by what they hold.

### Store.all_events

```rust
pub fn all_events(&self) -> Result<AllEvents>
```

Every stream's events, to read.

## aio::Store

```rust
impl aio::Store
```

### aio::Store.logger

```rust
pub fn logger(&self, stream: &str) -> LoggerBuilder<'_>
```

A logger of the stream `stream`, as `crate::Store::logger` gives
one: a `tracing` layer whose lines go to a buffer and never wait.

### aio::Store.events

```rust
pub fn events(&self, stream: &str) -> EventsBuilder<'_>
```

The stream of events `stream`, made the first time it opens, as
`crate::Store::events` opens it: its appends and reads awaited.

### aio::Store.all_events

```rust
pub fn all_events(&self) -> Result<AllEvents>
```

Every stream's events, to read. The first opens the records engine,
a plain call made once at start, since it may migrate records.db.

## Aggregate

```rust
pub struct Aggregate { /* private fields */ }
```

What `aggregate` makes rows of: spans of `every`, the values of the keys
in `by`, and of the fields in `stats` and `distinct`.

### Aggregate.new

```rust
pub fn new() -> Aggregate
```

### Aggregate.every

```rust
pub fn every(self, span: Duration) -> Self
```

A row a span this long, each beginning at a whole number of them
since 1970; the whole range is one span without it.

### Aggregate.by

```rust
pub fn by<S: Into<String>>(self, keys: impl IntoIterator<Item = S>) -> Self
```

A row a value of each key: a field's, or `stream`, `name` or `level`,
which are the event's own before a field of that name.

### Aggregate.stats

```rust
pub fn stats<S: Into<String>>(self, keys: impl IntoIterator<Item = S>) -> Self
```

The count, sum, least, most and mean of each field's numbers.

### Aggregate.distinct

```rust
pub fn distinct<S: Into<String>>(self, keys: impl IntoIterator<Item = S>) -> Self
```

How many different values each field has.

## Number

```rust
pub enum Number {
    Integer(i128),
    Float(f64),
}
```

A number a field holds: an integer exactly, however large a sum of them
grows, or a float.

### Number.as_f64

```rust
pub fn as_f64(self) -> f64
```

## Row

```rust
pub struct Row {
    pub at: SystemTime,
    pub count: u64,
    pub by: BTreeMap<String, Value>,
    pub stats: BTreeMap<String, Stats>,
    pub distinct: BTreeMap<String, u64>,
}
```

A row of `aggregate`: a span of time and a value of each key `by` named.

## Stats

```rust
pub struct Stats {
    pub samples: u64,
    pub sum: Number,
    pub min: Number,
    pub max: Number,
    pub avg: f64,
}
```

What a row says of the numbers a field holds; a field with no number in
a row has none there.

## StreamInfo

```rust
pub struct StreamInfo {
    pub name: String,
    pub keep: Duration,
    pub retained_from: SystemTime,
    pub newest: SystemTime,
}
```

What a store holds of a stream.

## Top

```rust
pub struct Top {
    pub value: Value,
    pub count: u64,
}
```

A value of a key and how many events hold it.

## Event

```rust
pub struct Event {
    pub at: SystemTime,
    pub stream: String,
    pub name: String,
    pub level: Option<Level>,
    pub message: Option<String>,
    pub fields: Fields,
    pub context: Fields,
    pub trace_id: Option<TraceId>,
    pub span_id: Option<SpanId>,
}
```

An event as a read gives it.

## Fields

```rust
pub struct Fields(_)
```

An event's fields, or its context: keys in the order they were written,
each value the JSON it was written as, so that `1.2300`, an integer past
2⁵³ and a key written twice come back as they went in.

### Fields.of

```rust
pub fn of(value: &impl Serialize) -> Result<Fields>
```

The fields of a struct or a map, in the order it writes them; `()`
is none.

### Fields.read

```rust
pub fn read(object: &str) -> Result<Fields>
```

The fields of a JSON object's text; `null` is none.

### Fields.len

```rust
pub fn len(&self) -> usize
```

### Fields.is_empty

```rust
pub fn is_empty(&self) -> bool
```

### Fields.iter

```rust
pub fn iter(&self) -> impl Iterator<Item = (&str, &str)>
```

Each key with the JSON of its value, in the order they were written.

### Fields.json

```rust
pub fn json(&self, key: &str) -> Option<&str>
```

The JSON a key's value was written as; the last, of a key written
twice.

### Fields.get

```rust
pub fn get<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>>
```

A key's value, read as `T`; `None` without the key.

### Fields.to

```rust
pub fn to<T: DeserializeOwned>(&self) -> Result<T>
```

The fields read as a struct or a map.

## Level

```rust
pub struct Level(_)
```

How grave an event is: one of six names, or a number between two of them
as Go's `slog` counts, which reads as the name under it.

### Level.TRACE

```rust
pub const TRACE: Level
```

### Level.DEBUG

```rust
pub const DEBUG: Level
```

### Level.INFO

```rust
pub const INFO: Level
```

### Level.WARN

```rust
pub const WARN: Level
```

### Level.ERROR

```rust
pub const ERROR: Level
```

### Level.FATAL

```rust
pub const FATAL: Level
```

### Level.of

```rust
pub fn of(number: i32) -> Level
```

The level of `slog`'s number: -4 debug, 0 info, 4 warn, 8 error.

### Level.number

```rust
pub fn number(self) -> i32
```

### Level.name

```rust
pub fn name(self) -> &'static str
```

## NewEvent

```rust
pub struct NewEvent { /* private fields */ }
```

An event to write: its name, and what it carries beside its fields.

### NewEvent.new

```rust
pub fn new(name: &str) -> NewEvent
```

### NewEvent.at

```rust
pub fn at(self, at: SystemTime) -> Self
```

Its time: when it is appended, unless given.

### NewEvent.level

```rust
pub fn level(self, level: Level) -> Self
```

### NewEvent.message

```rust
pub fn message(self, message: &str) -> Self
```

### NewEvent.fields

```rust
pub fn fields(self, fields: &impl Serialize) -> Self
```

What happened: a struct or a map, kept as the JSON it writes.

### NewEvent.context

```rust
pub fn context(self, context: &impl Serialize) -> Self
```

What it happened in: who asked, which request.

### NewEvent.trace

```rust
pub fn trace(self, trace_id: TraceId, span_id: SpanId) -> Self
```

## SpanId

```rust
pub struct SpanId(pub [u8; 8])
```

A span's id: eight bytes, written as 16 hex digits.

## TraceId

```rust
pub struct TraceId(pub [u8; 16])
```

A trace's id: sixteen bytes, written as 32 hex digits.

## AllEvents

```rust
pub struct AllEvents { /* private fields */ }
```

Every stream's events, to read.

### AllEvents.streams

```rust
pub fn streams(&self) -> Result<Vec<StreamInfo>>
```

Every stream with events, by name: what it keeps, where what it keeps
begins, and its newest event's time.

### AllEvents.list

```rust
pub fn list(&self) -> List
```

The events of every stream, or of those `streams` names, found by
the conditions the answer adds.

### AllEvents.poll

```rust
pub fn poll(&self, cursor: Option<&str>) -> Result<PollBatch>
```

Every packed event once, a batch a call, 1000 at most: from `cursor`,
a batch's, or from the oldest event kept without one. With no event
past the cursor, the batch is empty and its cursor the one given;
with no event at all, the cursor where the next packed ones begin.

### AllEvents.poll_limit

```rust
pub fn poll_limit(&self, cursor: Option<&str>, limit: usize) -> Result<PollBatch>
```

As `poll`, `limit` events at most, 10,000 at the most.

### AllEvents.damaged

```rust
pub fn damaged(&self) -> Result<Vec<Damage>>
```

What reads, packing, merges and polls met that no longer reads,
which `drop` removes: each head row and segment with its stream and
its events' times. What this run of the store has not met yet is not
listed.

### AllEvents.drop

```rust
pub fn drop(&self, damage: &Damage) -> Result<()>
```

Removes a head row or a segment that no longer reads, so that the
reads over it work again: a segment with its blocks and places, its
events counted as expired for a poll behind it. What still reads is
refused.

## EventCall

```rust
pub struct EventCall<'e> { /* private fields */ }
```

One event on its way into a stream.

### EventCall.at

```rust
pub fn at(self, at: SystemTime) -> Self
```

Its time, from as long ago as the stream keeps to ten minutes ahead
of the store's clock.

### EventCall.level

```rust
pub fn level(self, level: Level) -> Self
```

### EventCall.message

```rust
pub fn message(self, message: &str) -> Self
```

### EventCall.context

```rust
pub fn context(self, context: &impl Serialize) -> Self
```

What it happened in: who asked, which request.

### EventCall.trace

```rust
pub fn trace(self, trace_id: TraceId, span_id: SpanId) -> Self
```

### EventCall.append

```rust
pub fn append(self, fields: &impl Serialize) -> Result<()>
```

## Events

```rust
pub struct Events { /* private fields */ }
```

A stream of events: written with `append`, which waits for the file, and
read back by time and by what they hold.

### Events.append

```rust
pub fn append(&self, name: &str, fields: &impl Serialize) -> Result<()>
```

Writes an event of `name` with `fields`, a struct or a map, at the
store's time. It returns once the event is written, and a read finds
it at once.

### Events.event

```rust
pub fn event(&self, name: &str) -> EventCall<'_>
```

An append of one event, what it carries beside its fields before its
verb:

```rust
let (actor, change) = (serde_json::json!({ "actor": 7 }), serde_json::json!({ "user": 42 }));
audit.event("role.changed").at(at).context(&actor).append(&change)?;
```

### Events.append_all

```rust
pub fn append_all(&self, events: impl IntoIterator<Item = NewEvent>) -> Result<()>
```

Writes a list of events, all of them or none: a refused one names its
place in the list and its name.

### Events.list

```rust
pub fn list(&self) -> List
```

The stream's events, found by the conditions the answer adds.

### Events.lines

```rust
pub fn lines(&self) -> Result<Lines>
```

A writer of another program's output, a child's standard output or a
file followed: each line it is given becomes an event of this stream,
and a write never waits. What its buffer has no room for, and a line
longer than an event holds, are dropped and counted. Dropped, it hands
over what it holds, a last line without its newline too.

```rust
let mut worker = std::process::Command::new("worker").stdout(std::process::Stdio::piped()).spawn()?;
let mut out = store.events("worker").open()?.lines()?;
std::io::copy(worker.stdout.as_mut().unwrap(), &mut out)?;
```

An event named `log` keeps its text as its message, a line that is no
UTF-8 with U+FFFD for each sequence that spells nothing. A JSON object's
fields and a logfmt line's pairs become the event's fields, named
`json` or `logfmt`, when they spell the line again. Its level is found
where pino, logfmt, glog, Redis, log4j, zerolog and Postgres write it.

## EventsBuilder

```rust
pub struct EventsBuilder<'s> { /* private fields */ }
```

What opens a stream of events: its name, and how long it keeps them.

### EventsBuilder.keep

```rust
pub fn keep(self, keep: Duration) -> Self
```

How long the stream keeps its events. It is the stream's own, kept in
the file: an open that says none leaves it, fourteen days until one
says, and another than an earlier open of this store said is
`invalid`.

### EventsBuilder.open

```rust
pub fn open(self) -> Result<Events>
```

## Lines

```rust
pub struct Lines { /* private fields */ }
```

A writer of another program's lines into a stream, which never waits.

### Lines.dropped

```rust
pub fn dropped(&self) -> u64
```

The lines dropped since the writer began: a full buffer, a line
longer than an event holds, one the file refused.

## LoggerBuilder

```rust
pub struct LoggerBuilder<'s> { /* private fields */ }
```

What opens a logger: its stream, how it prints, the lines it holds.

### LoggerBuilder.level

```rust
pub fn level(self, level: Level) -> Self
```

The least level printed and kept; every level without it, and
`LOG_LEVEL` wins over it.

### LoggerBuilder.console

```rust
pub fn console(self, format: Format) -> Self
```

How lines print: pretty on a terminal and JSON otherwise without
it, and `LOG_FORMAT` wins over it unless it is `Off`.

### LoggerBuilder.time

```rust
pub fn time(self, time: Time) -> Self
```

A pretty line's time; `LOG_TIME` wins over it.

### LoggerBuilder.hide_stream

```rust
pub fn hide_stream(self) -> Self
```

Leaves the stream out of a pretty line.

### LoggerBuilder.to

```rust
pub fn to(self, out: impl Write + Send + 'static) -> Self
```

Prints to `out` instead of standard error.

### LoggerBuilder.redact

```rust
pub fn redact<S: AsRef<str>>(self, names: impl IntoIterator<Item = S>) -> Self
```

Hides the values of fields whose keys name a secret, at any depth,
where they print and where they are kept: `SECRETS` is the usual
names. A message's text is left as it is.

### LoggerBuilder.keep_url_passwords

```rust
pub fn keep_url_passwords(self) -> Self
```

Leaves the password of a URL inside a value as it is.

### LoggerBuilder.replace

```rust
pub fn replace(self, replace: impl Fn(&str, &str) -> Option<String> + Send + Sync + 'static) -> Self
```

Changes each field before it is hidden: given its key and its
JSON, the JSON to keep, text that is no JSON kept as a string, or
none to drop the field.

### LoggerBuilder.source

```rust
pub fn source(self) -> Self
```

Adds where each line was logged: `source={"function":…,"file":…,"line":42}`.

### LoggerBuilder.env

```rust
pub fn env(self, prefix: &str) -> Self
```

Reads `PREFIX_LOG_LEVEL`, `PREFIX_LOG_FORMAT` and `PREFIX_LOG_TIME`
in place of `LOG_LEVEL` and the rest.

### LoggerBuilder.no_env

```rust
pub fn no_env(self) -> Self
```

Reads no variable: what the code says is what the logger does.

### LoggerBuilder.buffer

```rust
pub fn buffer(self, lines: usize) -> Self
```

The lines held between writes, 1024 unless given: a line that finds
them all held is dropped and counted.

### LoggerBuilder.keep

```rust
pub fn keep(self, keep: Duration) -> Self
```

How long the stream keeps its events, as `store.events` takes it.

### LoggerBuilder.layer

```rust
pub fn layer(self) -> Result<Layer>
```

The layer that prints each line `tracing` hands it and keeps it in
the stream within a second.

## Maintenance

```rust
pub struct Maintenance {
    pub expired: u64,
    pub packed: u64,
    pub segments: u64,
    pub merged: u64,
}
```

What one pass of maintenance did.

## maintain

```rust
pub fn maintain(store: &Store) -> Result<Maintenance>
```

Runs the store's records maintenance now, as a store opened without
background work has to.

## PollBatch

```rust
pub struct PollBatch {
    pub events: Vec<Event>,
    pub cursor: String,
    pub expired: u64,
}
```

A batch of `poll`: its events, the cursor after them, and how many events
their streams' `keep` removed before the cursor reached them.

## All

```rust
pub struct All { /* private fields */ }
```

Every event of an answer, read a page at a time as it is walked, and
several pages at once while they hold fewer events than a page.

## List

```rust
pub struct List { /* private fields */ }
```

A read being asked: every condition narrows it, and `page` or `all` runs
it.

### List.count

```rust
pub fn count(&self) -> Result<u64>
```

How many events the read finds.

### List.top

```rust
pub fn top(&self, key: &str) -> Result<Vec<Top>>
```

The most frequent values of `key` and how many events hold each, the
most first and those as frequent in the order of their JSON: ten
unless the read's limit says, 1000 at most. The key is a field's or a
context's, or `stream`, `name` or `level`.

### List.aggregate

```rust
pub fn aggregate(&self, aggregate: &Aggregate) -> Result<Vec<Row>>
```

A row for each span and each value of the keys `by` names, in the
order of their spans: an answer whole, or an error of kind `limit`
past 10,000 rows.

### List.since

```rust
pub fn since(self, span: Duration) -> Self
```

The span before now the range covers; or `from` and `to`.

### List.from

```rust
pub fn from(self, at: SystemTime) -> Self
```

The range's start, which is in it.

### List.to

```rust
pub fn to(self, at: SystemTime) -> Self
```

The range's end, which is not in it.

### List.streams

```rust
pub fn streams<S: Into<String>>(self, streams: impl IntoIterator<Item = S>) -> Self
```

Any of these streams, for a read of every stream.

### List.names

```rust
pub fn names<S: Into<String>>(self, names: impl IntoIterator<Item = S>) -> Self
```

Any of these names: `["log"]` is the lines.

### List.min_level

```rust
pub fn min_level(self, level: Level) -> Self
```

This level and worse. An event without a level is not found.

### List.trace_id

```rust
pub fn trace_id(self, trace_id: TraceId) -> Self
```

### List.filter

```rust
pub fn filter(self, key: &str, value: &impl Serialize) -> Self
```

`key` with this value, at the top of an event's fields or of its
context, by the JSON the value is written as: `42` finds `42`, and
neither `"42"` nor `42.0`.

### List.search

```rust
pub fn search(self, text: &str) -> Self
```

This text as a phrase, its case ignored: in an event's message, in
its name, or in a value of its fields or its context at any depth.

### List.newest

```rust
pub fn newest(self) -> Self
```

Newest first; oldest first without it.

### List.limit

```rust
pub fn limit(self, events: usize) -> Self
```

The events of a page: 1000, and 10,000 at most.

### List.after

```rust
pub fn after(self, next: &str) -> Self
```

A page's `next`, with the rest of the read as it was: the page after
it, in the range the first page had.

### List.page

```rust
pub fn page(&self) -> Result<Page>
```

One page, from one moment of the file. It never splits a time, and
may hold fewer events than its limit, or none, and still have a
`next`: a read ends at what it may fetch rather than failing.

### List.field_names

```rust
pub fn field_names(&self) -> Result<Vec<String>>
```

The keys the events of the read's range hold, in their fields and
their contexts, sorted: as many as its limit, 1000 unless it says. A
packed segment that reaches into the range gives every key it holds,
which the file keeps beside its events.

### List.field_names_prefix

```rust
pub fn field_names_prefix(&self, prefix: &str) -> Result<Vec<String>>
```

The keys `field_names` gives that begin with `prefix`.

### List.tail

```rust
pub fn tail(&self) -> Result<Tail>
```

The events appended from now on that meet the read's conditions, a
batch as they come: a tail takes no range, order, limit or after.

### List.all

```rust
pub fn all(self) -> All
```

Every event of the answer, a page at a time.

## PAGE_EVENTS

```rust
pub const PAGE_EVENTS: usize
```

The events of a page unless its `limit` says.

## Page

```rust
pub struct Page {
    pub events: Vec<Event>,
    pub next: Option<String>,
}
```

One page of an answer. `next` is the page after it, for `after`; the last
page has none.

## Damage

```rust
pub struct Damage {
    pub part: Part,
    pub id: i64,
    pub stream: String,
    pub first: SystemTime,
    pub last: SystemTime,
    pub error: String,
}
```

A head row or a segment that no longer reads, as `damaged` lists it and
`drop` takes it.

## Part

```rust
pub enum Part {
    HeadRow,
    Segment,
}
```

What holds a damage's events: a head row those not packed yet, a segment
packed ones.

## Tail

```rust
pub struct Tail { /* private fields */ }
```

The events appended after a tail opened that meet its read, a batch as
they come; walked as an iterator, it ends when the store closes.

### Tail.next_within

```rust
pub fn next_within(&mut self, wait: Duration) -> Result<Option<TailBatch>>
```

The next batch, waiting up to `wait` for it: none when none came in
that time, and `Closed` once the store has closed.

## TailBatch

```rust
pub struct TailBatch {
    pub events: Vec<Event>,
    pub missed: u64,
}
```

A batch of a tail: its events, and how many events left the store's hold
before the tail read them.

## Layer

```rust
pub struct Layer { /* private fields */ }
```

A logger's layer: what `tracing` hands it is printed, and kept by a
store's logger. A clone is the same logger, for `flush` and `dropped` once
the subscriber holds the layer.

### Layer.flush

```rust
pub fn flush(&self)
```

Writes what the logger holds and returns once it is written, as the
store does every second and when it closes; then flushes the console.

### Layer.dropped

```rust
pub fn dropped(&self) -> u64
```

The lines dropped since the logger began: a full buffer, a line the
file refused, an event without a name or whose fields are no object.

### Layer.event

```rust
pub fn event(&self, name: &str, fields: &impl Serialize)
```

Writes an event named `name` with `fields`, a struct or a map, and
neither a level nor a message, as a line is written: at once, and
dropped under load. Its context is the current span's.

## Replace

```rust
pub type Replace = Arc<dyn Fn(&str, &str) -> Option<String> + Send + Sync>
```

What changes each field before it is hidden and kept: given its key and
its JSON, the value to keep, or none to drop the field.

## Time

```rust
pub enum Time {
    Clock,
    Full,
    Off,
}
```

How a pretty line's time shows; a JSON line always has its own.

## SECRETS

```rust
pub const SECRETS: &[&str]
```

The usual names of secrets, which `redact` takes to hide them. It hides a
counter named like one too, `token_count` among them.

## logger

```rust
pub fn logger(stream: &str) -> LoggerBuilder
```

A logger of the console alone, its lines printed and kept nowhere: the
same calls, the same options but a store's, and the same lines as a
store's logger.

```rust
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

let console = tinystore::console::logger("app").redact(tinystore::console::SECRETS).layer();
tracing_subscriber::registry().with(console).init();
tracing::info!(user = 42, "signed in");
```

## LoggerBuilder

```rust
pub struct LoggerBuilder { /* private fields */ }
```

What a logger of the console alone is told before its layer is made.

### LoggerBuilder.level

```rust
pub fn level(self, level: Level) -> Self
```

The least level printed and kept; every level without it, and
`LOG_LEVEL` wins over it.

### LoggerBuilder.console

```rust
pub fn console(self, format: Format) -> Self
```

How lines print: pretty on a terminal and JSON otherwise without
it, and `LOG_FORMAT` wins over it unless it is `Off`.

### LoggerBuilder.time

```rust
pub fn time(self, time: Time) -> Self
```

A pretty line's time; `LOG_TIME` wins over it.

### LoggerBuilder.hide_stream

```rust
pub fn hide_stream(self) -> Self
```

Leaves the stream out of a pretty line.

### LoggerBuilder.to

```rust
pub fn to(self, out: impl Write + Send + 'static) -> Self
```

Prints to `out` instead of standard error.

### LoggerBuilder.redact

```rust
pub fn redact<S: AsRef<str>>(self, names: impl IntoIterator<Item = S>) -> Self
```

Hides the values of fields whose keys name a secret, at any depth,
where they print and where they are kept: `SECRETS` is the usual
names. A message's text is left as it is.

### LoggerBuilder.keep_url_passwords

```rust
pub fn keep_url_passwords(self) -> Self
```

Leaves the password of a URL inside a value as it is.

### LoggerBuilder.replace

```rust
pub fn replace(self, replace: impl Fn(&str, &str) -> Option<String> + Send + Sync + 'static) -> Self
```

Changes each field before it is hidden: given its key and its
JSON, the JSON to keep, text that is no JSON kept as a string, or
none to drop the field.

### LoggerBuilder.source

```rust
pub fn source(self) -> Self
```

Adds where each line was logged: `source={"function":…,"file":…,"line":42}`.

### LoggerBuilder.env

```rust
pub fn env(self, prefix: &str) -> Self
```

Reads `PREFIX_LOG_LEVEL`, `PREFIX_LOG_FORMAT` and `PREFIX_LOG_TIME`
in place of `LOG_LEVEL` and the rest.

### LoggerBuilder.no_env

```rust
pub fn no_env(self) -> Self
```

Reads no variable: what the code says is what the logger does.

### LoggerBuilder.layer

```rust
pub fn layer(self) -> Layer
```

The layer that prints what `tracing` hands it.

## Format

```rust
pub enum Format {
    Pretty,
    Json,
    Off,
}
```

How a console prints a line. Without one, lines are pretty on a terminal
and JSON otherwise.

## aio::records::EventsBuilder

```rust
pub struct EventsBuilder<'s> { /* private fields */ }
```

What opens a stream of events: its name, and how long it keeps them.

### aio::records::EventsBuilder.keep

```rust
pub fn keep(self, keep: Duration) -> Self
```

How long the stream keeps its events, as
`crate::records::EventsBuilder::keep` says.

### aio::records::EventsBuilder.open

```rust
pub fn open(self) -> Result<Events>
```

Opens the stream: a plain call, made once at start, which may migrate
records.db and write the stream's row the first time.

## aio::records::Events

```rust
pub struct Events { /* private fields */ }
```

A stream of events: its appends wait for its file without a thread, and
its reads run on the store's pool.

### aio::records::Events.blocking

```rust
pub fn blocking(&self) -> &Events
```

The same stream, for its blocking calls.

### aio::records::Events.append

```rust
pub fn append(
    &self,
    name: &str,
    fields: &impl Serialize,
) -> impl Future<Output = Result<()>> + Send + 'static
```

Writes an event of `name` with `fields`, a struct or a map, at the
store's time, its fields serialized when the call is made. It answers
once the event is written, and a read finds it at once.

### aio::records::Events.event

```rust
pub fn event(&self, name: &str) -> EventCall<'_>
```

An append of one event, what it carries beside its fields before its
verb, as `crate::records::Events::event` has.

### aio::records::Events.append_all

```rust
pub fn append_all(
    &self,
    events: impl IntoIterator<Item = NewEvent>,
) -> impl Future<Output = Result<()>> + Send + 'static
```

Writes a list of events, all of them or none, in one commit.

The events are checked in the future's first poll, at most 4 MiB an
append, and queued at records.db's writer, whose commit runs on the
store's pool. Up to 256 KiB, a head row's worth, they are cut into rows
and compressed in that poll too; a heavier append, or one that comes
while another compresses, is compressed on the pool. When the store's
memory has no room for them yet, the append waits for it on no thread,
and is compressed on the pool once the room comes. Once polled, the
events commit and reach every tail open, whether or not anyone still
awaits them.

### aio::records::Events.list

```rust
pub fn list(&self) -> List
```

The stream's events, found by the conditions the answer adds.

### aio::records::Events.lines

```rust
pub fn lines(&self) -> Result<Lines>
```

A writer of another program's output, as
`crate::records::Events::lines` opens one: a write never waits, and
a flush is awaited.

## aio::records::EventCall

```rust
pub struct EventCall<'e> { /* private fields */ }
```

One event on its way into a stream.

### aio::records::EventCall.at

```rust
pub fn at(self, at: SystemTime) -> Self
```

Its time, from as long ago as the stream keeps to ten minutes ahead
of the store's clock.

### aio::records::EventCall.level

```rust
pub fn level(self, level: Level) -> Self
```

### aio::records::EventCall.message

```rust
pub fn message(self, message: &str) -> Self
```

### aio::records::EventCall.context

```rust
pub fn context(self, context: &impl Serialize) -> Self
```

What it happened in: who asked, which request.

### aio::records::EventCall.trace

```rust
pub fn trace(self, trace_id: TraceId, span_id: SpanId) -> Self
```

### aio::records::EventCall.append

```rust
pub fn append(self, fields: &impl Serialize) -> impl Future<Output = Result<()>> + Send + 'static
```

Writes the event with `fields`, as `Events::append_all` writes one.

## aio::records::AllEvents

```rust
pub struct AllEvents { /* private fields */ }
```

Every stream's events, to read.

### aio::records::AllEvents.blocking

```rust
pub fn blocking(&self) -> &AllEvents
```

The same events, for their blocking calls.

### aio::records::AllEvents.list

```rust
pub fn list(&self) -> List
```

The events of every stream, or of those `streams` names, found by
the conditions the answer adds.

### aio::records::AllEvents.streams

```rust
pub fn streams(&self) -> impl Future<Output = Result<Vec<StreamInfo>>> + Send + 'static
```

Every stream with events, by name, as
`crate::records::AllEvents::streams` lists them.

### aio::records::AllEvents.poll

```rust
pub fn poll(&self, cursor: Option<&str>) -> impl Future<Output = Result<PollBatch>> + Send + 'static
```

Every packed event once, a batch a call, 1000 at most, from `cursor`,
as `crate::records::AllEvents::poll` gives them: an empty batch at
once when nothing past the cursor is packed. A dropped poll leaves the
cursor the program keeps as it was.

### aio::records::AllEvents.poll_limit

```rust
pub fn poll_limit(
    &self,
    cursor: Option<&str>,
    limit: usize,
) -> impl Future<Output = Result<PollBatch>> + Send + 'static
```

As `poll`, `limit` events at most, 10,000 at the most.

### aio::records::AllEvents.damaged

```rust
pub fn damaged(&self) -> impl Future<Output = Result<Vec<Damage>>> + Send + 'static
```

What reads, packing, merges and polls met that no longer reads, as
`crate::records::AllEvents::damaged` lists it.

### aio::records::AllEvents.drop

```rust
pub fn drop(&self, damage: &Damage) -> impl Future<Output = Result<()>> + Send + 'static
```

Removes a head row or a segment that no longer reads, as
`crate::records::AllEvents::drop` does, on the store's pool: it
reads the part again under maintenance's lock, then writes its
removal. Once polled it runs to its end, as a write does.

## aio::records::List

```rust
pub struct List { /* private fields */ }
```

A read being asked: every condition narrows it, and an awaited call runs
it on the store's pool.

### aio::records::List.since

```rust
pub fn since(self, span: Duration) -> Self
```

The span before now the range covers; or `from` and `to`.

### aio::records::List.from

```rust
pub fn from(self, at: SystemTime) -> Self
```

The range's start, which is in it.

### aio::records::List.to

```rust
pub fn to(self, at: SystemTime) -> Self
```

The range's end, which is not in it.

### aio::records::List.streams

```rust
pub fn streams<S: Into<String>>(self, streams: impl IntoIterator<Item = S>) -> Self
```

Any of these streams, for a read of every stream.

### aio::records::List.names

```rust
pub fn names<S: Into<String>>(self, names: impl IntoIterator<Item = S>) -> Self
```

Any of these names: `["log"]` is the lines.

### aio::records::List.min_level

```rust
pub fn min_level(self, level: Level) -> Self
```

This level and worse. An event without a level is not found.

### aio::records::List.trace_id

```rust
pub fn trace_id(self, trace_id: TraceId) -> Self
```

### aio::records::List.filter

```rust
pub fn filter(self, key: &str, value: &impl Serialize) -> Self
```

`key` with this value, at the top of an event's fields or of its
context, by the JSON the value is written as.

### aio::records::List.search

```rust
pub fn search(self, text: &str) -> Self
```

This text as a phrase, its case ignored.

### aio::records::List.newest

```rust
pub fn newest(self) -> Self
```

Newest first; oldest first without it.

### aio::records::List.limit

```rust
pub fn limit(self, events: usize) -> Self
```

The events of a page: 1000, and 10,000 at most.

### aio::records::List.after

```rust
pub fn after(self, next: &str) -> Self
```

A page's `next`, with the rest of the read as it was.

### aio::records::List.page

```rust
pub fn page(&self) -> impl Future<Output = Result<Page>> + Send + 'static
```

One page, from one moment of the file, as
`crate::records::List::page` reads it.

### aio::records::List.field_names

```rust
pub fn field_names(&self) -> impl Future<Output = Result<Vec<String>>> + Send + 'static
```

The keys the events of the read's range hold, sorted, as
`crate::records::List::field_names` gives them.

### aio::records::List.field_names_prefix

```rust
pub fn field_names_prefix(
    &self,
    prefix: &str,
) -> impl Future<Output = Result<Vec<String>>> + Send + 'static
```

The keys `field_names` gives that begin with `prefix`.

### aio::records::List.count

```rust
pub fn count(&self) -> impl Future<Output = Result<u64>> + Send + 'static
```

How many events the read finds. It walks every page of the read on
one thread of the store's pool, several at once while they hold few
events; a drop stops the walk at the page it reads.

### aio::records::List.top

```rust
pub fn top(&self, key: &str) -> impl Future<Output = Result<Vec<Top>>> + Send + 'static
```

The most frequent values of `key` and how many events hold each, as
`crate::records::List::top` counts them, walking the read as
`count` does.

### aio::records::List.aggregate

```rust
pub fn aggregate(
    &self,
    aggregate: &Aggregate,
) -> impl Future<Output = Result<Vec<Row>>> + Send + 'static
```

A row for each span and each value of the keys `by` names, as
`crate::records::List::aggregate` makes them, walking the read as
`count` does.

### aio::records::List.tail

```rust
pub fn tail(&self) -> Result<Tail>
```

The events appended from now on that meet the read's conditions, a
batch as they come, as `crate::records::List::tail` opens a tail:
a plain call, which checks the read and opens the tail.

### aio::records::List.all

```rust
pub fn all(self) -> All
```

Every event of the answer, as a stream: a page's worth at a time, read
on the store's pool once the stream has given the last, never sooner.
A cancellation token around this call stops its reads as one around a
blocking read does.

## aio::records::All

```rust
pub struct All { /* private fields */ }
```

Every event of an answer, a `Stream` that reads a page's worth on the
store's pool each time it has given the last. Dropped while a page is
read, that read stops as a dropped read does.

## aio::records::Tail

```rust
pub struct Tail { /* private fields */ }
```

The events appended after a tail opened that meet its read, a `Stream`
of batches that waits on no thread: each append that brings events wakes
it from the thread that committed them, and the batch is taken in the
poll, from the store's hold of its last megabyte of events. It ends when
the store closes, and dropped, the tail closes.

A wait with a bound is the runtime's: `tokio::time::timeout(d,
tail.next())` ends the wait and keeps the tail open.

## aio::records::Lines

```rust
pub struct Lines { /* private fields */ }
```

A writer of another program's lines into a stream: a write goes to its
buffer and never waits, and a flush is awaited. Dropped, it hands over
what it holds, a last line without its newline too, which the store
writes within a second.

It is no `std::io::Write`, whose flush would block the executor's thread.

### aio::records::Lines.write

```rust
pub fn write(&mut self, bytes: &[u8]) -> Result<()>
```

Takes a piece of output: each line it ends becomes an event. It never
waits, and fails only once the writer or its store closed.

### aio::records::Lines.flush

```rust
pub fn flush(&mut self) -> impl Future<Output = Result<()>> + Send + 'static
```

Writes the events of every line ended so far, and answers once they
are written; a line without its newline yet stays. It runs on the
store's pool, and once polled runs to its end, as a write does.

### aio::records::Lines.dropped

```rust
pub fn dropped(&self) -> u64
```

The lines dropped since the writer began: a full buffer, a line
longer than an event holds, one the file refused.

<!-- Generated by task reference from crates/tinystore through rustdoc. Edit the doc comments there, not this file. -->
