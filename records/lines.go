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
// a file being followed: each line it is given becomes a record in stream,
// queued as the handler queues its lines, so a Write never waits and a record
// the buffer has no room for is dropped and counted in Stats. The lines of one
// record are joined first: a stack trace's frames, a traceback, a JSON value
// printed over several lines. A record named "log" keeps its text byte for
// byte as its body; one named "json" or "logfmt" keeps a JSON object's fields
// or logfmt pairs, which spell its line again. Its level is taken from where
// pino, logfmt, glog, Redis, log4j and their kin write it, in colour or not,
// and its time is when its first line arrived. A record still growing waits a
// flush or two for a line that joins it; Close hands it over.
func (s *Store) Lines(stream string) io.WriteCloser {
	w := newLineWriter(stream, s.now, s.enqueue)
	w.release = func() { s.lines.remove(w) }
	s.lines.add(w)
	return w
}

// the names Lines gives its records, which say how each spells its line again
const (
	textLine   = "log"    // the body is the line
	jsonLine   = "json"   // the attributes are a JSON object's fields, as exactFields reads them
	logfmtLine = "logfmt" // the attributes are logfmt pairs, as spellLogfmt writes them
)

// maxJoinedLines is how many lines one record joins
const maxJoinedLines = 1000

var errLinesClosed = fmt.Errorf("records: lines: %w", tinystore.ErrClosed)

type lineWriter struct {
	stream  string
	now     func() time.Time
	enqueue func(Record) // where a record goes once its lines are joined
	release func()       // what Close lets go of
	room    int          // the most text a record of the stream can keep
	mu      sync.Mutex
	partial []byte // a line not yet ended
	pending *pendingRecord
	closed  bool
}

func newLineWriter(stream string, now func() time.Time, enqueue func(Record)) *lineWriter {
	room := maxBlockInput - inputSize(&Record{Stream: stream, Name: textLine})
	return &lineWriter{stream: stream, now: now, enqueue: enqueue, release: func() {}, room: room}
}

// pendingRecord is one record's lines so far
type pendingRecord struct {
	text   []byte
	at     time.Time
	lines  int
	room   int
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
	w.handOverAllLocked()
	w.closed = true
	w.release()
	return nil
}

// handOverAll hands over what the writer holds, as the store closes
func (w *lineWriter) handOverAll() {
	w.mu.Lock()
	defer w.mu.Unlock()
	w.handOverAllLocked()
}

func (w *lineWriter) handOverAllLocked() {
	if len(w.partial) > 0 {
		w.feed(w.partial)
		w.partial = nil
	}
	w.handOverPending()
}

// handOverIdle hands over a record that did not grow since the flush before
func (w *lineWriter) handOverIdle() {
	w.mu.Lock()
	defer w.mu.Unlock()
	switch {
	case w.pending == nil:
	case w.pending.waited:
		w.handOverPending()
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
	w.handOverPending()
	w.pending = &pendingRecord{at: w.now(), room: w.room}
	w.pending.add(line)
}

func (p *pendingRecord) add(line []byte) {
	if p.lines > 0 {
		p.text = append(p.text, '\n')
	}
	p.text = append(p.text, line...)
	p.lines++
	p.waited = false
	if p.lines == 1 && (bytes.HasPrefix(line, []byte("{")) || string(bytes.TrimSpace(line)) == "[") {
		p.open.json = true
	}
	p.open.scan(line)
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
func (p *pendingRecord) joins(line []byte) bool {
	switch {
	case p.lines >= maxJoinedLines || len(p.text)+1+len(line) > p.room:
		return false
	case p.open.depth > 0:
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

func (b *brackets) scan(line []byte) {
	if !b.json {
		return
	}
	for _, c := range line {
		switch {
		case b.escape:
			b.escape = false
		case b.inString && c == '\\':
			b.escape = true
		case c == '"':
			b.inString = !b.inString
		case b.inString:
		case c == '{' || c == '[':
			b.depth++
		case c == '}' || c == ']':
			b.depth--
		}
	}
	b.inString = b.inString && b.depth > 0
	b.json = b.depth > 0
}

// handOverPending hands over the record being joined
func (w *lineWriter) handOverPending() {
	if w.pending != nil {
		w.enqueue(w.record(w.pending))
		w.pending = nil
	}
}

// record keeps a JSON object's fields or logfmt pairs when they spell the
// line again and fit a record, and the text as it is otherwise
func (w *lineWriter) record(p *pendingRecord) Record {
	text := string(p.text)
	record := Record{At: p.at, Stream: w.stream, Name: textLine}
	if fields, ok := exactFields(text); ok {
		record.Name, record.Attrs = jsonLine, fields
	} else if fields, ok = logfmtFields(text); ok {
		record.Name, record.Attrs = logfmtLine, fields
	}
	if record.Attrs == nil || inputSize(&record) > maxBlockInput {
		record.Name, record.Attrs, record.Body = textLine, nil, &text
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

// lineWriters are the writers Lines gave out and not yet closed, which every
// flush asks for the records they hold
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
