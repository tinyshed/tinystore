// Package records is streams of events, as plan/api/records.md has them: a
// stream opened by name, its events appended whole and read back by time and
// by what they hold, a page at a time, and every stream read together. A
// store's slog handler keeps its lines in a stream through the same append; a
// tail follows what is written from now on, and a writer of lines turns
// another program's output into events.
//
//	audit, err := records.OpenEvents(store, "audit", records.Keep(365*24*time.Hour))
//	err = audit.Append(ctx, "role.changed", records.Fields{"user": 42, "role": "admin"})
//	page, err := events.List(ctx, records.Query{Since: time.Hour, MinLevel: slog.LevelWarn})
package records

import (
	"bytes"
	"context"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"log/slog"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/internal/core"
	"github.com/tinyshed/tinystore/internal/logline"
	"github.com/tinyshed/tinystore/internal/wire"
)

// Fields is what an event holds: any value JSON writes, by key.
type Fields map[string]any

// Where is what a query finds in an event's fields or context: each key with
// this value, by the JSON it was written as.
type Where map[string]any

// Event is an event: what AppendMany writes and what a read gives back. A
// read fills every field it has; a write gives a name and what it knows, its
// time now unless given, from what the stream keeps to ten minutes ahead.
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

// Page is a page of events, and where the next begins: "" on the last.
type Page struct {
	Events []Event
	Next   string
}

// Top is one of a key's most frequent values, and how many events hold it.
type Top struct {
	// Value is the JSON value, its numbers a json.Number.
	Value any
	Count int
}

// Stats is a field's numbers in a row: an integer's sum exact however large
// it grows, or a float's, as json.Number spells them; Samples are the
// numbers, which Avg divides Sum by.
type Stats struct {
	Samples       int
	Sum, Min, Max json.Number
	Avg           float64
}

// Row is a row of an aggregate: a span of Every and a value of each key of
// By, with its count.
type Row struct {
	// At is where the row's span begins, counted from 1970 in UTC.
	At    time.Time
	Count int
	// By is each key's value, its numbers a json.Number; nil for events without it.
	By       map[string]any
	Stats    map[string]Stats
	Distinct map[string]int
}

// Stream is a stream with events: what it keeps, where that begins, and its
// newest event's time.
type Stream struct {
	Name string
	Keep time.Duration
	// RetainedFrom is its oldest event, or where its keep begins while it holds older.
	RetainedFrom time.Time
	Newest       time.Time
}

// Batch is every event once, a batch a poll: its events, where the next
// poll begins, and what expired before it.
type Batch struct {
	Events []Event
	// Cursor is where the next poll begins, kept once these events are safe.
	Cursor string
	// Expired is the events keep removed before the cursor reached them.
	Expired int
}

// TailBatch is a tail's batch: the events written since the last, and how
// many it missed.
type TailBatch struct {
	Events []Event
	Missed int
}

// Damage is a head row or a segment that no longer reads, as Damaged lists
// it and Drop takes it.
type Damage struct {
	// Part is "head row" or "segment".
	Part        string
	ID          int64
	Stream      string
	First, Last time.Time
	Error       string
}

// Option is how a stream opens.
type Option interface{ open(*openOptions) }

type openOptions struct{ keep *time.Duration }

type keep time.Duration

// Keep is how long the stream keeps its events, kept in its file with it:
// an events handle and a store's handler of one name share it. An open that
// gives none changes nothing; a stream nobody gave one keeps 14 days.
func Keep(d time.Duration) keep { return keep(d) }

func (k keep) open(o *openOptions) { d := time.Duration(k); o.keep = &d }

func (k keep) Apply(s *logline.Settings) { d := time.Duration(k); s.Keep = &d }

// AppendOption is what an append gives besides a name and fields.
type AppendOption interface{ append(*Event) }

type at time.Time

// At is the event's time: from what its stream keeps to ten minutes ahead.
func At(t time.Time) AppendOption { return at(t) }

func (a at) append(e *Event) { e.At = time.Time(a) }

type givenContext struct{ value any }

// Context is what the event happened in: who asked, which request.
func Context(value any) AppendOption { return givenContext{value} }

func (c givenContext) append(e *Event) { e.Context = c.value }

// WithTrace is ctx carrying a trace and the span inside it, as a traceparent
// header's 32 and 16 hex digits: a line logged and an event appended with it,
// however deep, take them. Ids that are not hex of that length are no trace,
// as W3C's traceparent ignores them; a span may be "".
func WithTrace(ctx context.Context, traceID, spanID string) context.Context {
	trace, err := readTraceID(traceID, "a trace id")
	if err != nil {
		return ctx
	}
	var span [8]byte
	if spanID != "" {
		if span, err = readSpanID(spanID, "a span id"); err != nil {
			return ctx
		}
	}
	return logline.WithTrace(ctx, trace, span)
}

func readTraceID(text, what string) (id [16]byte, err error) {
	return id, hexInto(id[:], text, what)
}

func readSpanID(text, what string) (id [8]byte, err error) {
	return id, hexInto(id[:], text, what)
}

// hexInto reads an id's hex digits into id, which says how many there are.
func hexInto(id []byte, text, what string) error {
	decoded, err := hex.DecodeString(text)
	if err != nil || len(decoded) != len(id) {
		return core.Errorf(core.Invalid, "%s %q: %d hex digits, as a traceparent header writes it", what, text, 2*len(id))
	}
	copy(id, decoded)
	return nil
}

// Events is a stream of events: written with Append, which returns once the
// file holds them, and read back.
type Events struct {
	store  *core.Store
	name   string
	opener *core.Opener
}

// OpenEvents opens a stream of events. It opens at its first call; its error
// is a name or an option it refuses.
func OpenEvents(store *tinystore.Store, stream string, opts ...Option) (*Events, error) {
	var o openOptions
	for _, opt := range opts {
		opt.open(&o)
	}
	return openEvents(store.CoreHome().Store, stream, o.keep)
}

func openEvents(store *core.Store, stream string, kept *time.Duration) (*Events, error) {
	if err := core.CheckName(stream, "stream"); err != nil {
		return nil, err
	}
	message := wire.RecordsOpen{Stream: stream}
	if kept != nil {
		ms, err := core.Millis(*kept, "stream "+stream+": keep")
		if err != nil {
			return nil, err
		}
		message.Keep = &ms
	}
	opener := &core.Opener{Method: wire.MethodRecordsOpen, Body: wire.Encode(&message)}
	return &Events{store: store, name: stream, opener: opener}, nil
}

// Name is the stream's name.
func (e *Events) Name() string { return e.name }

// Append writes an event: fields any value JSON writes as an object, a Fields
// or a struct. It returns once the event is written, and a read finds it at once.
func (e *Events) Append(ctx context.Context, name string, fields any, opts ...AppendOption) error {
	event := Event{Name: name, Fields: fields}
	for _, opt := range opts {
		opt.append(&event)
	}
	return e.AppendMany(ctx, []Event{event})
}

// AppendMany writes a list of events whole or not at all; a refused event
// names its place in the list and its name.
func (e *Events) AppendMany(ctx context.Context, events []Event) error {
	message := wire.RecordsAppend{Events: make([]wire.RecordsNewEvent, len(events))}
	for place, event := range events {
		written, err := newEvent(ctx, event, place)
		if err != nil {
			return err
		}
		message.Events[place] = written
	}
	return e.appendWire(ctx, message)
}

func (e *Events) appendWire(ctx context.Context, message wire.RecordsAppend) error {
	_, err := core.ViaLink{Link: e.store.Link}.Call(ctx, e.opener, wire.MethodRecordsAppend, func(handle uint64) []byte {
		message.Handle = handle
		return wire.Encode(&message)
	}, core.Writing)
	return err
}

// newEvent is an event as the wire carries it, the context's trace when it gives none.
func newEvent(ctx context.Context, event Event, place int) (wire.RecordsNewEvent, error) {
	named := fmt.Sprintf("event %d %q", place, event.Name)
	if event.Name == "" {
		return wire.RecordsNewEvent{}, core.Errorf(core.Invalid, "%s: an event has a name", named)
	}
	out := wire.RecordsNewEvent{Name: event.Name}
	if !event.At.IsZero() {
		nanos := event.At.UnixNano()
		out.At = &nanos
	}
	if event.Level != nil {
		level := int64(event.Level.Level())
		out.Level = &level
	}
	if event.Message != "" {
		out.Message = &event.Message
	}
	var err error
	if out.Fields, err = object(event.Fields, named+": its fields"); err != nil {
		return out, err
	}
	if out.Context, err = object(event.Context, named+": its context"); err != nil {
		return out, err
	}
	trace, span := logline.TraceOf(ctx)
	if event.TraceID != "" {
		if trace, err = readTraceID(event.TraceID, named+": its trace id"); err != nil {
			return out, err
		}
		span = [8]byte{}
		if event.SpanID != "" {
			if span, err = readSpanID(event.SpanID, named+": its span id"); err != nil {
				return out, err
			}
		}
	}
	out.TraceId, out.SpanId = wireTrace(trace, span)
	return out, nil
}

// wireTrace is a trace and its span as the wire carries them: both, or
// neither, since the store keeps a span with its trace.
func wireTrace(trace [16]byte, span [8]byte) (*[]byte, *[]byte) {
	if trace == ([16]byte{}) || span == ([8]byte{}) {
		return nil, nil
	}
	traceID, spanID := trace[:], span[:]
	return &traceID, &spanID
}

// object is a value's JSON when it is an object with a key, none for nil, null or {}.
func object(value any, what string) (*string, error) {
	var text []byte
	switch given := value.(type) {
	case nil:
		return nil, nil
	case json.RawMessage:
		text = given
	default:
		var out bytes.Buffer
		encoder := json.NewEncoder(&out)
		encoder.SetEscapeHTML(false)
		if err := encoder.Encode(value); err != nil {
			return nil, core.Wrap(core.Invalid, err, "%s: JSON cannot write them", what)
		}
		text = out.Bytes()
	}
	text = bytes.TrimSpace(text)
	if string(text) == "null" || string(text) == "{}" {
		return nil, nil
	}
	if len(text) == 0 || text[0] != '{' || !json.Valid(text) {
		return nil, core.Errorf(core.Invalid, "%s: %.40s is no JSON object", what, text)
	}
	spelled := string(text)
	return &spelled, nil
}

// eventOf is an event a read gave.
func eventOf(read wire.RecordsEvent) Event {
	event := Event{At: time.Unix(0, read.At).UTC(), Stream: read.Stream, Name: read.Name}
	if read.Level != nil {
		event.Level = slog.Level(*read.Level)
	}
	if read.Message != nil {
		event.Message = *read.Message
	}
	if read.Fields != nil {
		event.Fields = json.RawMessage(*read.Fields)
	}
	if read.Context != nil {
		event.Context = json.RawMessage(*read.Context)
	}
	if read.TraceId != nil {
		event.TraceID = hex.EncodeToString(*read.TraceId)
	}
	if read.SpanId != nil {
		event.SpanID = hex.EncodeToString(*read.SpanId)
	}
	return event
}

func eventsOf(read []wire.RecordsEvent) []Event {
	events := make([]Event, len(read))
	for i, each := range read {
		events[i] = eventOf(each)
	}
	return events
}

// Lines is a writer of another program's output into this stream, each line
// an event at the time it arrived; a stack trace's or a JSON value's lines
// make one, and a write never waits. Close hands over what it holds.
//
//	out, err := worker.Lines(ctx)
//	cmd.Stdout = out
//	err = cmd.Run()
//	err = out.Close()
func (e *Events) Lines(ctx context.Context) (*Lines, error) {
	c, err := e.store.Link.Connection(ctx)
	if err != nil {
		return nil, err
	}
	handle, err := core.HandleOn(ctx, c, e.opener)
	if err != nil {
		return nil, err
	}
	stream, err := opened(ctx, c, wire.MethodRecordsLines, wire.Encode(&wire.Handle{Handle: handle}), false)
	if err != nil {
		return nil, err
	}
	return newLines(e.store, stream, core.Piece(c)), nil
}

// opened is a stream of the method, open once its RESPONSE came; end leaves
// the client nothing to send.
func opened(ctx context.Context, c *core.Connection, method uint16, body []byte, end bool) (*core.Stream, error) {
	stream, err := c.Session.Open(ctx, method, body, end)
	if err != nil {
		return nil, err
	}
	if _, err := stream.Next(ctx); err != nil {
		stream.Cancel()
		return nil, err
	}
	return stream, nil
}

func decode(answer []byte, m wire.Message) error {
	if err := wire.Decode(answer, m); err != nil {
		return core.Wrap(core.Protocol, err, "an answer that does not read")
	}
	return nil
}
