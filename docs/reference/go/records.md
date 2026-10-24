# Records API for Go

Every public type, function and constant of the Records engine in `tinyshed.org/tinystore/records` and `tinyshed.org/tinystore/console`, generated from its source. The [Records guide](../../guides/records/README.md) explains how to use them, and the [Bun and Node](../bun/records.md), [Python](../python/records.md) and [Rust](../rust/records.md) pages list the same API.

## Buffer

```go
func Buffer(lines int) console.Option
```

Buffer is the lines a store's handler holds for the store, 1024 unless given: past it a line is dropped and counted, never waited for.

## Keep

```go
func Keep(d time.Duration) keep
```

Keep is how long the stream keeps its events, kept in its file with it: an events handle and a store's handler of one name share it. An open that gives none changes nothing; a stream nobody gave one keeps 14 days.

## WithTrace

```go
func WithTrace(ctx context.Context, traceID, spanID string) context.Context
```

WithTrace is ctx carrying a trace and the span inside it, as a traceparent header's 32 and 16 hex digits: a line logged and an event appended with it, however deep, take them. Ids that are not hex of that length are no trace, as W3C's traceparent ignores them; a span may be "".

## AllEvents

```go
type AllEvents struct {
	// contains filtered or unexported fields
}
```

AllEvents is every stream's events, to read, and what a store holds of them.

### OpenAllEvents

```go
func OpenAllEvents(store *tinystore.Store) (*AllEvents, error)
```

OpenAllEvents is every stream's events of the store, to read.

### AllEvents.Aggregate

```go
func (a *AllEvents) Aggregate(ctx context.Context, q Query) ([]Row, error)
```

Aggregate is a row for each span of Every and each value of the keys of By, with its count, the Stats of its fields and their Distinct values.

### AllEvents.All

```go
func (a *AllEvents) All(ctx context.Context, q Query) iter.Seq2[Event, error]
```

All is every event the query finds, a page after another, holding nothing between pages.

### AllEvents.Count

```go
func (a *AllEvents) Count(ctx context.Context, q Query) (int, error)
```

Count is how many events the query finds.

### AllEvents.Damaged

```go
func (a *AllEvents) Damaged(ctx context.Context) ([]Damage, error)
```

Damaged is what reads met that no longer reads, which Drop removes: each head row and segment with its times.

### AllEvents.Drop

```go
func (a *AllEvents) Drop(ctx context.Context, damage Damage) error
```

Drop removes a head row or a segment that Damaged listed, so that the reads over it work again; one that still reads is never dropped.

### AllEvents.FieldNames

```go
func (a *AllEvents) FieldNames(ctx context.Context, q Query) ([]string, error)
```

FieldNames is the keys the events hold, in fields and in context, sorted; Prefix narrows them.

### AllEvents.List

```go
func (a *AllEvents) List(ctx context.Context, q Query) (Page, error)
```

List is one page of every stream's events, from one moment of the file; Streams narrows them.

### AllEvents.Poll

```go
func (a *AllEvents) Poll(ctx context.Context, cursor string) (Batch, error)
```

Poll is every event once, a batch a call, from a cursor the program keeps, the oldest event kept from "". With no event past the cursor, the batch is empty and its cursor the one given. It reads what the store has packed, which a stream's waiting events are once a segment's worth or an hour old.

### AllEvents.Streams

```go
func (a *AllEvents) Streams(ctx context.Context) ([]Stream, error)
```

Streams is every stream with events, by name, with what it keeps, where that begins and its newest event's time.

### AllEvents.Tail

```go
func (a *AllEvents) Tail(ctx context.Context, q Query) iter.Seq2[TailBatch, error]
```

Tail is the events written from now on that the query finds, a batch as they are written. A reader slower than the writers skips what left the store's hold, and the batch after says how many. It ends when the loop is left, ctx is done or the store closes.

### AllEvents.Top

```go
func (a *AllEvents) Top(ctx context.Context, key string, q Query) ([]Top, error)
```

Top is a key's most frequent values and how many events hold each: ten unless Limit asks for more, 1000 at most. The key is a field's or a context's, or stream, name or level.

## AppendOption

```go
type AppendOption interface {
	// contains filtered or unexported methods
}
```

AppendOption is what an append gives besides a name and fields.

### At

```go
func At(t time.Time) AppendOption
```

At is the event's time: from what its stream keeps to ten minutes ahead.

### Context

```go
func Context(value any) AppendOption
```

Context is what the event happened in: who asked, which request.

## Batch

```go
type Batch struct {
	Events []Event
	// Cursor is where the next poll begins, kept once these events are safe.
	Cursor string
	// Expired is the events keep removed before the cursor reached them.
	Expired int
}
```

Batch is every event once, a batch a poll: its events, where the next poll begins, and what expired before it.

## Damage

```go
type Damage struct {
	// Part is "head row" or "segment".
	Part        string
	ID          int64
	Stream      string
	First, Last time.Time
	Error       string
}
```

Damage is a head row or a segment that no longer reads, as Damaged lists it and Drop takes it.

## Event

```go
type Event struct {
	// At is the zero time for now on a write.
	At time.Time
	// Stream is the stream a read found it in.
	Stream string
	// Name is what happened; "log" for a logger's line.
	Name string
	// Level is nil for an event without one, and slog's number on a read.
	Level   slog.Leveler
	Message string
	// Fields is what happened: on a write any value JSON writes as an object,
	// a Fields or a struct; on a read the object's JSON as the file keeps it, a
	// json.RawMessage, its spelling, its keys' order and a key written twice
	// as they were. Nil when it has none.
	Fields any
	// Context is what it happened in, as Fields is: a logger's With, an Append's Context.
	Context any
	// TraceID is 32 hex digits, as a traceparent header writes it: the
	// context's trace on a write unless given.
	TraceID string
	// SpanID is 16 hex digits.
	SpanID string
}
```

Event is an event: what AppendMany writes and what a read gives back. A read fills every field it has; a write gives a name and what it knows, its time now unless given, from what the stream keeps to ten minutes ahead.

## Events

```go
type Events struct {
	// contains filtered or unexported fields
}
```

Events is a stream of events: written with Append, which returns once the file holds them, and read back.

### OpenEvents

```go
func OpenEvents(store *tinystore.Store, stream string, opts ...Option) (*Events, error)
```

OpenEvents opens a stream of events. It opens at its first call; its error is a name or an option it refuses.

### Events.Aggregate

```go
func (e *Events) Aggregate(ctx context.Context, q Query) ([]Row, error)
```

Aggregate is a row for each span of Every and each value of the keys of By, with its count, the Stats of its fields and their Distinct values.

### Events.All

```go
func (e *Events) All(ctx context.Context, q Query) iter.Seq2[Event, error]
```

All is every event the query finds, a page after another, holding nothing between pages.

### Events.Append

```go
func (e *Events) Append(ctx context.Context, name string, fields any, opts ...AppendOption) error
```

Append writes an event: fields any value JSON writes as an object, a Fields or a struct. It returns once the event is written, and a read finds it at once.

### Events.AppendMany

```go
func (e *Events) AppendMany(ctx context.Context, events []Event) error
```

AppendMany writes a list of events whole or not at all; a refused event names its place in the list and its name.

### Events.Count

```go
func (e *Events) Count(ctx context.Context, q Query) (int, error)
```

Count is how many events the query finds.

### Events.FieldNames

```go
func (e *Events) FieldNames(ctx context.Context, q Query) ([]string, error)
```

FieldNames is the keys the events hold, in fields and in context, sorted; Prefix narrows them.

### Events.Lines

```go
func (e *Events) Lines(ctx context.Context) (*Lines, error)
```

Lines is a writer of another program's output into this stream, each line an event at the time it arrived; a stack trace's or a JSON value's lines make one, and a write never waits. Close hands over what it holds.

	out, err := worker.Lines(ctx)
	cmd.Stdout = out
	err = cmd.Run()
	err = out.Close()

### Events.List

```go
func (e *Events) List(ctx context.Context, q Query) (Page, error)
```

List is one page of the stream's events, from one moment of its file.

### Events.Name

```go
func (e *Events) Name() string
```

Name is the stream's name.

### Events.Tail

```go
func (e *Events) Tail(ctx context.Context, q Query) iter.Seq2[TailBatch, error]
```

Tail is the events written from now on that the query finds, a batch as they are written. A reader slower than the writers skips what left the store's hold, and the batch after says how many. It ends when the loop is left, ctx is done or the store closes.

### Events.Top

```go
func (e *Events) Top(ctx context.Context, key string, q Query) ([]Top, error)
```

Top is a key's most frequent values and how many events hold each: ten unless Limit asks for more, 1000 at most. The key is a field's or a context's, or stream, name or level.

## Fields

```go
type Fields map[string]any
```

Fields is what an event holds: any value JSON writes, by key.

## Handler

```go
type Handler struct {
	// contains filtered or unexported fields
}
```

Handler is a slog.Handler of a store's: each line printed as console.NewHandler prints it, and kept in its stream as an event named log, its message the line's, the call's attributes its fields and those of logger.With its context. A call never waits for the store: lines wait in a bounded buffer and go every second, or once half of it waits, and what does not fit, or a write that fails, is dropped and counted, and said in TinyStore's own lines, since no call waits to be told. The store's close hands over what it holds. It is a slog.Handler as any other: slog.SetDefault takes it, and with console.Off it sits beside a program's own handler.

	handler := records.NewHandler(store, "api", console.Redact(console.Secrets...))
	log := slog.New(handler)
	log.Info("signed in", "user", 42)
	handler.Event("signup", "plan", "pro")

### NewHandler

```go
func NewHandler(store *tinystore.Store, stream string, options ...console.Option) *Handler
```

NewHandler is a store's handler of a stream, with console's options and Keep, Buffer and Levels. Its level, the least it keeps, and prints unless Levels gives a Console part, follows the config log, which it opens in the background: a change of it reaches every handler of the store, in every process, until it is reset. A stream's name the store refuses panics, as a pattern regexp.MustCompile refuses does: it is the program's, not its input's.

### Handler.Dropped

```go
func (h *Handler) Dropped() int
```

Dropped is the lines dropped since the handler began: a full buffer, or a write that failed.

### Handler.Enabled

```go
func (h *Handler) Enabled(ctx context.Context, level slog.Level) bool
```

### Handler.Event

```go
func (h *Handler) Event(name string, args ...any)
```

Event writes an event of the stream: named for itself, with no level and no message, its fields slog's key and value pairs, at once and dropped under load as a line is. An event that must not be lost is Append's.

### Handler.EventContext

```go
func (h *Handler) EventContext(ctx context.Context, name string, args ...any)
```

EventContext is Event in ctx, whose trace it takes.

### Handler.Flush

```go
func (h *Handler) Flush(ctx context.Context) error
```

Flush hands what the buffer holds to the store now, as the timer does every second; what a write that failed held is dropped and counted.

### Handler.Handle

```go
func (h *Handler) Handle(ctx context.Context, record slog.Record) error
```

### Handler.WithAttrs

```go
func (h *Handler) WithAttrs(attrs []slog.Attr) slog.Handler
```

### Handler.WithGroup

```go
func (h *Handler) WithGroup(name string) slog.Handler
```

## Levels

```go
type Levels struct {
	Store   slog.Leveler // the least level kept; LOG_LEVEL and the config log win over it
	Console slog.Leveler // the least level printed; LOG_CONSOLE_LEVEL wins over it
}
```

Levels are the least levels a store's handler keeps and prints, each part left out as the option is: a nil Store keeps every level, and a nil Console prints what is kept, following the config log's changes of it. console.Level(level) is Levels{Store: level}, and either replaces the other.

	records.Levels{Store: slog.LevelInfo, Console: slog.LevelWarn}  keeps info+, prints warn+
	records.Levels{Console: slog.LevelWarn}                         keeps every level, prints warn+

### Levels.Apply

```go
func (l Levels) Apply(s *logline.Settings)
```

## Lines

```go
type Lines struct {
	// contains filtered or unexported fields
}
```

Lines is a writer of another program's output into a stream, as Events.Lines begins it: each line becomes an event, and a write never waits. It sends whole lines, so that what it has no room for, Dropped counts as lines. The store's close hands over what it holds.

### Lines.Close

```go
func (l *Lines) Close() error
```

Close ends the writer: what it holds goes, a last line without its newline too, and it returns once the store wrote them.

### Lines.Dropped

```go
func (l *Lines) Dropped() int
```

Dropped is the lines dropped: past what the writer holds, and, once it closed, by the store.

### Lines.Write

```go
func (l *Lines) Write(p []byte) (int, error)
```

Write takes a piece of output, cut anywhere; it never waits.

## Option

```go
type Option interface {
	// contains filtered or unexported methods
}
```

Option is how a stream opens.

## Page

```go
type Page struct {
	Events []Event
	Next   string
}
```

Page is a page of events, and where the next begins: "" on the last.

## Query

```go
type Query struct {
	// Since is the span before now; or From and To.
	Since time.Duration
	// From is the range's start, included, and To its end, excluded; open when zero.
	From, To time.Time
	// Streams is any of these streams, for every stream's read; a stream reads its own.
	Streams []string
	// Names is any of these names: {"log"} is the lines.
	Names []string
	// MinLevel is this level and worse; an event without a level is not found.
	MinLevel slog.Leveler
	TraceID  string
	Where    Where
	// Search is this text in the message, the name or a value, as a phrase, its case ignored.
	Search string
	// Newest reads newest first; oldest first without it.
	Newest bool
	// Limit is a page's events, 1000 and 10,000 at most; a top's values, ten
	// and 1000 at most; field names' keys.
	Limit int
	// After is a page's Next, with the rest of the query as it was.
	After string

	// Every is the span of an aggregate's row: the range is one without it.
	Every time.Duration
	// By is keys whose every value has rows of its own.
	By []string
	// Stats is fields whose numbers a row sums.
	Stats []string
	// Distinct is fields whose different values a row counts.
	Distinct []string
	// Prefix narrows the keys FieldNames gives.
	Prefix string
}
```

Query is what a read finds: every condition given holds, and a condition left at its zero value is not given. A program that may lack a trace id checks it before it asks, since "" reads every trace.

	records.Query{Since: time.Hour, MinLevel: slog.LevelWarn}
	records.Query{From: start, To: end, Streams: []string{"api"}, Where: records.Where{"user": 42}}

## Row

```go
type Row struct {
	// At is where the row's span begins, counted from 1970 in UTC.
	At    time.Time
	Count int
	// By is each key's value, its numbers a json.Number; nil for events without it.
	By       map[string]any
	Stats    map[string]Stats
	Distinct map[string]int
}
```

Row is a row of an aggregate: a span of Every and a value of each key of By, with its count.

## Stats

```go
type Stats struct {
	Samples       int
	Sum, Min, Max json.Number
	Avg           float64
}
```

Stats is a field's numbers in a row: an integer's sum exact however large it grows, or a float's, as json.Number spells them; Samples are the numbers, which Avg divides Sum by.

## Stream

```go
type Stream struct {
	Name string
	Keep time.Duration
	// RetainedFrom is its oldest event, or where its keep begins while it holds older.
	RetainedFrom time.Time
	Newest       time.Time
}
```

Stream is a stream with events: what it keeps, where that begins, and its newest event's time.

## TailBatch

```go
type TailBatch struct {
	Events []Event
	Missed int
}
```

TailBatch is a tail's batch: the events written since the last, and how many it missed.

## Top

```go
type Top struct {
	// Value is the JSON value, its numbers a json.Number.
	Value any
	Count int
}
```

Top is one of a key's most frequent values, and how many events hold it.

## Where

```go
type Where map[string]any
```

Where is what a query finds in an event's fields or context: each key with this value, by the JSON it was written as.

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
var Secrets = logline.Secrets
```

Secrets is what Redact takes to hide the usual secrets. It hides a counter named like one too, token\_count among them.

## console.NewHandler

```go
func NewHandler(stream string, options ...Option) slog.Handler
```

NewHandler writes a logger's lines to standard error, and keeps none.

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

### console.Format.Apply

```go
func (f Format) Apply(s *logline.Settings)
```

## console.Option

```go
type Option = logline.Option
```

Option changes a logger's lines: on the console, and in the store when the handler is a store's.

### console.FromEnv

```go
func FromEnv(prefix string) Option
```

FromEnv reads prefix\_LOG\_LEVEL, prefix\_LOG\_CONSOLE\_LEVEL, prefix\_LOG\_FORMAT and prefix\_LOG\_TIME, each of which wins over its own option. Without an environment option a handler reads LOG\_LEVEL and the rest, as FromEnv("") does.

### console.FromLookup

```go
func FromLookup(prefix string, lookup func(name string) (string, bool)) Option
```

FromLookup reads the variables FromEnv names through lookup, the process's environment left alone.

### console.Level

```go
func Level(level slog.Leveler) Option
```

Level prints and keeps lines from level up; without it every line is. On a store's handler it is records.Levels{Store: level}, which sets the level it keeps and the level it prints apart.

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

### console.Time.Apply

```go
func (t Time) Apply(s *logline.Settings)
```

<!-- Generated by task reference from sdk/go/records/ and sdk/go/console/. Edit the doc comments there, not this file. -->
