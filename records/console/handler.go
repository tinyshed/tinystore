package console

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

	"github.com/tinyshed/tinystore/records/internal/logline"
)

// handler writes each line to its console and hands it to its keeper, a
// records store, when it has one. A line becomes a record named "log" whose
// body is the message: the attributes of logger.With are its context, who is
// speaking, and the call's are its attributes; a group's keys are written
// group.key, and a value is spelled as slog.JSONHandler spells it, an error as
// its message. A line logged with records.WithTrace's context takes its trace.
type handler struct {
	keeper    logline.Keeper // nil for a handler of the console alone
	stream    string
	level     slog.Leveler  // nil keeps every level
	echo      *logline.Echo // nil writes no console line
	redact    redactor
	replace   func([]string, slog.Attr) slog.Attr
	addSource bool
	// context and shown never change once built: WithAttrs copies them
	context []logline.Field
	shown   []logline.Field // the context as a pretty console line shows it; see show
	groups  []string
	prefix  string // the groups, each followed by a dot
	own     bool
}

func newHandler(stream string, keeper logline.Keeper, options []Option) *handler {
	s := settings{env: defaultEnvironment}
	for _, option := range options {
		option.apply(&s)
	}
	s, ignored := s.env.overSettings(s)
	h := &handler{
		keeper: keeper, stream: stream, level: s.level, echo: newEcho(s),
		redact: newRedactor(s.redact, s.keepURLPasswords), replace: s.replace, addSource: s.addSource,
	}
	if h.echo != nil {
		sayIgnored(h.echo, ignored, h.now())
	}
	return h
}

// newEcho is where a handler's lines go, or nil for nowhere: a terminal gets
// them pretty, and a pipe or a file JSON, unless an option says which
func newEcho(s settings) *logline.Echo {
	out := s.to
	if out == nil {
		out = os.Stderr
	}
	echo := logline.NewEcho(out, logline.Format(s.format))
	if echo == nil {
		return nil
	}
	echo.Look.Time, echo.Look.HideStream = lineTime(s.time), s.hideStream
	if s.zone != nil {
		// a test's own writer, whose lines do not depend on the machine's zone
		echo.Look.Zone, echo.Look.Color = s.zone, false
	}
	return echo
}

func lineTime(t Time) logline.Time {
	switch t {
	case TimeFull:
		return logline.Full
	case TimeOff:
		return logline.NoTime
	}
	return logline.Clock
}

func (h *handler) now() time.Time {
	if h.keeper != nil {
		return h.keeper.Now()
	}
	return time.Now()
}

func (h *handler) Enabled(_ context.Context, level slog.Level) bool {
	if h.level != nil && level < h.level.Level() {
		return false
	}
	if h.own {
		return h.echo != nil && h.showsOwn(level)
	}
	return h.echo != nil || h.keeper != nil
}

// showsOwn says whether the console shows one of the records engine's own
// lines: from Info up, or from the handler's Level when it has one, since the
// engine writes a Debug summary of its flush every second
func (h *handler) showsOwn(level slog.Level) bool {
	return h.level != nil || level >= slog.LevelInfo
}

func (h *handler) Handle(ctx context.Context, record slog.Record) error {
	var room [8]attr // most lines' attributes, without an allocation
	attrs, own := h.attrs(room[:0], record)
	line := h.line(record, attrs, spell)
	line.TraceID, line.SpanID = logline.TraceOf(ctx)
	if h.echo != nil && (!h.own && !own || h.showsOwn(record.Level)) {
		h.print(line, attrs)
	}
	if h.keeper != nil && !h.own && !own {
		h.keeper.Keep(line)
	}
	return nil
}

// attr is an attribute with its groups' prefix, its value resolved and ReplaceAttr applied
type attr struct {
	key   string
	value slog.Value
}

// attrs flattens a line's attributes into flat, its source last, and says
// whether the line is the records engine's own
func (h *handler) attrs(flat []attr, record slog.Record) ([]attr, bool) {
	own := false
	record.Attrs(func(a slog.Attr) bool {
		own = own || isOwn(a)
		flat = h.flatten(flat, h.groups, h.prefix, a)
		return true
	})
	if h.addSource && record.PC != 0 {
		flat = append(flat, attr{key: logline.SourceKey, value: slog.AnyValue(sourceOf(record.PC))})
	}
	return flat, own
}

// line is a slog line with its attributes spelled
func (h *handler) line(record slog.Record, attrs []attr, spelling func(slog.Value) string) logline.Line {
	at := record.Time
	if at.IsZero() {
		at = h.now()
	}
	level, message := record.Level, record.Message
	return logline.Line{
		At: at, Stream: h.stream, Name: logline.LogName, Level: &level, Body: &message,
		Context: h.context, Attrs: h.redact.fields(spelled(attrs, spelling)),
	}
}

// print writes the line to the console: a JSON line as the record spells it,
// a pretty one with its durations as a person reads them
func (h *handler) print(line logline.Line, attrs []attr) {
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

func spelled(attrs []attr, spelling func(slog.Value) string) []logline.Field {
	if len(attrs) == 0 {
		return nil
	}
	fields := make([]logline.Field, len(attrs))
	for i, a := range attrs {
		fields[i] = logline.Field{Key: a.key, Value: spelling(a.value)}
	}
	return fields
}

func (h *handler) WithAttrs(attrs []slog.Attr) slog.Handler {
	next := *h
	var flat []attr
	for _, a := range attrs {
		flat = h.flatten(flat, h.groups, h.prefix, a)
		next.own = next.own || isOwn(a)
	}
	next.context = h.redact.fields(append(slices.Clone(h.context), spelled(flat, spell)...))
	if h.echo != nil && h.echo.Pretty {
		next.shown = h.redact.fields(append(slices.Clone(h.shown), spelled(flat, show)...))
	}
	return &next
}

func (h *handler) WithGroup(name string) slog.Handler {
	if name == "" {
		return h
	}
	next := *h
	next.groups = append(slices.Clip(h.groups), name)
	next.prefix = h.prefix + name + "."
	return &next
}

// isOwn is the attribute the store's logger gives every line of the records engine
func isOwn(a slog.Attr) bool {
	return a.Key == "engine" && a.Value.Resolve().String() == "records"
}

// flatten adds one attribute, a group's attributes under its name; an empty
// attribute is dropped, and an empty group's key is no prefix, as in slog
func (h *handler) flatten(out []attr, groups []string, prefix string, a slog.Attr) []attr {
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
// 1.5s, where the record keeps its nanoseconds as slog.JSONHandler does
func show(value slog.Value) string {
	if value.Kind() == slog.KindDuration {
		return string(logline.AppendString(nil, value.Duration().String()))
	}
	return spell(value)
}

// spell writes a resolved value as JSON the way slog.JSONHandler does; NaN and
// the infinities, which it cannot write, are the strings strconv gives them
func spell(value slog.Value) string {
	switch value.Kind() {
	case slog.KindString:
		return string(logline.AppendString(nil, value.String()))
	case slog.KindInt64:
		return strconv.FormatInt(value.Int64(), 10)
	case slog.KindUint64:
		return strconv.FormatUint(value.Uint64(), 10)
	case slog.KindFloat64:
		return string(logline.AppendFloat(nil, value.Float64()))
	case slog.KindBool:
		return strconv.FormatBool(value.Bool())
	case slog.KindDuration:
		return strconv.FormatInt(int64(value.Duration()), 10)
	case slog.KindTime:
		return string(logline.AppendString(nil, value.Time().Format(time.RFC3339Nano)))
	}
	return spellAny(value.Any())
}

func spellAny(value any) string {
	if err, ok := value.(error); ok {
		if _, marshals := value.(json.Marshaler); !marshals {
			return string(logline.AppendString(nil, err.Error()))
		}
	}
	encoded, err := json.Marshal(value)
	if err != nil {
		return string(logline.AppendString(nil, fmt.Sprint(value)))
	}
	return string(encoded)
}
