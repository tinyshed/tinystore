package records

import (
	"context"
	"log/slog"
	"time"
	"unsafe"

	"github.com/tinyshed/tinystore/records/console"
	"github.com/tinyshed/tinystore/records/internal/logline"
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
// Each line is also written to standard error as it is logged, as
// console.Handler writes it, and the options are console's: the format, the
// level, the fields to redact, the environment's names. The engine's own lines
// reach the console from Info up, and never the store.
//
//	slog.New(logs.Handler("api", console.JSON, console.Redact(console.Secrets...)))
func (s *Store) Handler(stream string, options ...console.Option) slog.Handler {
	return logline.NewHandler(stream, keeper{s}, options)
}

// keeper hands a handler's lines to the store's queue
type keeper struct{ s *Store }

func (k keeper) Keep(line logline.Line) {
	k.s.enqueue(Record{
		At: line.At, Stream: line.Stream, Name: line.Name, Level: line.Level, Body: line.Body,
		TraceID: line.TraceID, SpanID: line.SpanID, Context: fieldsOf(line.Context), Attrs: fieldsOf(line.Attrs),
	})
}

func (k keeper) Now() time.Time { return k.s.now() }

// fieldsOf is a line's fields as a record's: the two types are one layout,
// which TestALinesFieldIsARecordsField keeps, so the slice is shared, as the
// handler shares its context with every line it logs
func fieldsOf(line []logline.Field) []Field {
	//nolint:gosec // one layout, which TestALinesFieldIsARecordsField keeps
	return unsafe.Slice((*Field)(unsafe.Pointer(unsafe.SliceData(line))), len(line))
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

// fits is a line the format can keep, at a time the store's window accepts
func (s *Store) fits(r *Record) bool {
	return checkRecord(r) == nil && s.window(s.now()).check(r.At, r.Stream) == nil
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
	s.sayDrops(s.now())
	return err
}

// a full buffer's drops within this long of the last said are counted, not said
const quietDrops = 10 * time.Minute

// sayDrops says how many lines a full buffer dropped since it last said so, at
// most once in a quiet period, as the Bun and Python loggers say theirs: a
// burst is one line, and a handler that keeps dropping says how many every ten
// minutes rather than every second. A failed write is background work failing,
// which the store's failure log says.
func (s *Store) sayDrops(now time.Time) {
	full := s.droppedFull.Load()
	if full == s.dropsSaid || !s.dropsSaidAt.IsZero() && now.Sub(s.dropsSaidAt) < quietDrops {
		return
	}
	s.log.Warn("log lines dropped", "dropped", full-s.dropsSaid, "buffer", cap(s.queue))
	s.dropsSaid, s.dropsSaidAt = full, now
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
