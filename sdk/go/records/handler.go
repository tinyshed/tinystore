package records

import (
	"context"
	"log/slog"
	"sync"
	"time"

	"tinyshed.org/tinystore"
	"tinyshed.org/tinystore/console"
	"tinyshed.org/tinystore/internal/core"
	"tinyshed.org/tinystore/internal/logline"
	"tinyshed.org/tinystore/internal/wire"
)

const (
	// flushEvery is how long a line waits for the store at most, unless half
	// the buffer fills first.
	flushEvery = time.Second
	// quiet is how long lines dropped after the last said are counted rather
	// than said again.
	quiet = 10 * time.Minute
)

// Handler is a slog.Handler of a store's: each line printed as
// console.NewHandler prints it, and kept in its stream as an event named log,
// its message the line's, the call's attributes its fields and those of
// logger.With its context. A call never waits for the store: lines wait in a
// bounded buffer and go every second, or once half of it waits, and what does
// not fit, or a write that fails, is dropped and counted, and said in
// TinyStore's own lines, since no call waits to be told. The store's close
// hands over what it holds. It is a slog.Handler as any other: slog.SetDefault
// takes it, and with console.Off it sits beside a program's own handler.
//
//	handler := records.NewHandler(store, "api", console.Redact(console.Secrets...))
//	log := slog.New(handler)
//	log.Info("signed in", "user", 42)
//	handler.Event("signup", "plan", "pro")
type Handler struct {
	inner  *logline.Handler
	keeper *keeper
}

// NewHandler is a store's handler of a stream, with console's options and
// Keep, Buffer and Levels. Its level, the least it keeps, and prints unless
// Levels gives a Console part, follows the config log, which it opens in the
// background: a change of it reaches every handler of the store, in every
// process, until it is reset. A stream's name the store refuses panics, as a pattern
// regexp.MustCompile refuses does: it is the program's, not its input's.
func NewHandler(store *tinystore.Store, stream string, options ...console.Option) *Handler {
	told := logline.Read(options)
	events, err := openEvents(store.CoreHome().Store, stream, told.Keep)
	if err != nil {
		panic(err)
	}
	most := told.Buffer
	if most <= 0 {
		most = 1024
	}
	k := &keeper{events: events, most: most, failures: core.FailureLog{Work: "records flush " + stream}}
	events.store.OnClose(core.StopLoggers, k.stop)
	level := leveled(told)
	settings := told.Settings
	settings.Level = level
	followLog(events.store, told, level)
	return &Handler{inner: logline.NewHandler(stream, k, settings, told.Ignored), keeper: k}
}

type buffer int

// Buffer is the lines a store's handler holds for the store, 1024 unless
// given: past it a line is dropped and counted, never waited for.
func Buffer(lines int) console.Option { return buffer(lines) }

func (b buffer) Apply(s *logline.Settings) { s.Buffer = int(b) }

// Levels are the least levels a store's handler keeps and prints, each part
// left out as the option is: a nil Store keeps every level, and a nil Console
// prints what is kept, following the config log's changes of it.
// console.Level(level) is Levels{Store: level}, and either replaces the other.
//
//	records.Levels{Store: slog.LevelInfo, Console: slog.LevelWarn}  keeps info+, prints warn+
//	records.Levels{Console: slog.LevelWarn}                         keeps every level, prints warn+
type Levels struct {
	Store   slog.Leveler // the least level kept; LOG_LEVEL and the config log win over it
	Console slog.Leveler // the least level printed; LOG_CONSOLE_LEVEL wins over it
}

func (l Levels) Apply(s *logline.Settings) { s.Level, s.Printed = l.Store, l.Console }

func (h *Handler) Enabled(ctx context.Context, level slog.Level) bool {
	return h.inner.Enabled(ctx, level)
}

func (h *Handler) Handle(ctx context.Context, record slog.Record) error {
	return h.inner.Handle(ctx, record)
}

func (h *Handler) WithAttrs(attrs []slog.Attr) slog.Handler { return h.inner.WithAttrs(attrs) }

func (h *Handler) WithGroup(name string) slog.Handler { return h.inner.WithGroup(name) }

// Event writes an event of the stream: named for itself, with no level and
// no message, its fields slog's key and value pairs, at once and dropped
// under load as a line is. An event that must not be lost is Append's.
func (h *Handler) Event(name string, args ...any) {
	h.inner.Event(context.Background(), name, args...)
}

// EventContext is Event in ctx, whose trace it takes.
func (h *Handler) EventContext(ctx context.Context, name string, args ...any) {
	h.inner.Event(ctx, name, args...)
}

// Flush hands what the buffer holds to the store now, as the timer does
// every second; what a write that failed held is dropped and counted.
func (h *Handler) Flush(ctx context.Context) error { return h.keeper.flush(ctx) }

// Dropped is the lines dropped since the handler began: a full buffer, or a
// write that failed.
func (h *Handler) Dropped() int {
	h.keeper.mu.Lock()
	defer h.keeper.mu.Unlock()
	return h.keeper.dropped
}

// keeper holds a handler's lines for the store: the handler's and those of
// the handlers its With and WithGroup make.
type keeper struct {
	events   *Events
	most     int
	failures core.FailureLog

	mu      sync.Mutex
	waiting []logline.Line
	// asked says a write was asked for and has not taken the lines yet
	asked   bool
	timer   *time.Timer
	dropped int
	unsaid  int
	saidAt  time.Time
	stopped bool

	// writing is held by the write that runs, so that a timer's and a
	// Flush's never interleave their batches
	writing sync.Mutex
}

// Keep holds a line for the store, or drops and counts it when the buffer
// holds no room; once half of it waits, a write begins.
func (k *keeper) Keep(line logline.Line) {
	k.mu.Lock()
	defer k.mu.Unlock()
	if len(k.waiting) >= k.most || k.stopped {
		k.dropped++
		k.unsaid++
		return
	}
	k.waiting = append(k.waiting, line)
	switch {
	case len(k.waiting) >= k.most/2 && !k.asked:
		k.asked = true
		go func() { _ = k.flush(context.Background()) }()
	case k.timer == nil:
		k.timer = time.AfterFunc(flushEvery, func() { _ = k.flush(context.Background()) })
	}
}

// flush writes what the buffer holds, and again while half of it filled
// during the write before: a full buffer drops a line before the line could
// ask for a write, so a burst that filled it while a write ran would
// otherwise wait for the timer's next second, every line until then dropped.
func (k *keeper) flush(ctx context.Context) error {
	k.sayDropped()
	k.writing.Lock()
	defer k.writing.Unlock()
	var failed error
	for {
		k.mu.Lock()
		batch := k.waiting
		k.waiting, k.asked = nil, false
		if k.timer != nil {
			k.timer.Stop()
			k.timer = nil
		}
		k.mu.Unlock()
		if len(batch) == 0 {
			return failed
		}
		for _, piece := range pieces(batch) {
			if err := k.write(ctx, piece); err != nil {
				failed = err
				k.failures.Failed(err)
				k.mu.Lock()
				k.dropped += len(piece)
				k.mu.Unlock()
			} else {
				k.failures.Succeeded()
			}
		}
		k.mu.Lock()
		more := len(k.waiting) >= k.most/2
		// what came while the write ran waits for the next second
		if !more && len(k.waiting) > 0 && k.timer == nil && !k.stopped {
			k.timer = time.AfterFunc(flushEvery, func() { _ = k.flush(context.Background()) })
		}
		k.mu.Unlock()
		if !more {
			return failed
		}
	}
}

// piece is the bytes of text a write carries at most. A request's body holds
// 2 MiB, and a buffer of long lines, or one made larger, held more: the whole
// write was refused and every line in it dropped.
const piece = 1 << 20

// pieces is the batch in writes of at most piece bytes of text each, in order.
func pieces(batch []logline.Line) [][]logline.Line {
	var out [][]logline.Line
	start, size := 0, 0
	for i, line := range batch {
		weight := 64 + len(line.Name)
		if line.Body != nil {
			weight += len(*line.Body)
		}
		for _, field := range line.Attrs {
			weight += len(field.Key) + len(field.Value)
		}
		for _, field := range line.Context {
			weight += len(field.Key) + len(field.Value)
		}
		if i > start && size+weight > piece {
			out = append(out, batch[start:i])
			start, size = i, 0
		}
		size += weight
	}
	return append(out, batch[start:])
}

// write appends a batch of lines, each an event of the stream.
func (k *keeper) write(ctx context.Context, batch []logline.Line) error {
	message := wire.RecordsAppend{Events: make([]wire.RecordsNewEvent, len(batch))}
	for i, line := range batch {
		event := wire.RecordsNewEvent{Name: line.Name, Message: line.Body}
		if !line.At.IsZero() {
			nanos := line.At.UnixNano()
			event.At = &nanos
		}
		if line.Level != nil {
			level := int64(*line.Level)
			event.Level = &level
		}
		event.Fields, event.Context = objectOf(line.Attrs), objectOf(line.Context)
		event.TraceId, event.SpanId = wireTrace(line.TraceID, line.SpanID)
		message.Events[i] = event
	}
	return k.events.appendWire(ctx, message)
}

// objectOf is a line's fields as the JSON object they spell, none for no field.
func objectOf(fields []logline.Field) *string {
	if len(fields) == 0 {
		return nil
	}
	out := []byte{'{'}
	for i, field := range fields {
		if i > 0 {
			out = append(out, ',')
		}
		out = logline.AppendString(out, field.Key)
		out = append(out, ':')
		out = append(out, field.Value...)
	}
	spelled := string(append(out, '}'))
	return &spelled
}

// sayDropped says the lines dropped for want of room, at most once in a
// quiet span: a burst is one line.
func (k *keeper) sayDropped() {
	k.mu.Lock()
	if k.unsaid == 0 || !k.saidAt.IsZero() && time.Since(k.saidAt) < quiet {
		k.mu.Unlock()
		return
	}
	dropped := k.unsaid
	k.unsaid, k.saidAt = 0, time.Now()
	k.mu.Unlock()
	core.SayOwn("WARN", "log lines dropped", "logger", k.events.name, "dropped", dropped, "buffer", k.most)
}

// stop hands over what the handler holds, as the store closes, and takes no more.
func (k *keeper) stop() {
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	_ = k.flush(ctx)
	k.mu.Lock()
	k.stopped = true
	if k.timer != nil {
		k.timer.Stop()
		k.timer = nil
	}
	k.mu.Unlock()
}
