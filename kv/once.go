package kv

import (
	"context"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/internal/core"
	"github.com/tinyshed/tinystore/internal/wire"
)

// Once runs a function once a key and keeps its answer: a request sent twice
// is answered as the first was rather than done twice. The core lets one run
// of a key go at a time, every client of the store included: it answers the
// kept answer, or hands the run to this client and keeps what it sends back.
type Once[T any] struct {
	parts
	values core.Values[T]
}

// OpenOnce opens answers of type T kept once a key, a day unless Keep says.
//
//	charges, err := kv.OpenOnce[Receipt](store, "charges", kv.Keep(24*time.Hour))
func OpenOnce[T any](home tinystore.Home, name string, opts ...OnceOption) (*Once[T], error) {
	var o onceOptions
	for _, opt := range opts {
		opt.once(&o)
	}
	if err := core.CheckName(name, "once"); err != nil {
		return nil, err
	}
	keepMs, err := millis(o.keep, "kv once "+name+": keep")
	if err != nil {
		return nil, err
	}
	of := homeOf(home)
	open := &core.Opener{Method: wire.MethodKvOnceOpen, Body: wire.Encode(&wire.KvOnceOpen{Name: name, Keep: keepMs})}
	return &Once[T]{parts{store: of.Store, name: name, opener: open, via: core.ViaLink{Link: of.Store.Link}}, core.JSONValues[T]()}, nil
}

// Under is the answers of a branch, each key apart from the same key elsewhere.
func (o *Once[T]) Under(owners ...any) *Once[T] { return &Once[T]{o.below(owners), o.values} }

// Run is the answer kept for key, or fn's, which is kept. A run of a key
// that another runs, here or in another process, waits for it and gets its
// answer; an error of fn keeps nothing and is Run's, so the next run runs fn
// again. A connection lost after fn returned and before its answer was kept
// is OutcomeUnknown.
func (o *Once[T]) Run(ctx context.Context, key string, fn func(ctx context.Context) (T, error)) (T, error) {
	var zero T
	stream, kept, err := o.claim(ctx, key)
	if err != nil || stream == nil {
		return kept, err
	}
	answer, err := fn(ctx)
	if err != nil {
		// fn's own error is the one to give
		_ = end(ctx, stream, wire.Encode(&wire.KvAnswer{}))
		return zero, err
	}
	raw, err := o.values.Encode(answer)
	if err != nil {
		_ = end(ctx, stream, wire.Encode(&wire.KvAnswer{}))
		return zero, err
	}
	if err := end(ctx, stream, wire.Encode(&wire.KvAnswer{Found: true, Value: &raw})); err != nil {
		return zero, err
	}
	return answer, nil
}

// claim asks for key's run: the answer kept for it, or the run the core
// handed over, which a connection lost before the answer asks for again.
func (o *Once[T]) claim(ctx context.Context, key string) (*core.Stream, T, error) {
	var stream *core.Stream
	var kept T
	err := o.store.Link.Run(ctx, core.Reading, func(c *core.Connection) error {
		handle, err := core.HandleOn(ctx, c, o.opener)
		if err != nil {
			return err
		}
		st, err := c.Session.Open(ctx, wire.MethodKvOnceRun, wire.Encode(&wire.KvCall{Handle: handle, Under: o.under, Key: key}), false)
		if err != nil {
			return err
		}
		first, err := st.Next(ctx)
		if err != nil {
			st.Cancel()
			return err
		}
		if !first.End {
			stream = st
			return nil
		}
		var offered wire.KvAnswer
		if err := decode(first.Body, &offered); err != nil {
			return err
		}
		if !offered.Found {
			return core.Errorf(core.Protocol, "kv once %s: a run ended without its answer", o.name)
		}
		var raw wire.Raw
		if offered.Value != nil {
			raw = *offered.Value
		}
		kept, err = o.values.Decode(raw)
		return err
	})
	return stream, kept, err
}

// end sends the run's answer as the stream's last DATA, and waits for the
// core's, once it is kept.
func end(ctx context.Context, stream *core.Stream, answer []byte) error {
	err := stream.Send(ctx, answer, true)
	if err == nil {
		_, err = stream.Next(ctx)
	}
	if lost, _ := core.IsConnectionLost(err); lost {
		return core.Wrap(core.OutcomeUnknown, err, "the connection was lost before the answer was kept; the next run may run again")
	}
	return err
}

// Get is the answer kept for key, and whether one is.
func (o *Once[T]) Get(ctx context.Context, key string) (T, bool, error) {
	var zero T
	answer, err := o.call(ctx, wire.MethodKvOnceGet, wire.KvCall{Key: key}, core.Reading)
	if err != nil {
		return zero, false, err
	}
	var kept wire.KvAnswer
	if err := decode(answer, &kept); err != nil || !kept.Found {
		return zero, false, err
	}
	var raw wire.Raw
	if kept.Value != nil {
		raw = *kept.Value
	}
	value, err := o.values.Decode(raw)
	return value, err == nil, err
}

// Delete forgets the answer kept for key, so that its next run runs fn again.
func (o *Once[T]) Delete(ctx context.Context, key string) (bool, error) {
	return foundOf(o.call(ctx, wire.MethodKvOnceDelete, wire.KvCall{Key: key}, core.Writing))
}
