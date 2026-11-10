package logline

import (
	"encoding/hex"
	"encoding/json"
	"io"
	"log/slog"
	"os"
	"strconv"
	"strings"
	"sync"
	"time"

	"tinyshed.org/tinystore/internal/term"
)

// A console line is pretty for a person at a terminal, one JSON object a line
// for a collector. The Bun and Python loggers write the same bytes, which
// testdata/console/console.json holds:
//
//	11:02:11.123 WARN  api  slow request  ms=1200
//	{"time":"2026-10-02T11:02:11.123Z","level":"WARN","stream":"api","msg":"slow request","ms":1200}

// Format is how a console writes its lines: pretty on a terminal and JSON
// otherwise when it is FormatDefault.
type Format int

const (
	FormatDefault Format = iota
	Pretty
	JSON
	Off
)

// Time is how a pretty line shows its time; a JSON line always has it.
type Time int

const (
	Clock Time = iota // 11:02:11.123
	Full              // 2026-10-02 11:02:11.123 +03:00
	NoTime
)

// Look is how a pretty line shows.
type Look struct {
	Color      bool
	Zone       *time.Location
	Time       Time
	HideStream bool
}

// Echo writes the lines of one handler and of the handlers its With and
// WithGroup make, a whole line a write, so that the lines of many goroutines
// never interleave.
type Echo struct {
	mu     sync.Mutex
	out    io.Writer
	Pretty bool
	Look   Look
}

// NewEcho writes to w, a terminal only when it is a file that is one; nil
// writes nothing.
func NewEcho(w io.Writer, format Format) *Echo {
	if format == Off {
		return nil
	}
	e := &Echo{out: w, Pretty: format == Pretty, Look: Look{Zone: time.Local}}
	if file, ok := w.(*os.File); ok {
		// FORCE_COLOR says a person reads the pipe, so the default is theirs too
		e.Pretty = e.Pretty || format == FormatDefault && (term.IsTerminal(file) || term.Forced())
		e.Look.Color = e.Pretty && term.Colors(file)
	}
	return e
}

// Write prints a line; a console that refuses it loses that line, never the
// record a store keeps.
func (e *Echo) Write(line *Line) {
	var text []byte
	if e.Pretty {
		text = AppendPretty(nil, line, e.Look)
	} else {
		text = AppendJSON(nil, line)
	}
	e.mu.Lock()
	defer e.mu.Unlock()
	//nolint:errcheck // a console that refuses a line loses the line, and the record goes on to the store
	e.out.Write(text)
}

// AppendJSON writes a line as one JSON object and a newline, its keys in this
// order: time, level, stream, the event's name for a line other than a log
// line, msg, the context's fields, the attributes', the trace and span. The
// time is UTC to the millisecond, as JavaScript's Date gives it; a field keeps
// its spelling, and a key repeated stays repeated, as in slog.
func AppendJSON(dst []byte, l *Line) []byte {
	dst = append(dst, '{')
	// a record of the zero time has none, as slog's handlers write it
	if !l.At.IsZero() {
		dst = append(dst, `"time":"`...)
		dst = l.At.UTC().AppendFormat(dst, "2006-01-02T15:04:05.000Z07:00")
		dst = append(dst, `",`...)
	}
	if l.Level != nil {
		dst = append(dst, `"level":`...)
		dst = AppendString(dst, l.Level.String())
		dst = append(dst, ',')
	}
	dst = append(dst, `"stream":`...)
	dst = AppendString(dst, l.Stream)
	if l.Name != LogName {
		dst = append(dst, `,"event":`...)
		dst = AppendString(dst, l.Name)
	}
	if l.Body != nil {
		dst = append(dst, `,"msg":`...)
		dst = AppendString(dst, *l.Body)
	}
	for _, fields := range [2][]Field{l.Context, l.Attrs} {
		for _, field := range fields {
			dst = append(dst, ',')
			dst = AppendString(dst, field.Key)
			dst = append(dst, ':')
			dst = append(dst, field.Value...)
		}
	}
	if l.TraceID != ([16]byte{}) {
		dst = append(dst, `,"trace_id":"`...)
		dst = hex.AppendEncode(dst, l.TraceID[:])
		dst = append(dst, '"')
	}
	if l.SpanID != ([8]byte{}) {
		dst = append(dst, `,"span_id":"`...)
		dst = hex.AppendEncode(dst, l.SpanID[:])
		dst = append(dst, '"')
	}
	return append(dst, "}\n"...)
}

// the colours of a pretty line, the terminal's own sixteen so that its theme
// chooses the shades
const (
	reset   = "\x1b[0m"
	dim     = "\x1b[2m"
	red     = "\x1b[31m"
	green   = "\x1b[32m"
	yellow  = "\x1b[33m"
	blue    = "\x1b[34m"
	magenta = "\x1b[35m"
	cyan    = "\x1b[36m"
)

// AppendPretty writes a line as a person reads it: the time of day, the level
// padded to five, the stream, the message, then the context's fields and the
// attributes', and the trace's first eight digits. A value of several lines, a
// stack or a traceback, follows the line, indented:
//
//	11:02:11.123 ERROR api  charge failed  userId=42
//	    err: Error: card declined
//	        at charge (pay.ts:12:9)
//
// A key, and a string, shows without its quotes when it is one word with no
// '=', quote or backslash; a source shows as its file and line; any other
// value shows as its JSON. A body's further lines follow the line before such
// a value, so that none starts where a line does, and what a program logged
// shows no control character but a tab: the body, the event's name, a value
// below the line and a source each show it as its escape.
func AppendPretty(dst []byte, l *Line, look Look) []byte {
	paint := painter(look.Color)
	switch {
	case look.Time == NoTime || l.At.IsZero():
	case look.Time == Full:
		dst = paint.text(dst, dim, l.At.In(look.Zone).Format("2006-01-02 15:04:05.000 -07:00"))
		dst = append(dst, ' ')
	default:
		dst = paint.text(dst, dim, l.At.In(look.Zone).Format("15:04:05.000"))
		dst = append(dst, ' ')
	}
	label, hue := prettyLevel(l.Level)
	dst = paint.text(dst, hue, label)

	// the level is padded to five; the columns after it are two spaces apart
	separator := strings.Repeat(" ", max(0, 5-len(label))) + " "
	if !look.HideStream {
		dst = append(dst, separator...)
		dst = paint.text(dst, cyan, l.Stream)
		separator = "  "
	}
	message, further := prettyMessage(l)
	if message != "" {
		dst = append(dst, separator...)
		dst = append(dst, message...)
		separator = "  "
	}

	var below []Field
	field := func(key, text string) {
		dst = append(dst, separator...)
		separator = " "
		if !bare(key) {
			key = string(AppendString(nil, key))
		}
		dst = paint.text(dst, dim, key+"=")
		dst = append(dst, text...)
	}
	for _, fields := range [2][]Field{l.Context, l.Attrs} {
		for _, f := range fields {
			text, lines := prettyValue(f)
			if lines {
				below = append(below, Field{Key: f.Key, Value: text})
				continue
			}
			field(f.Key, text)
		}
	}
	if l.TraceID != ([16]byte{}) {
		field("trace", hex.EncodeToString(l.TraceID[:4]))
	}
	for _, part := range further {
		dst = append(dst, "\n    "...)
		dst = append(dst, escaped(part)...)
	}
	for _, f := range below {
		dst = appendBelow(dst, paint, f.Key, f.Value)
	}
	return append(dst, '\n')
}

// appendBelow writes a value of several lines under its line, the first after
// its key, each indented by four spaces
func appendBelow(dst []byte, paint painter, key, text string) []byte {
	for i, line := range linesOf(text) {
		if i == 0 {
			line = key + ": " + line
		}
		dst = append(dst, "\n    "...)
		dst = paint.text(dst, dim, escaped(line))
	}
	return dst
}

// prettyMessage is a log line's message, or an event's name and the body it
// may carry, as the line shows them; and the further lines of a body of
// several, which go below the line as a value's do
func prettyMessage(l *Line) (message string, further []string) {
	body := ""
	if l.Body != nil {
		body = *l.Body
	}
	lines := linesOf(body)
	first := escaped(lines[0])
	switch {
	case l.Name == LogName:
		return first, lines[1:]
	case first == "":
		return escaped(l.Name), lines[1:]
	}
	return escaped(l.Name) + "  " + first, lines[1:]
}

// linesOf is the lines of a text of several, without the line ends after the
// last, and each without the carriage return before its newline:
//
//	"line one\r\nline two\r\n"  →  line one, line two
func linesOf(text string) []string {
	lines := strings.Split(strings.TrimRight(text, "\r\n"), "\n")
	for i, line := range lines {
		lines[i] = strings.TrimSuffix(line, "\r")
	}
	return lines
}

// escaped is text a program logged as a pretty line shows it: each control
// character but a tab, and DEL, as its JSON escape, so that none reaches the
// terminal as an escape sequence, a carriage return or a line of its own. The
// colours the console paints with are its own, never in this text.
//
//	"\x1b[2J"  →  \u001b[2J    "a\rb"  →  a\rb    "\x7f"  →  \u007f
func escaped(text string) string {
	if !strings.ContainsFunc(text, isEscaped) {
		return text
	}
	out := make([]byte, 0, len(text)+8)
	// a control character and DEL are one byte each, never part of another character's
	for i := 0; i < len(text); i++ {
		if c := text[i]; isEscaped(rune(c)) {
			out = appendEscape(out, rune(c))
		} else {
			out = append(out, c)
		}
	}
	return string(out)
}

// isEscaped is whether a pretty line shows a character a program logged as its escape
func isEscaped(r rune) bool {
	return (r < 0x20 && r != '\t') || r == 0x7f
}

// prettyLevel spells a level as slog does, INFO, WARN+2, with its colour; a
// line without one is an event
func prettyLevel(level *slog.Level) (label, hue string) {
	switch {
	case level == nil:
		return "EVENT", magenta
	case *level < slog.LevelInfo:
		return level.String(), blue
	case *level < slog.LevelWarn:
		return level.String(), green
	case *level < slog.LevelError:
		return level.String(), yellow
	}
	return level.String(), red
}

// prettyValue is how a field shows, and whether it is text of several lines,
// which goes under the line instead
func prettyValue(f Field) (shown string, lines bool) {
	if f.Key == SourceKey {
		if place, ok := shortSource(f.Value); ok {
			return place, false
		}
	}
	if !strings.HasPrefix(f.Value, `"`) {
		return f.Value, false
	}
	var text string
	if json.Unmarshal([]byte(f.Value), &text) != nil {
		return f.Value, false
	}
	if strings.Contains(text, "\n") {
		return text, true
	}
	if bare(text) {
		return text, false
	}
	return f.Value, false
}

// SourceKey is the field where a line says where it was logged, as slog's
// handlers spell it: {"function":"main.main","file":"/app/main.go","line":42}.
const SourceKey = "source"

// shortSource is a source as a pretty line shows it, its file's directory and
// name and its line, as zap's console does:
//
//	{"function":"main.run","file":"/home/ann/app/server/main.go","line":42}  →  server/main.go:42
func shortSource(spelled string) (string, bool) {
	var source struct {
		File string `json:"file"`
		Line int    `json:"line"`
	}
	if !strings.HasPrefix(spelled, "{") || json.Unmarshal([]byte(spelled), &source) != nil || source.File == "" {
		return "", false
	}
	parts := strings.FieldsFunc(source.File, func(r rune) bool { return r == '/' || r == '\\' })
	if len(parts) > 2 {
		parts = parts[len(parts)-2:]
	}
	return escaped(strings.Join(parts, "/") + ":" + strconv.Itoa(source.Line)), true
}

// bare is a string a person reads without its quotes: not empty, and no
// character a space or below, DEL, '"', '=' or '\'
func bare(text string) bool {
	if text == "" {
		return false
	}
	for _, r := range text {
		if r <= ' ' || r == 0x7f || r == '"' || r == '=' || r == '\\' {
			return false
		}
	}
	return true
}

// painter colours text when its line goes to a terminal that shows colours
type painter bool

func (p painter) text(dst []byte, hue, text string) []byte {
	if !p {
		return append(dst, text...)
	}
	dst = append(dst, hue...)
	dst = append(dst, text...)
	return append(dst, reset...)
}
