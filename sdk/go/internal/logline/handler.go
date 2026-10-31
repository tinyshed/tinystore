package logline

import (
	"context"
	"encoding/json"
	"fmt"
	"log/slog"
	"os"
	"runtime"
	"slices"
	"strconv"
	"time"
)

// Handler writes each line to its console and hands it to its keeper, a
// store's, when it has one. A line becomes an event named "log" whose message
// is the line's: the attributes of logger.With are its context, who is
// speaking, and the call's are its fields; a group's keys are written
// group.key, and a value is spelled as slog.JSONHandler spells it, an error as
// its message. A line logged with a context that carries a trace takes it. A
// line is printed from the level printed and kept from the level kept, each
// where the handler has a place for it.
type Handler struct {
	keeper    Keeper // nil for a handler of the console alone
	stream    string
	level     slog.Leveler // the least kept; nil keeps every level
	printed   slog.Leveler // the least printed; nil prints what is kept
	echo      *Echo        // nil writes no console line
	redact    redactor
	replace   func([]string, slog.Attr) slog.Attr
	addSource bool
	// context and shown never change once built: WithAttrs copies them
	context []Field
	shown   []Field // the context as a pretty console line shows it; see show
	groups  []string
	prefix  string // the groups, each followed by a dot
}

// NewHandler is a handler of the stream as s says, which hands its lines to
// keeper too unless it is nil. It says on its console what of the
// environment it could not read.
func NewHandler(stream string, keeper Keeper, s Settings, ignored []Unread) *Handler {
	h := &Handler{
		keeper: keeper, stream: stream, level: s.Level, printed: s.Printed, echo: newEcho(s),
		redact: newRedactor(s.Redact, s.KeepURLPasswords), replace: s.Replace, addSource: s.AddSource,
	}
	if h.echo != nil {
		SayIgnored(h.echo, ignored)
	}
	return h
}

// newEcho is where a handler's lines go, or nil for nowhere: a terminal gets
// them pretty, and a pipe or a file JSON, unless an option says which
func newEcho(s Settings) *Echo {
	out := s.To
	if out == nil {
		out = os.Stderr
	}
	echo := NewEcho(out, s.Format)
	if echo == nil {
		return nil
	}
	echo.Look.Time, echo.Look.HideStream = s.Time, s.HideStream
	if s.Zone != nil {
		// a test's own writer, whose lines do not depend on the machine's zone
		echo.Look.Zone, echo.Look.Color = s.Zone, false
	}
	return echo
}

// Pretty says whether the handler's console lines are a person's.
func (h *Handler) Pretty() bool { return h.echo != nil && h.echo.Pretty }

// Enabled says whether a place the handler has takes a line of level, so that
// slog builds no record that nothing would print or keep.
func (h *Handler) Enabled(_ context.Context, level slog.Level) bool {
	return h.prints(level) || h.keeps(level)
}

// prints says whether a line of level is printed: the handler has a console,
// and the level reaches the level printed, or the level kept without one.
func (h *Handler) prints(level slog.Level) bool {
	least := h.printed
	if least == nil {
		least = h.level
	}
	return h.echo != nil && (least == nil || level >= least.Level())
}

// keeps says whether a line of level is kept: the handler is a store's, and
// the level reaches its level.
func (h *Handler) keeps(level slog.Level) bool {
	return h.keeper != nil && (h.level == nil || level >= h.level.Level())
}

func (h *Handler) Handle(ctx context.Context, record slog.Record) error {
	level := record.Level
	h.write(ctx, record, LogName, &level, &record.Message)
	return nil
}

// Event writes an event of the stream: named for itself, with no level and
// no message, its fields slog's key and value pairs, written as a line is.
func (h *Handler) Event(ctx context.Context, name string, args ...any) {
	var pcs [1]uintptr
	runtime.Callers(3, pcs[:]) // Callers, this function and the package's Event
	record := slog.NewRecord(time.Now(), 0, "", pcs[0])
	record.Add(args...)
	h.write(ctx, record, name, nil, nil)
}

func (h *Handler) write(ctx context.Context, record slog.Record, name string, level *slog.Level, message *string) {
	var room [8]attr // most lines' attributes, without an allocation
	attrs := h.attrs(room[:0], record)
	// a record of the zero time keeps none, and the store gives it its clock's
	line := Line{
		At: record.Time, Stream: h.stream, Name: name, Level: level, Body: message,
		Context: h.context, Attrs: h.redact.fields(spelled(attrs, spell)),
	}
	line.TraceID, line.SpanID = TraceOf(ctx)
	// an event has no level, and goes wherever the handler has a place
	printed, kept := h.echo != nil, h.keeper != nil
	if level != nil {
		printed, kept = h.prints(*level), h.keeps(*level)
	}
	if printed {
		h.print(line, attrs)
	}
	if kept {
		h.keeper.Keep(line)
	}
}

// attr is an attribute with its groups' prefix, its value resolved and ReplaceAttr applied
type attr struct {
	key   string
	value slog.Value
}

// attrs flattens a line's attributes into flat, its source last
func (h *Handler) attrs(flat []attr, record slog.Record) []attr {
	record.Attrs(func(a slog.Attr) bool {
		flat = h.flatten(flat, h.groups, h.prefix, a)
		return true
	})
	if h.addSource && record.PC != 0 {
		flat = append(flat, attr{key: SourceKey, value: slog.AnyValue(sourceOf(record.PC))})
	}
	return flat
}

// print writes the line to the console: a JSON line as the store keeps it,
// a pretty one with its durations as a person reads them
func (h *Handler) print(line Line, attrs []attr) {
	if h.echo.Pretty {
		line.Context = h.shown
		if slices.ContainsFunc(attrs, isDuration) {
			line.Attrs = h.redact.fields(spelled(attrs, show))
		}
	}
	h.echo.Write(&line)
}

func isDuration(a attr) bool {
	return a.value.Kind() == slog.KindDuration
}

func spelled(attrs []attr, spelling func(slog.Value) string) []Field {
	if len(attrs) == 0 {
		return nil
	}
	fields := make([]Field, len(attrs))
	for i, a := range attrs {
		fields[i] = Field{Key: a.key, Value: spelling(a.value)}
	}
	return fields
}

func (h *Handler) WithAttrs(attrs []slog.Attr) slog.Handler {
	next := *h
	var flat []attr
	for _, a := range attrs {
		flat = h.flatten(flat, h.groups, h.prefix, a)
	}
	next.context = h.redact.fields(onceEach(h.context, spelled(flat, spell)))
	if h.echo != nil && h.echo.Pretty {
		next.shown = h.redact.fields(onceEach(h.shown, spelled(flat, show)))
	}
	return &next
}

// onceEach is context with added after it, each key once: where it was first
// given, holding the value given last, so that a With inside another replaces
// the outer one's key where it prints, in what is kept and for a query's where.
//
//	component=http user=7, then component=access  →  component=access user=7
func onceEach(context, added []Field) []Field {
	once := slices.Clone(context)
	for _, field := range added {
		at := slices.IndexFunc(once, func(held Field) bool { return held.Key == field.Key })
		if at < 0 {
			once = append(once, field)
		} else {
			once[at].Value = field.Value
		}
	}
	return once
}

func (h *Handler) WithGroup(name string) slog.Handler {
	if name == "" {
		return h
	}
	next := *h
	next.groups = append(slices.Clip(h.groups), name)
	next.prefix = h.prefix + name + "."
	return &next
}

// flatten adds one attribute, a group's attributes under its name; an empty
// attribute is dropped, and an empty group's key is no prefix, as in slog
func (h *Handler) flatten(out []attr, groups []string, prefix string, a slog.Attr) []attr {
	a.Value = a.Value.Resolve()
	if a.Value.Kind() != slog.KindGroup && h.replace != nil {
		a = h.replace(groups, a)
		a.Value = a.Value.Resolve()
	}
	if a.Value.Kind() == slog.KindGroup {
		if a.Key != "" {
			groups = append(slices.Clip(groups), a.Key)
			prefix += a.Key + "."
		}
		for _, inner := range a.Value.Group() {
			out = h.flatten(out, groups, prefix, inner)
		}
		return out
	}
	if a.Key == "" && a.Value.Equal(slog.Value{}) {
		return out
	}
	return append(out, attr{key: prefix + a.Key, value: a.Value})
}

// source is where a line was logged, as slog's handlers spell it
type source struct {
	Function string `json:"function"`
	File     string `json:"file"`
	Line     int    `json:"line"`
}

func sourceOf(pc uintptr) source {
	frame, _ := runtime.CallersFrames([]uintptr{pc}).Next()
	return source{Function: frame.Function, File: frame.File, Line: frame.Line}
}

// show spells a value for a pretty console line: a duration as Go writes one,
// 1.5s, where the store keeps its nanoseconds as slog.JSONHandler does
func show(value slog.Value) string {
	if value.Kind() == slog.KindDuration {
		return string(AppendString(nil, value.Duration().String()))
	}
	return spell(value)
}

// spell writes a resolved value as JSON the way slog.JSONHandler does; NaN and
// the infinities, which it cannot write, are the strings strconv gives them
func spell(value slog.Value) string {
	switch value.Kind() {
	case slog.KindString:
		return string(AppendString(nil, value.String()))
	case slog.KindInt64:
		return strconv.FormatInt(value.Int64(), 10)
	case slog.KindUint64:
		return strconv.FormatUint(value.Uint64(), 10)
	case slog.KindFloat64:
		return string(AppendFloat(nil, value.Float64()))
	case slog.KindBool:
		return strconv.FormatBool(value.Bool())
	case slog.KindDuration:
		return strconv.FormatInt(int64(value.Duration()), 10)
	case slog.KindTime:
		return string(AppendString(nil, value.Time().Format(time.RFC3339Nano)))
	}
	return Spell(value.Any())
}

// Spell writes a value as JSON, an error as its message, and what JSON
// cannot write as its text, never a lost line.
func Spell(value any) string {
	if err, ok := value.(error); ok {
		if _, marshals := value.(json.Marshaler); !marshals {
			return string(AppendString(nil, err.Error()))
		}
	}
	encoded, err := json.Marshal(value)
	if err != nil {
		return string(AppendString(nil, fmt.Sprint(value)))
	}
	return string(encoded)
}
