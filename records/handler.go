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
// of the records engine itself are refused, or writing a log would log again.
//
// A line becomes a record named "log" whose body is the message. The
// attributes of logger.With are its context, who is speaking, and the call's
// are its attributes; a group's keys are written group.key, and a value is
// spelled as slog.JSONHandler spells it, an error as its message.
func (s *Store) Handler(stream string) slog.Handler {
	return &handler{store: s, stream: stream}
}

type handler struct {
	store   *Store
	stream  string
	context []Field // never changed once built: WithAttrs copies it
	prefix  string
	own     bool
}

func (h *handler) Enabled(context.Context, slog.Level) bool {
	return !h.own
}

func (h *handler) Handle(_ context.Context, line slog.Record) error {
	record, own := h.record(line)
	switch {
	case h.own || own:
		return nil
	case checkRecord(&record) != nil:
		h.store.dropped.Add(1)
		return nil //nolint:nilerr // a line the format cannot keep is dropped and counted, as the handler promises
	}
	select {
	case h.store.queue <- record:
	default:
		h.store.dropped.Add(1)
	}
	return nil
}

// record maps one line, and says whether it is the records engine's own
func (h *handler) record(line slog.Record) (Record, bool) {
	at := line.Time
	if at.IsZero() {
		at = h.store.now()
	}
	level, message := line.Level, line.Message
	record := Record{At: at, Stream: h.stream, Name: "log", Level: &level, Body: &message, Context: h.context}
	own := false
	line.Attrs(func(attr slog.Attr) bool {
		own = own || isOwn(attr)
		record.Attrs = appendAttr(record.Attrs, h.prefix, attr)
		return true
	})
	return record, own
}

func (h *handler) WithAttrs(attrs []slog.Attr) slog.Handler {
	next := *h
	next.context = slices.Clone(h.context)
	for _, attr := range attrs {
		next.context = appendAttr(next.context, h.prefix, attr)
		next.own = next.own || isOwn(attr)
	}
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
func appendAttr(fields []Field, prefix string, attr slog.Attr) []Field {
	value := attr.Value.Resolve()
	if value.Kind() == slog.KindGroup {
		if attr.Key != "" {
			prefix += attr.Key + "."
		}
		for _, inner := range value.Group() {
			fields = appendAttr(fields, prefix, inner)
		}
		return fields
	}
	if attr.Key == "" && value.Equal(slog.Value{}) {
		return fields
	}
	return append(fields, Field{Key: prefix + attr.Key, Value: spell(value)})
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

// Flush writes what the handler holds as one batch. A batch whose write fails
// is dropped and counted, as a full buffer's lines are.
func (s *Store) Flush(ctx context.Context) error {
	batch := s.drain()
	if len(batch) == 0 {
		return nil
	}

	err := s.appendChecked(ctx, batch)
	if err != nil {
		s.dropped.Add(uint64(len(batch)))
	}
	return err
}

// drain takes at most a buffer's worth, so that lines arriving meanwhile wait
// for the next flush instead of stretching this one
func (s *Store) drain() []Record {
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
