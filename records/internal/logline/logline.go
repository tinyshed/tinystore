// Package logline is a logger's line as a console writes it and the records
// store keeps it. It is shared by records/console, the logger, and records,
// which keeps the lines, so that a program logging to the console alone links
// neither SQLite nor zstd.
package logline

import (
	"context"
	"log/slog"
	"math"
	"strconv"
	"time"
	"unicode/utf8"
)

// LogName is the name of a record a logger writes for a line, as against an event's.
const LogName = "log"

// Field is a key and its value as JSON.
type Field struct {
	Key   string
	Value string
}

// Line is one line of a logger, its fields already JSON.
type Line struct {
	At      time.Time
	Stream  string
	Name    string      // LogName, or an event's name
	Level   *slog.Level // nil for an event
	Body    *string
	TraceID [16]byte // none when zero
	SpanID  [8]byte  // none when zero
	Context []Field
	Attrs   []Field
}

// Keeper takes a logger's lines beside its console: the records store.
type Keeper interface {
	// Keep queues a line and never blocks; a full queue drops it.
	Keep(line Line)
	Now() time.Time
}

// NewHandler is records/console's handler with a keeper beside its console,
// options being console's. records/console sets it as it loads, so that the
// records engine's handler is the console's and the store's options are its own.
var NewHandler func(stream string, keeper Keeper, options any) slog.Handler

// WithTrace is ctx carrying a trace and the span inside it.
func WithTrace(ctx context.Context, trace [16]byte, span [8]byte) context.Context {
	return context.WithValue(ctx, traceKey{}, tracing{trace: trace, span: span})
}

// TraceOf is the trace and span ctx carries, both zero when it carries none.
func TraceOf(ctx context.Context) (trace [16]byte, span [8]byte) {
	if carried, found := ctx.Value(traceKey{}).(tracing); found {
		return carried.trace, carried.span
	}
	return trace, span
}

type traceKey struct{}

type tracing struct {
	trace [16]byte
	span  [8]byte
}

// AppendFloat writes the shortest spelling that reads back as the same float;
// NaN and the infinities, which JSON has no number for, are the strings
// strconv gives them.
func AppendFloat(out []byte, value float64) []byte {
	if math.IsNaN(value) || math.IsInf(value, 0) {
		return AppendString(out, strconv.FormatFloat(value, 'g', -1, 64))
	}
	return strconv.AppendFloat(out, value, 'g', -1, 64)
}

// AppendString quotes as encoding/json does, without escaping HTML. Control
// characters and the line and paragraph separators are escaped, and an invalid
// byte becomes the replacement character.
func AppendString(out []byte, value string) []byte {
	const hex = "0123456789abcdef"
	out = append(out, '"')
	for i := 0; i < len(value); {
		r, size := utf8.DecodeRuneInString(value[i:])
		switch {
		case r == '"' || r == '\\':
			out = append(out, '\\', byte(r))
		case r == '\n':
			out = append(out, '\\', 'n')
		case r == '\r':
			out = append(out, '\\', 'r')
		case r == '\t':
			out = append(out, '\\', 't')
		case r == '\b':
			out = append(out, '\\', 'b')
		case r == '\f':
			out = append(out, '\\', 'f')
		case r == utf8.RuneError && size == 1:
			out = utf8.AppendRune(out, utf8.RuneError)
		case r < 0x20 || r == 0x2028 || r == 0x2029:
			out = append(out, '\\', 'u', hex[r>>12&0xf], hex[r>>8&0xf], hex[r>>4&0xf], hex[r&0xf])
		default:
			out = append(out, value[i:i+size]...)
		}
		i += size
	}
	return append(out, '"')
}
