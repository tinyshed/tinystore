package records

import (
	"context"
	"log/slog"
	"slices"
)

// Handler queues records for records.db and never blocks its caller: when the
// buffer is full the record is dropped and counted in Stats. Lines logged by
// the records engine itself are refused, or writing a log would log again.
func (s *Store) Handler() slog.Handler {
	return &handler{store: s}
}

type handler struct {
	store  *Store
	attrs  []slog.Attr
	prefix string
	own    bool
}

func (h *handler) Enabled(context.Context, slog.Level) bool {
	return !h.own
}

func (h *handler) Handle(_ context.Context, r slog.Record) error {
	if h.own {
		return nil
	}
	attrs := make(map[string]any, len(h.attrs)+r.NumAttrs())
	for _, attr := range h.attrs {
		attrs[attr.Key] = jsonValue(attr.Value)
	}
	own := false
	r.Attrs(func(attr slog.Attr) bool {
		own = own || isOwn(attr)
		attrs[h.prefix+attr.Key] = jsonValue(attr.Value)
		return true
	})
	if own {
		return nil
	}

	at := r.Time
	if at.IsZero() {
		at = h.store.now()
	}
	select {
	case h.store.queue <- Record{At: at, Level: r.Level, Message: r.Message, Attrs: attrs}:
	default:
		h.store.dropped.Add(1)
	}
	return nil
}

func (h *handler) WithAttrs(attrs []slog.Attr) slog.Handler {
	next := *h
	next.attrs = slices.Clone(h.attrs)
	for _, attr := range attrs {
		next.attrs = append(next.attrs, slog.Attr{Key: h.prefix + attr.Key, Value: attr.Value})
		next.own = next.own || isOwn(attr)
	}
	return &next
}

func (h *handler) WithGroup(name string) slog.Handler {
	next := *h
	next.prefix = h.prefix + name + "."
	return &next
}

// isOwn is the attribute the store's logger gives every line of this engine
func isOwn(attr slog.Attr) bool {
	return attr.Key == "engine" && attr.Value.Resolve().String() == "records"
}

// jsonValue keeps what encoding/json can write, and an error as its message
func jsonValue(value slog.Value) any {
	resolved := value.Resolve()
	if err, ok := resolved.Any().(error); ok {
		return err.Error()
	}
	if resolved.Kind() == slog.KindGroup {
		group := map[string]any{}
		for _, attr := range resolved.Group() {
			group[attr.Key] = jsonValue(attr.Value)
		}
		return group
	}
	return resolved.Any()
}
