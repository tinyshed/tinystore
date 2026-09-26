package records

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"strings"
	"sync"
	"time"

	"github.com/tinyshed/tinystore"
)

// Lines is a writer for another program's output, a child process's stdout or
// a file being followed: each line it is given becomes a record of stream
// named "log", queued as the handler queues its lines, so a Write never waits
// and a record the buffer has no room for is dropped and counted in Stats.
// The lines of one record are joined first: a stack trace's frames, a
// traceback, a JSON value printed over several lines. A record keeps its text
// byte for byte, or a JSON object's fields when they spell the line again,
// and takes its level from where pino, logfmt, glog, log4j and their kin
// write it; its time is when its first line arrived. A record still growing
// waits a flush or two for a line that joins it; Close hands it over.
func (s *Store) Lines(stream string) io.WriteCloser {
	w := &lineWriter{stream: stream, now: s.now, hand: s.queue1}
	w.release = func() { s.lines.remove(w) }
	s.lines.add(w)
	return w
}

// maxJoinedLines is how many lines one record joins
const maxJoinedLines = 1000

var errLinesClosed = fmt.Errorf("records: lines: %w", tinystore.ErrClosed)

type lineWriter struct {
	stream  string
	now     func() time.Time
	hand    func(Record) // where a record goes once its lines are joined
	release func()       // what Close lets go of
	mu      sync.Mutex
	partial []byte // a line not yet ended
	pending *joining
	closed  bool
}

// joining is one record's lines so far
type joining struct {
	text   []byte
	at     time.Time
	lines  int
	open   brackets // what a JSON value begun on its first line left open
	waited bool     // a flush found it, and it has not grown since
}

func (w *lineWriter) Write(p []byte) (int, error) {
	w.mu.Lock()
	defer w.mu.Unlock()
	if w.closed {
		return 0, errLinesClosed
	}
	w.partial = append(w.partial, p...)
	for {
		end := bytes.IndexByte(w.partial, '\n')
		if end < 0 {
			break
		}
		w.feed(w.partial[:end])
		w.partial = w.partial[end+1:]
	}
	if len(w.partial) >= maxBlockInput {
		w.feed(w.partial)
		w.partial = w.partial[:0]
	}
	w.partial = append([]byte(nil), w.partial...)
	return len(p), nil
}

// Close hands over the record it holds, and a last line no newline ended
func (w *lineWriter) Close() error {
	w.mu.Lock()
	defer w.mu.Unlock()
	if w.closed {
		return nil
	}
	w.handOverAll()
	w.closed = true
	w.release()
	return nil
}

// handOverEverything hands over what the writer holds, as the store closes
func (w *lineWriter) handOverEverything() {
	w.mu.Lock()
	defer w.mu.Unlock()
	w.handOverAll()
}

func (w *lineWriter) handOverAll() {
	if len(w.partial) > 0 {
		w.feed(w.partial)
		w.partial = nil
	}
	w.emit()
}

// handOverIdle hands over a record that did not grow since the flush before
func (w *lineWriter) handOverIdle() {
	w.mu.Lock()
	defer w.mu.Unlock()
	switch {
	case w.pending == nil:
	case w.pending.waited:
		w.emit()
	default:
		w.pending.waited = true
	}
}

// feed joins a line to the record before it, or hands that record over and
// begins the next with it
func (w *lineWriter) feed(line []byte) {
	if w.pending != nil && w.pending.joins(line) {
		w.pending.add(line)
		return
	}
	w.emit()
	w.pending = &joining{at: w.now()}
	w.pending.add(line)
}

func (j *joining) add(line []byte) {
	if j.lines > 0 {
		j.text = append(j.text, '\n')
	}
	j.text = append(j.text, line...)
	j.lines++
	j.waited = false
	if j.lines == 1 && (bytes.HasPrefix(line, []byte("{")) || string(bytes.TrimSpace(line)) == "[") {
		j.open.json = true
	}
	j.open.scan(line)
}

// joins says whether a line belongs to the record before it: an indented
// line, the lines that open a traceback or name an exception or its cause, and
// every line of a JSON value its first line left open
//
//	2026-09-26 12:00:01,500 ERROR request failed   a record
//	Traceback (most recent call last):             joins: a traceback
//	  File "app.py", line 3, in handle             joins: indented
//	ValueError: bad input                          joins: names an exception
//	2026-09-26 12:00:02,000 INFO next              a record
//	{                                              a record, a JSON value begun on a line of its own
//	"status": 200                                  joins: the value is open
//	}                                              joins, and closes it
//	[2026-09-26 12:00:03] info done                a record: brackets begin no value but a line's own
func (j *joining) joins(line []byte) bool {
	switch {
	case j.lines >= maxJoinedLines || len(j.text)+1+len(line) > maxBlockInput-maxJoinedLines:
		return false
	case j.open.depth > 0:
		return true
	case len(line) > 0 && (line[0] == ' ' || line[0] == '\t'):
		return true
	}
	return continues(line)
}

// the beginnings of a line that go on what the line before began
var continuations = []string{"Caused by: ", "Traceback (most recent call last):", "goroutine "}

// continues is a line a stack trace goes on with: a cause, a traceback, a
// goroutine, or the name of an exception, alone or before a colon
//
//	ValueError: bad input    java.io.IOException: disk    KeyError
func continues(line []byte) bool {
	for _, prefix := range continuations {
		if bytes.HasPrefix(line, []byte(prefix)) {
			return true
		}
	}
	name := line
	if end := bytes.IndexByte(line, ':'); end >= 0 {
		name = line[:end]
	}
	for _, c := range name {
		if !isLetter(c) && !isDigit(c) && c != '.' && c != '_' && c != '$' {
			return false
		}
	}
	for _, suffix := range []string{"Error", "Exception", "Warning", "Throwable"} {
		if bytes.HasSuffix(name, []byte(suffix)) {
			return true
		}
	}
	return false
}

// brackets follows a JSON value over its lines
type brackets struct {
	json             bool // the first line began a value
	depth            int
	inString, escape bool
}

func (j *brackets) scan(line []byte) {
	if !j.json {
		return
	}
	for _, c := range line {
		switch {
		case j.escape:
			j.escape = false
		case j.inString && c == '\\':
			j.escape = true
		case c == '"':
			j.inString = !j.inString
		case j.inString:
		case c == '{' || c == '[':
			j.depth++
		case c == '}' || c == ']':
			j.depth--
		}
	}
	j.inString = j.inString && j.depth > 0
	j.json = j.depth > 0
}

// emit hands over the record being joined
func (w *lineWriter) emit() {
	if w.pending != nil {
		w.hand(w.record(w.pending))
		w.pending = nil
	}
}

func (w *lineWriter) record(j *joining) Record {
	text := string(j.text)
	record := Record{At: j.at, Stream: w.stream, Name: "log"}
	if fields, ok := exactFields(text); ok {
		record.Attrs = fields
	} else {
		record.Body = &text
	}
	if level, ok := levelOf(text, record.Attrs); ok {
		record.Level = &level
	}
	return record
}

// exactFields is a JSON object's fields when, written one after another
// without spaces, they spell the text again: nothing of the line is lost
//
//	{"level":30,"msg":"ok"}    → level 30, msg "ok"
//	{ "level": 30 }            → stays text: its spaces would be lost
func exactFields(text string) ([]Field, bool) {
	if len(text) < 2 || text[0] != '{' || text[len(text)-1] != '}' {
		return nil, false
	}
	fields, err := objectFields(text)
	if err != nil || len(fields) == 0 || len(fields) > maxFields {
		return nil, false
	}
	var spelled strings.Builder
	spelled.WriteByte('{')
	for i, field := range fields {
		if i > 0 {
			spelled.WriteByte(',')
		}
		spelled.Write(appendJSONKey(nil, field.Key))
		spelled.WriteByte(':')
		spelled.WriteString(field.Value)
	}
	spelled.WriteByte('}')
	return fields, spelled.String() == text
}

// objectFields reads a JSON object's fields in order, each value as it is spelled
func objectFields(text string) ([]Field, error) {
	decoder := json.NewDecoder(strings.NewReader(text))
	if opening, err := decoder.Token(); err != nil || opening != json.Delim('{') {
		return nil, errors.New("not an object")
	}
	var fields []Field
	for decoder.More() {
		key, err := decoder.Token()
		if err != nil {
			return nil, err
		}
		name, ok := key.(string)
		if !ok {
			return nil, errors.New("a key that is not a string")
		}
		var value json.RawMessage
		if err = decoder.Decode(&value); err != nil {
			return nil, err
		}
		fields = append(fields, Field{Key: name, Value: string(value)})
	}
	if _, err := decoder.Token(); err != nil {
		return nil, err
	}
	if err := decoder.Decode(new(any)); !errors.Is(err, io.EOF) {
		return nil, errors.New("trailing data")
	}
	return fields, nil
}

// lineWriters are the writers Lines gave out and not yet closed; a flush
// hands over what they hold once it has waited a flush
type lineWriters struct {
	mu      sync.Mutex
	writers map[*lineWriter]struct{}
}

func (l *lineWriters) add(w *lineWriter) {
	l.mu.Lock()
	defer l.mu.Unlock()
	if l.writers == nil {
		l.writers = map[*lineWriter]struct{}{}
	}
	l.writers[w] = struct{}{}
}

func (l *lineWriters) remove(w *lineWriter) {
	l.mu.Lock()
	defer l.mu.Unlock()
	delete(l.writers, w)
}

func (l *lineWriters) each(do func(*lineWriter)) {
	l.mu.Lock()
	writers := make([]*lineWriter, 0, len(l.writers))
	for w := range l.writers {
		writers = append(writers, w)
	}
	l.mu.Unlock()
	for _, w := range writers {
		do(w)
	}
}

func appendJSONKey(out []byte, key string) []byte {
	encoded, err := json.Marshal(key)
	if err != nil {
		return appendJSONString(out, key)
	}
	return append(out, encoded...)
}
