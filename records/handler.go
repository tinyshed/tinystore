package records

import (
	"context"
	"encoding/json"
	"fmt"
	"log/slog"
	"slices"
	"strconv"
	"time"
)

// Handler queues the application's log lines for stream and never blocks its
// caller: when the buffer is full a line is dropped and counted in Stats. Lines
// of the records engine itself are kept out of the store, or writing a log
// would log again.
//
// A line becomes a record named "log" whose body is the message. The
// attributes of logger.With are its context, who is speaking, and the call's
// are its attributes; a group's keys are written group.key, and a value is
// spelled as slog.JSONHandler spells it, an error as its message. A line
// logged with a context of WithTrace takes its trace and span.
//
// Each line is also written to standard error as it is logged, pretty on a
// terminal and one JSON object a line otherwise, as the Bun and Python
// loggers write theirs; ConsolePretty, ConsoleJSON, ConsoleOff and Stdout
// change that. The engine's own lines reach the console from Info up, and
// never the store.
//
//	slog.New(logs.Handler("api", records.ConsoleJSON, records.Redact("password", "token")))
func (s *Store) Handler(stream string, options ...HandlerOption) slog.Handler {
	return newHandler(s, stream, options)
}

// Handler is a logger's handler without a store: its lines go to the console
// alone, as Store.Handler writes them there, for a program that wants the
// logger and not the records. Opening the store later and calling its Handler
// instead keeps every line too.
//
//	slog.SetDefault(slog.New(records.Handler("app", records.Redact("password"))))
func Handler(stream string, options ...HandlerOption) slog.Handler {
	return newHandler(nil, stream, options)
}

func newHandler(s *Store, stream string, options []HandlerOption) *handler {
	var settings handlerSettings
	for _, option := range options {
		option.handlerOption(&settings)
	}
	return &handler{
		store: s, stream: stream, level: settings.level,
		echo: newEcho(settings), redact: newRedactor(settings.redact),
	}
}

type handler struct {
	store  *Store // nil for a handler of the console alone
	stream string
	level  slog.Leveler // nil keeps every level
	echo   *echo        // nil writes no console line
	redact redactor
	// context and shown never change once built: WithAttrs copies them
	context []Field
	shown   []Field // the context as a pretty console line shows it; see show
	prefix  string
	own     bool
}

func (h *handler) Enabled(_ context.Context, level slog.Level) bool {
	if h.level != nil && level < h.level.Level() {
		return false
	}
	if h.own {
		return h.echo != nil && h.showsOwn(level)
	}
	return h.echo != nil || h.store != nil
}

// showsOwn says whether the console shows one of the engine's own lines: from
// Info up, or from the handler's Level when it has one, since the engine
// writes a Debug summary of its flush every second
func (h *handler) showsOwn(level slog.Level) bool {
	return h.level != nil || level >= slog.LevelInfo
}

func (h *handler) Handle(ctx context.Context, line slog.Record) error {
	record, own := h.record(line)
	record.TraceID, record.SpanID = TraceOf(ctx)
	if h.echo != nil && (!h.own && !own || h.showsOwn(line.Level)) {
		h.print(record, line)
	}
	if h.store != nil && !h.own && !own {
		h.store.enqueue(record)
	}
	return nil
}

// print writes the line to the console: a JSON line as the record spells it,
// a pretty one with its durations as a person reads them
func (h *handler) print(record Record, line slog.Record) {
	if h.echo.pretty {
		record.Context = h.shown
		record.Attrs = nil
		line.Attrs(func(attr slog.Attr) bool {
			record.Attrs = appendAttr(record.Attrs, h.prefix, attr, show)
			return true
		})
		record.Attrs = h.redact.fields(record.Attrs)
	}
	h.echo.write(&record)
}

// enqueue queues a record for the next flush, or drops and counts it when it
// does not fit the format or the store's window, or the buffer has no room
func (s *Store) enqueue(record Record) {
	if !s.fits(&record) {
		s.dropped.Add(1)
		s.droppedInvalid.Add(1)
		return
	}
	s.enqueueMu.RLock()
	defer s.enqueueMu.RUnlock()
	if s.stopping {
		s.dropped.Add(1)
		s.droppedWrite.Add(1)
		return
	}
	select {
	case s.queue <- record:
		s.askForFlush()
	default:
		s.dropped.Add(1)
		s.droppedFull.Add(1)
	}
}

// askForFlush asks for the flush before its interval once the queue is half
// full, once until that flush drains it, so that a burst is written rather than
// dropped and a steady trickle still waits for the interval
func (s *Store) askForFlush() {
	if len(s.queue) >= cap(s.queue)/2 && s.asked.CompareAndSwap(false, true) {
		s.flushSoon()
	}
}

// record maps one line, and says whether it is the records engine's own
func (h *handler) record(line slog.Record) (Record, bool) {
	at := line.Time
	switch {
	case !at.IsZero():
	case h.store != nil:
		at = h.store.now()
	default:
		at = time.Now()
	}
	level, message := line.Level, line.Message
	record := Record{At: at, Stream: h.stream, Name: logName, Level: &level, Body: &message, Context: h.context}
	own := false
	line.Attrs(func(attr slog.Attr) bool {
		own = own || isOwn(attr)
		record.Attrs = appendAttr(record.Attrs, h.prefix, attr, spell)
		return true
	})
	record.Attrs = h.redact.fields(record.Attrs)
	return record, own
}

// fits is a line the format can keep, at a time the store's window accepts
func (s *Store) fits(r *Record) bool {
	return checkRecord(r) == nil && s.window(s.now()).check(r.At) == nil
}

func (h *handler) WithAttrs(attrs []slog.Attr) slog.Handler {
	next := *h
	next.context = slices.Clone(h.context)
	pretty := h.echo != nil && h.echo.pretty
	if pretty {
		next.shown = slices.Clone(h.shown)
	}
	for _, attr := range attrs {
		next.context = appendAttr(next.context, h.prefix, attr, spell)
		if pretty {
			next.shown = appendAttr(next.shown, h.prefix, attr, show)
		}
		next.own = next.own || isOwn(attr)
	}
	next.context = h.redact.fields(next.context)
	next.shown = h.redact.fields(next.shown)
	return &next
}

func (h *handler) WithGroup(name string) slog.Handler {
	if name == "" {
		return h
	}
	next := *h
	next.prefix = h.prefix + name + "."
	return &next
}

// isOwn is the attribute the store's logger gives every line of this engine
func isOwn(attr slog.Attr) bool {
	return attr.Key == "engine" && attr.Value.Resolve().String() == "records"
}

// appendAttr adds one attribute, a group's attributes under its name; an
// empty attribute is dropped, and an empty group's key is no prefix, as in slog
func appendAttr(fields []Field, prefix string, attr slog.Attr, spelling func(slog.Value) string) []Field {
	value := attr.Value.Resolve()
	if value.Kind() == slog.KindGroup {
		if attr.Key != "" {
			prefix += attr.Key + "."
		}
		for _, inner := range value.Group() {
			fields = appendAttr(fields, prefix, inner, spelling)
		}
		return fields
	}
	if attr.Key == "" && value.Equal(slog.Value{}) {
		return fields
	}
	return append(fields, Field{Key: prefix + attr.Key, Value: spelling(value)})
}

// show spells a value for a pretty console line: a duration as Go writes one,
// 1.5s, where the record keeps its nanoseconds as slog.JSONHandler does
func show(value slog.Value) string {
	if value.Kind() == slog.KindDuration {
		return string(appendJSONString(nil, value.Duration().String()))
	}
	return spell(value)
}

// spell writes a resolved value as JSON the way slog.JSONHandler does; NaN and
// the infinities, which it cannot write, are the strings strconv gives them
func spell(value slog.Value) string {
	switch value.Kind() {
	case slog.KindString:
		return string(appendJSONString(nil, value.String()))
	case slog.KindInt64:
		return strconv.FormatInt(value.Int64(), 10)
	case slog.KindUint64:
		return strconv.FormatUint(value.Uint64(), 10)
	case slog.KindFloat64:
		return string(appendJSONFloat(nil, value.Float64()))
	case slog.KindBool:
		return strconv.FormatBool(value.Bool())
	case slog.KindDuration:
		return strconv.FormatInt(int64(value.Duration()), 10)
	case slog.KindTime:
		return string(appendJSONString(nil, value.Time().Format(time.RFC3339Nano)))
	}
	return spellAny(value.Any())
}

func spellAny(value any) string {
	if err, ok := value.(error); ok {
		if _, marshals := value.(json.Marshaler); !marshals {
			return string(appendJSONString(nil, err.Error()))
		}
	}
	encoded, err := json.Marshal(value)
	if err != nil {
		return string(appendJSONString(nil, fmt.Sprint(value)))
	}
	return string(encoded)
}

// Flush writes what the handler has queued, and the records that writers of
// Lines held through a whole flush without a line joining them. It appends one
// segment's worth of input at a time.
//
// A write that fails is dropped and counted with what was to follow it, as a
// full buffer's lines are.
func (s *Store) Flush(ctx context.Context) error {
	return s.flush(ctx, (*lineWriter).handOverIdle)
}

func (s *Store) flushInBackground(ctx context.Context) error {
	started := time.Now()
	beforeAppended := s.appended.Load()
	beforeFull, beforeInvalid, beforeWrite := s.droppedFull.Load(), s.droppedInvalid.Load(), s.droppedWrite.Load()
	err := s.Flush(ctx)
	if s.log.Enabled(ctx, slog.LevelDebug) {
		s.log.Debug("flush finished", "duration", time.Since(started),
			"appended_delta", s.appended.Load()-beforeAppended,
			"dropped_full_delta", s.droppedFull.Load()-beforeFull,
			"dropped_invalid_delta", s.droppedInvalid.Load()-beforeInvalid,
			"dropped_write_delta", s.droppedWrite.Load()-beforeWrite,
			"failed", err != nil)
	}
	return err
}

// flush takes what the queue holds, then has the writers of Lines hand over
// into the queue it emptied, and writes both
func (s *Store) flush(ctx context.Context, handOver func(*lineWriter)) error {
	s.flushMu.Lock()
	defer s.flushMu.Unlock()
	queued := s.drain()
	s.lines.each(handOver)
	return s.appendQueued(ctx, append(queued, s.drain()...))
}

func (s *Store) flushFinal(ctx context.Context) error {
	s.flushMu.Lock()
	defer s.flushMu.Unlock()
	s.lines.stop()
	queued := s.drain()
	s.lines.each((*lineWriter).handOverAll)
	s.enqueueMu.Lock()
	s.stopping = true
	queued = append(queued, s.drain()...)
	s.enqueueMu.Unlock()
	return s.appendQueued(ctx, queued)
}

func (s *Store) appendQueued(ctx context.Context, queued []Record) error {
	pieces := appendsOf(queued)
	for i, piece := range pieces {
		if err := s.appendChecked(ctx, piece); err != nil {
			for _, lost := range pieces[i:] {
				s.dropped.Add(uint64(len(lost)))
				s.droppedWrite.Add(uint64(len(lost)))
			}
			return err
		}
	}
	return nil
}

// appendsOf cuts records into Appends of at most a segment's input each; a
// record alone always fits
func appendsOf(records []Record) [][]Record {
	var pieces [][]Record
	for start := 0; start < len(records); {
		end, input := start, 0
		for end < len(records) && (end == start || input+inputSize(&records[end]) <= maxAppendInput) {
			input += inputSize(&records[end])
			end++
		}
		pieces = append(pieces, records[start:end])
		start = end
	}
	return pieces
}

// drain takes at most a buffer's worth, so that lines arriving meanwhile wait
// for the next flush instead of stretching this one
func (s *Store) drain() []Record {
	s.asked.Store(false)
	var batch []Record
	for range cap(s.queue) {
		select {
		case record := <-s.queue:
			batch = append(batch, record)
		default:
			return batch
		}
	}
	return batch
}
