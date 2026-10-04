# Records API for Go

Every public type, function and constant of the Records engine in `github.com/tinyshed/tinystore/records` and `github.com/tinyshed/tinystore/records/console`, generated from its source. The [Records guide](../../records/README.md) explains how to use them, and the [Bun and Node](../bun/records.md) and [Python](../python/records.md) pages list the same API.

## LimitRecordBytes

```go
const (
	LimitRecordBytes = "bytes of a record, a block's"
	LimitAppendBytes = "bytes of records in one Append, a segment's; split it"
)
```

The names of a record's limits, which a LimitError's Name holds.

## TraceOf

```go
func TraceOf(ctx context.Context) (TraceID, SpanID)
```

TraceOf is the trace and span ctx carries, both zero when it carries none.

## WithTrace

```go
func WithTrace(ctx context.Context, trace TraceID, span SpanID) context.Context
```

WithTrace is ctx carrying a trace and the span inside it. A line logged with it through the Handler, and a record appended with it that names no trace of its own, take both:

	ctx = records.WithTrace(ctx, trace, span)
	logger.InfoContext(ctx, "charged")   →  a record of that trace and span

It takes no dependency on a tracing library: OpenTelemetry's ids are the same bytes, records.TraceID(span.SpanContext().TraceID()).

## Batch

```go
type Batch struct {
	Records []Record
	Next    Cursor
	Expired int
}
```

Batch is what Follow read. Expired counts the segments retention removed before the cursor reached them.

## Budget

```go
type Budget struct {
	Blocks, Bytes, Decoded int
}
```

Budget bounds one Scan before it starts: the blocks it may open, the bytes it may fetch and the records it may decode. Reaching it ends a page early rather than failing the read.

## Cursor

```go
type Cursor struct {
	Segment int64
	Row     int
}
```

Cursor is a place in the sealed segments, in the order they were sealed: the record at Row of the segment Segment. The zero Cursor is the oldest segment still kept.

## Damage

```go
type Damage struct {
	Stream   string
	Segment  int64     // a sealed segment, dropped whole; zero for a head row
	HeadRow  int64     // zero for a segment
	From, To time.Time // the times it held
	Reason   string    // the invariant its bytes broke
}
```

Damage is a row that no longer reads: a head row, or a sealed segment with its blocks. Its records are lost; Drop removes it, so that the reads over it work again.

## DamageError

```go
type DamageError struct {
	Damage Damage
	Err    error
}
```

DamageError is a read, a follow or a seal that met a damaged row. errors.Is finds tinystore.ErrCorrupt in it, and its Damage is what to Drop.

### DamageError.Error

```go
func (e *DamageError) Error() string
```

### DamageError.Unwrap

```go
func (e *DamageError) Unwrap() error
```

## Field

```go
type Field struct {
	Key   string
	Value string
}
```

Field is one key and its value as JSON, spelled as it was given: 1.2300, -0, a big integer and a nested object come back byte for byte.

### Bool

```go
func Bool(key string, value bool) Field
```

### Float

```go
func Float(key string, value float64) Field
```

Float writes the shortest spelling that reads back as the same float. NaN and the infinities, which JSON has no number for, become the strings "NaN", "+Inf" and "-Inf".

### Int

```go
func Int(key string, value int64) Field
```

### JSON

```go
func JSON(key string, value []byte) Field
```

JSON keeps value as it is; Append refuses it unless it is valid JSON.

### String

```go
func String(key, value string) Field
```

## Maintenance

```go
type Maintenance struct {
	SealedSegments, SealedRecords int
	ExpiredSegments, ExpiredHeads int
	// MergedSegments counts the segments a merge moved into another segment of
	// their stream, each keeping its place for Follow. MergedRecords counts
	// the records the merges wrote again.
	MergedSegments, MergedRecords int
	// Damaged counts the head rows that no longer read, which the rest of
	// their heads sealed around.
	Damaged int
}
```

Maintenance is what one Maintain call did.

## Options

```go
type Options struct {
	// Retention is how long a record is kept: a segment goes once its newest
	// record is older, and Append refuses an older one. Fourteen days when
	// zero.
	Retention time.Duration

	// ClockSkew is how far ahead of the store's clock a record's time may be;
	// Append refuses a later one, which would hold its segment past retention.
	// Ten minutes when zero.
	ClockSkew time.Duration

	// SealAge is how long a quiet stream's records wait in the head before
	// they are sealed however few they are, and so how far Follow may lag
	// behind them: longer means fewer, larger segments. An hour when zero.
	SealAge time.Duration

	// Buffer is how many records the handler holds in memory; beyond it a
	// record is dropped and counted, never waited for. 1024 when zero.
	Buffer int

	// Flush is how often the handler writes what it holds, and sooner once
	// half its Buffer waits. A second when zero.
	Flush time.Duration

	// Budget is the most one Read may spend; a query may only narrow it.
	Budget Budget
}
```

## Page

```go
type Page struct {
	Records []Record
	// More says the limit or the budget ended the page before the range did.
	More bool
	// Next is the same query with its range moved past this page.
	Next Query
}
```

Page is one bounded part of an answer, in event-time order. It never splits a timestamp.

## Printer

```go
type Printer struct {
	// contains filtered or unexported fields
}
```

Printer writes records as a logger's console writes its lines: pretty for a person at a terminal, its levels in colour where the terminal shows them, and one JSON object a line otherwise, unless format says which. The tinystore command prints a store's records with one.

### NewPrinter

```go
func NewPrinter(w io.Writer, format console.Format) *Printer
```

NewPrinter prints to w, which is a terminal only when it is a file that is one. console.Off prints nothing.

### Printer.Print

```go
func (p *Printer) Print(r Record)
```

Print writes a record, a whole line a write.

## Query

```go
type Query struct {
	From, To time.Time
	// Since, when it is not zero, starts the range that long before the
	// store's clock; From then stays zero.
	Since time.Duration
	// Streams matches a record of any one of them.
	Streams []string
	// Names matches a record of any one of them.
	Names []string
	// MinLevel matches a level of MinLevel or above. A record without a level
	// does not match.
	MinLevel *slog.Level
	TraceID  TraceID
	// Attrs matches a record holding each field, by key and exact JSON
	// spelling.
	Attrs []Field
	// Context matches a record holding each field, by key and exact JSON
	// spelling.
	Context []Field
	// Search matches a record whose body, or name, holds the text, its case
	// ignored. Each record the rest of the query leaves is read for it, so a
	// search over much text ends pages early at its Budget, never fails.
	Search string
	Newest bool   // newest first; oldest first otherwise
	Limit  int    // records a page returns: 1000 when zero, at most 10000
	Budget Budget // may only narrow Options.Budget
}
```

Query selects the records in \[From, To), or over the last Since, that meet every condition given.

	Query{Since: time.Hour, MinLevel: &warn}            the last hour's warnings
	Query{From: start, To: end, Streams: []string{"api"}}

## Record

```go
type Record struct {
	At     time.Time // unix nanoseconds in the file, read back in UTC
	Stream string    // a namespace the application names: a service, a container
	Name   string    // the event: "click", "checkout"; the slog handler writes "log"
	// Level is absent when nil.
	Level *slog.Level
	// Body is absent when nil, which is not the same as an empty body.
	Body *string
	// TraceID is none when zero.
	TraceID TraceID
	// SpanID is none when zero.
	SpanID SpanID
	// Context says who produced the record. It keeps its order and repeated
	// keys.
	Context []Field
	// Attrs says what happened. It keeps its order and repeated keys.
	Attrs []Field
}
```

Record is one log line or event.

## RecordError

```go
type RecordError struct {
	Index        int
	Stream, Name string
	Err          error
}
```

RecordError is an Append refused because of one record. Nothing was written, so the others may be sent again without it.

### RecordError.Error

```go
func (e *RecordError) Error() string
```

### RecordError.Unwrap

```go
func (e *RecordError) Unwrap() error
```

## SpanID

```go
type SpanID [8]byte
```

## Stats

```go
type Stats struct {
	// Dropped is the sum of the three reasons that follow.
	Appended, Dropped uint64
	// DroppedFull is a full buffer, DroppedInvalid an invalid or
	// out-of-window record, and DroppedWrite a failed flush.
	DroppedFull, DroppedInvalid, DroppedWrite uint64
	SealedSegments, ExpiredSegments           uint64
	MergedSegments                            uint64
	Queries                                   uint64
	// ReadBlocks and ReadBytes are what reads and follows fetched. They count
	// blocks and head rows, and their bytes with the segment rows beside them:
	// the figures a Budget bounds.
	ReadBlocks, ReadBytes uint64
	// Damaged is how many rows this handle has met that no longer read and
	// are not dropped.
	Damaged uint64
}
```

Stats counts this handle's work.

## Store

```go
type Store struct {
	// contains filtered or unexported fields
}
```

### Open

```go
func Open(ctx context.Context, store *tinystore.Store, options Options) (*Store, error)
```

Open opens records.db inside the store. The store closes it and, unless it is Manual, writes what the handler holds every Options.Flush, and seals and expires every minute.

### Store.All

```go
func (s *Store) All(ctx context.Context, query Query) iter.Seq2[Record, error]
```

All walks every record a query selects, a page at a time, holding no snapshot between pages; an error ends the walk after it is yielded.

### Store.Append

```go
func (s *Store) Append(ctx context.Context, batch ...Record) error
```

Append writes every record or none, in one transaction, and a Scan sees them as soon as it returns. A record the format cannot keep, or whose time is past retention or more than ClockSkew ahead of the store's clock, is a \*RecordError naming it. A record of no trace takes ctx's, from WithTrace.

### Store.Close

```go
func (s *Store) Close(ctx context.Context) error
```

Close writes what the handler still holds, lets the work in flight finish and closes records.db; cancellation stops waiting, not the cleanup. The store calls it: an application closes the store instead.

### Store.Damaged

```go
func (s *Store) Damaged() []Damage
```

Damaged lists the rows this handle has met that no longer read, in the order it met them: what Drop removes.

### Store.Drop

```go
func (s *Store) Drop(ctx context.Context, damage Damage) error
```

Drop removes a damaged head row, or a damaged segment with all its blocks, in one transaction, and forgets it. It refuses a row that still reads with tinystore.ErrConflict, so that it removes only what is lost already. Follow counts a dropped segment in Batch.Expired, as it counts retention's.

### Store.Flush

```go
func (s *Store) Flush(ctx context.Context) error
```

Flush writes what the handler has queued, and the records that writers of Lines held through a whole flush without a line joining them. It appends one segment's worth of input at a time.

A write that fails is dropped and counted with what was to follow it, as a full buffer's lines are.

### Store.Follow

```go
func (s *Store) Follow(ctx context.Context, after Cursor, limit int) (Batch, error)
```

Follow returns up to limit records of the sealed segments from after on: segments in the order they were sealed, each one's records in event-time order, and the cursor to continue from. A record reaches Follow once its head is sealed, a segment's worth or SealAge after it arrived; Read sees it at once. A late record is in a later segment than its neighbours in time.

### Store.Handler

```go
func (s *Store) Handler(stream string, options ...console.Option) slog.Handler
```

Handler queues the application's log lines for stream and never blocks its caller: when the buffer is full a line is dropped and counted in Stats. Lines of the records engine itself are kept out of the store, or writing a log would log again.

A line becomes a record named "log" whose body is the message. The attributes of logger.With are its context, who is speaking, and the call's are its attributes; a group's keys are written group.key, and a value is spelled as slog.JSONHandler spells it, an error as its message. A line logged with a context of WithTrace takes its trace and span.

Each line is also written to standard error as it is logged, as console.Handler writes it, and the options are console's: the format, the level, the fields to redact, the environment's names. The engine's own lines reach the console from Info up, and never the store.

	slog.New(logs.Handler("api", console.JSON, console.Redact(console.Secrets...)))

### Store.Lines

```go
func (s *Store) Lines(stream string) io.WriteCloser
```

Lines is a writer for another program's output, a child process's stdout or a file being followed: each line it is given becomes a record in stream, queued as the handler queues its lines. A Write never waits, and a record the buffer has no room for is dropped and counted in Stats. A line larger than one record's bound is dropped and counted too.

The lines of one record are joined first: a stack trace's frames, a traceback, a JSON value printed over several lines. A record still growing waits a flush or two for a line that joins it; Close hands it over.

A record named "log" keeps its text byte for byte as its body; one named "json" or "logfmt" keeps a JSON object's fields or logfmt pairs, which spell its line again. A record's level is taken from where pino, logfmt, glog, Redis, log4j and their kin write it, in colour or not, and its time is when its first line arrived.

### Store.Maintain

```go
func (s *Store) Maintain(ctx context.Context) (Maintenance, error)
```

Maintain removes the segments and head rows retention has passed, seals every head that holds a segment's worth or has waited Options.SealAge, and merges the small segments of the streams it sealed, four of a size at a time. A head row that no longer reads is logged once, counted in Maintenance.Damaged and left for Drop; the rest of its head seals.

### Store.Report

```go
func (s *Store) Report() []tinystore.Measure
```

### Store.Scan

```go
func (s *Store) Scan(ctx context.Context, query Query) (Page, error)
```

Scan returns one page of the records a query selects, in event-time order: oldest first, or newest first when it asks. A page ends where its limit or its budget ran out, never inside one timestamp; Page.More says so, and Page.Next asks for what follows. Nothing is decoded while the snapshot the page was read from is held.

### Store.Snapshot

```go
func (s *Store) Snapshot(ctx context.Context, dir string) ([]tinystore.SnapshotFile, error)
```

Snapshot copies records.db into dir while the engine keeps working; what the handler holds in memory is not in the copy, and the head is.

### Store.Stats

```go
func (s *Store) Stats() Stats
```

## TraceID

```go
type TraceID [16]byte
```

## console.AddSource

```go
const AddSource = addSource(true)
```

AddSource adds where each line was logged, as slog's handlers spell it: source={"function":"main.run","file":"/app/main.go","line":42}, which a pretty line shows as source=app/main.go:42.

## console.HideStream

```go
const HideStream = hiddenStream(true)
```

HideStream leaves the stream out of a pretty line; a JSON line and the store keep it.

## console.KeepURLPasswords

```go
const KeepURLPasswords = keptURLPasswords(true)
```

KeepURLPasswords leaves the password of a URL inside a value as it is. Without it, postgres://ann:hunter2@db/app is kept as postgres://ann:\[redacted]@db/app.

## console.NoEnv

```go
const NoEnv = noEnv(true)
```

NoEnv reads no variable: what the code says is what the handler does.

## console.Secrets

```go
var Secrets = []string{
	"password", "passwd", "passphrase", "secret", "token", "credential", "credentials", "authorization", "cookie",
	"api key", "private key", "secret key", "access key", "signing key", "encryption key",
	"connection string", "dsn",
}
```

Secrets is what Redact takes to hide the usual secrets. It hides a counter named like one too, token\_count among them.

## console.Handler

```go
func Handler(stream string, options ...Option) slog.Handler
```

Handler writes a logger's lines to standard error, and keeps none.

## console.Format

```go
type Format int
```

Format is how the console writes a line. Without one, lines are pretty on a terminal and JSON otherwise.

### console.Pretty

```go
const (
	Pretty Format = iota + 1 // 11:02:11.123 INFO  api  started  port=3000
	JSON                     // one object a line, for a collector
	Off                      // nothing: a store's handler still keeps each line
)
```

## console.Option

```go
type Option interface {
	// contains filtered or unexported methods
}
```

Option changes a logger's lines: on the console, and in the store when the handler is a records store's.

### console.FromEnv

```go
func FromEnv(prefix string) Option
```

FromEnv reads prefix\_LOG\_LEVEL, prefix\_LOG\_FORMAT and prefix\_LOG\_TIME, which win over the code. Without an environment option a handler reads LOG\_LEVEL, LOG\_FORMAT and LOG\_TIME, as FromEnv("") does.

### console.FromLookup

```go
func FromLookup(prefix string, lookup func(name string) (string, bool)) Option
```

FromLookup reads the variables FromEnv names through lookup, the process's environment left alone.

### console.Level

```go
func Level(level slog.Leveler) Option
```

Level keeps lines from level up; without it every line is kept.

### console.Redact

```go
func Redact(names ...string) Option
```

Redact hides the values of fields whose keys name a secret, at any depth of a JSON value. A key names one when some of its words in a row, written together, are a name's: the words of DB\_PASSWORD, PasswordHash, api-key and APIKey are split at '\_', '-', '.', spaces and capitals, the case ignored. The message is not searched.

	Redact("password", "api key")  hides  password, DB_PASSWORD, PasswordHash, api_key, apiKey, APIKEY
	Redact("token")                hides  bot_token, token_count, and not tokens_used

### console.ReplaceAttr

```go
func ReplaceAttr(replace func(groups []string, attr slog.Attr) slog.Attr) Option
```

ReplaceAttr changes or drops each attribute before it is kept and shown, as slog.HandlerOptions' does: groups are the attribute's groups, outermost first, and a zero Attr drops it. It is not called for the time, the level or the message.

### console.To

```go
func To(w io.Writer) Option
```

To writes the lines to w instead of standard error.

## console.Time

```go
type Time int
```

Time is how a pretty line shows its time; a JSON line always has it.

### console.TimeClock

```go
const (
	TimeClock Time = iota + 1 // 11:02:11.123, the default
	TimeFull                  // 2026-10-02 11:02:11.123 +03:00
	TimeOff                   // none, where docker or journald stamp each line
)
```

<!-- Generated by task reference from records/ and records/console/. Edit the doc comments there, not this file. -->
