package records

import (
	"encoding/hex"
	"encoding/json"
	"io"
	"log/slog"
	"os"
	"strings"
	"sync"
	"time"

	"github.com/tinyshed/tinystore/internal/term"
)

// A console line is what a Handler writes as a line is logged, beside the
// record it queues: pretty for a person at a terminal, one JSON object a line
// for a collector. The Bun and Python loggers write the same bytes, which
// testdata/console.json holds:
//
//	11:02:11.123 WARN  api  slow request  ms=1200
//	{"time":"2026-10-02T11:02:11.123Z","level":"WARN","stream":"api","msg":"slow request","ms":1200}

// the name of a record a logger writes for a line, as against an event's
const logName = "log"

// echo writes the lines of one Handler and of the handlers its With and
// WithGroup make, a whole line a write, so that the lines of many goroutines
// never interleave.
type echo struct {
	mu     sync.Mutex
	out    io.Writer
	pretty bool
	color  bool
	zone   *time.Location
}

// newEcho is where a handler's lines go, or nil for nowhere: a terminal
// gets them pretty, and a pipe or a file JSON, unless an option says which.
func newEcho(settings handlerSettings) *echo {
	if settings.console == ConsoleOff {
		return nil
	}
	file := os.Stderr
	if settings.stdout {
		file = os.Stdout
	}
	c := &echo{out: file, zone: time.Local}
	if settings.out != nil {
		// a test's own writer, whose lines do not depend on the machine's zone
		c.out, c.zone = settings.out, time.UTC
	}
	// FORCE_COLOR says a person reads the pipe, so the default is theirs too
	c.pretty = settings.console == ConsolePretty || settings.console == 0 && (term.IsTerminal(file) || term.Forced())
	c.color = c.pretty && settings.out == nil && term.Colors(file)
	return c
}

// Printer writes records as a Handler's console writes its lines: pretty for
// a person at a terminal, its levels in colour where the terminal shows them,
// and one JSON object a line otherwise, unless console says which. The
// tinystore command prints a store's records with one.
type Printer struct {
	echo *echo // nil prints nothing
}

// NewPrinter prints to w, which is a terminal only when it is a file that is
// one. ConsoleOff prints nothing.
func NewPrinter(w io.Writer, console Console) *Printer {
	if console == ConsoleOff {
		return &Printer{}
	}
	c := &echo{out: w, zone: time.Local, pretty: console == ConsolePretty}
	if file, ok := w.(*os.File); ok {
		c.pretty = c.pretty || console == 0 && (term.IsTerminal(file) || term.Forced())
		c.color = c.pretty && term.Colors(file)
	}
	return &Printer{echo: c}
}

// Print writes a record, a whole line a write.
func (p *Printer) Print(r Record) {
	if p.echo != nil {
		p.echo.write(&r)
	}
}

// write prints a record; a console that refuses a line loses that line, never
// the record the handler queues
func (c *echo) write(r *Record) {
	var line []byte
	if c.pretty {
		line = appendPretty(nil, r, c.color, c.zone)
	} else {
		line = appendJSONLine(nil, r)
	}
	c.mu.Lock()
	defer c.mu.Unlock()
	//nolint:errcheck // a console that refuses a line loses the line, and the record goes on to the store
	c.out.Write(line)
}

// appendJSONLine writes a record as one JSON object and a newline, its keys in
// this order: time, level, stream, the event's name for a record other than a
// log line, msg, the context's fields, the attributes', the trace and span.
// The time is UTC to the millisecond, as JavaScript's Date gives it; a field
// keeps the record's spelling, and a key repeated stays repeated, as in slog.
func appendJSONLine(dst []byte, r *Record) []byte {
	dst = append(dst, `{"time":"`...)
	dst = r.At.UTC().AppendFormat(dst, "2006-01-02T15:04:05.000Z07:00")
	dst = append(dst, '"')
	if r.Level != nil {
		dst = append(dst, `,"level":`...)
		dst = appendJSONString(dst, r.Level.String())
	}
	dst = append(dst, `,"stream":`...)
	dst = appendJSONString(dst, r.Stream)
	if r.Name != logName {
		dst = append(dst, `,"event":`...)
		dst = appendJSONString(dst, r.Name)
	}
	if r.Body != nil {
		dst = append(dst, `,"msg":`...)
		dst = appendJSONString(dst, *r.Body)
	}
	for _, fields := range [2][]Field{r.Context, r.Attrs} {
		for _, field := range fields {
			dst = append(dst, ',')
			dst = appendJSONString(dst, field.Key)
			dst = append(dst, ':')
			dst = append(dst, field.Value...)
		}
	}
	if r.TraceID != (TraceID{}) {
		dst = append(dst, `,"trace_id":"`...)
		dst = hex.AppendEncode(dst, r.TraceID[:])
		dst = append(dst, '"')
	}
	if r.SpanID != (SpanID{}) {
		dst = append(dst, `,"span_id":"`...)
		dst = hex.AppendEncode(dst, r.SpanID[:])
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

// appendPretty writes a record as a person reads it: the time of day, the
// level padded to five, the stream, the message, then the context's fields
// and the attributes', and the trace's first eight digits. A value of several
// lines, a stack or a traceback, follows the line, indented:
//
//	11:02:11.123 ERROR api  charge failed  userId=42
//	    err: Error: card declined
//	        at charge (pay.ts:12:9)
//
// A key, and a string, shows without its quotes when it is one word with no
// '=', quote or backslash; any other value shows as its JSON.
func appendPretty(dst []byte, r *Record, color bool, zone *time.Location) []byte {
	paint := painter(color)
	dst = paint.text(dst, dim, r.At.In(zone).Format("15:04:05.000"))
	label, hue := prettyLevel(r.Level)
	dst = append(dst, ' ')
	dst = paint.text(dst, hue, label)
	dst = append(dst, strings.Repeat(" ", max(0, 5-len(label)))...)
	dst = append(dst, ' ')
	dst = paint.text(dst, cyan, r.Stream)
	if message := prettyMessage(r); message != "" {
		dst = append(dst, "  "...)
		dst = append(dst, message...)
	}

	var below []Field
	separator := "  "
	field := func(key, text string) {
		dst = append(dst, separator...)
		separator = " "
		if !bare(key) {
			key = string(appendJSONString(nil, key))
		}
		dst = paint.text(dst, dim, key+"=")
		dst = append(dst, text...)
	}
	for _, fields := range [2][]Field{r.Context, r.Attrs} {
		for _, f := range fields {
			text, lines := prettyValue(f.Value)
			if lines {
				below = append(below, Field{Key: f.Key, Value: text})
				continue
			}
			field(f.Key, text)
		}
	}
	if r.TraceID != (TraceID{}) {
		field("trace", hex.EncodeToString(r.TraceID[:4]))
	}
	for _, f := range below {
		dst = appendBelow(dst, paint, f.Key, f.Value)
	}
	return append(dst, '\n')
}

// appendBelow writes a value of several lines under its line, the first after
// its key, each indented by four spaces
func appendBelow(dst []byte, paint painter, key, text string) []byte {
	lines := strings.Split(strings.TrimRight(text, "\r\n"), "\n")
	for i, line := range lines {
		line = strings.TrimSuffix(line, "\r")
		if i == 0 {
			line = key + ": " + line
		}
		dst = append(dst, "\n    "...)
		dst = paint.text(dst, dim, line)
	}
	return dst
}

// prettyMessage is a log line's message, or an event's name and the body it
// may carry
func prettyMessage(r *Record) string {
	body := ""
	if r.Body != nil {
		body = *r.Body
	}
	switch {
	case r.Name == logName:
		return body
	case body == "":
		return r.Name
	}
	return r.Name + "  " + body
}

// prettyLevel spells a level as slog does, INFO, WARN+2, with its colour; a
// record without one is an event
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

// prettyValue is how a field's JSON shows, and whether it is text of several
// lines, which goes under the line instead
func prettyValue(spelled string) (shown string, lines bool) {
	if !strings.HasPrefix(spelled, `"`) {
		return spelled, false
	}
	var text string
	if json.Unmarshal([]byte(spelled), &text) != nil {
		return spelled, false
	}
	if strings.Contains(text, "\n") {
		return text, true
	}
	if bare(text) {
		return text, false
	}
	return spelled, false
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
