package kv

import (
	"context"
	"time"

	"tinyshed.org/tinystore"
	"tinyshed.org/tinystore/internal/core"
	"tinyshed.org/tinystore/internal/wire"
)

// Once runs a function once a key and keeps its answer: a request sent twice
// is answered as the first was rather than done twice. The core lets one run
// of a key go at a time, every client of the store included: it answers the
// kept answer, or hands the run to this client and keeps what it sends back.
// Opened with Fresh, a kept answer older than it is answered at once, and the
// core may hand this client the build of the next one, which runs in the
// background.
type Once[T any] struct {
	parts
	// aged opens the handle that Fetch asks through, which reads each
	// answer's age: Run without Fresh opens as an older client did, and so
	// still reaches an older server.
	aged   *core.Opener
	values core.Values[T]
}

// Fetched is an answer with how old it is: Stale once it is older than the
// Fresh its answers opened with.
type Fetched[T any] struct {
	Value T
	Age   time.Duration
	Stale bool
}

// OpenOnce opens answers of type T kept once a key, a day unless Keep says.
//
//	charges, err := kv.OpenOnce[Receipt](store, "charges", kv.Keep(24*time.Hour))
//	books, err := kv.OpenOnce[GradeBook](store, "cabinet-books", kv.Fresh(10*time.Minute), kv.Keep(12*time.Hour))
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
	freshMs, err := millis(o.fresh, "kv once "+name+": fresh")
	if err != nil {
		return nil, err
	}
	of := homeOf(home)
	// Only this client, which reads Refresh, sends Fresh: the core hands a
	// build in the background only to a handle opened with it, so an older
	// client never reads one as a run of its own.
	aged := &core.Opener{Method: wire.MethodKvOnceOpen, Body: wire.Encode(&wire.KvOnceOpen{Name: name, Keep: keepMs, Fresh: freshMs, Aged: true})}
	plain := aged
	if freshMs == nil {
		plain = &core.Opener{Method: wire.MethodKvOnceOpen, Body: wire.Encode(&wire.KvOnceOpen{Name: name, Keep: keepMs})}
	}
	opened := parts{store: of.Store, name: name, opener: plain, via: core.ViaLink{Link: of.Store.Link}}
	return &Once[T]{opened, aged, core.JSONValues[T]()}, nil
}

// Under is the answers of a branch, each key apart from the same key elsewhere.
func (o *Once[T]) Under(owners ...any) *Once[T] { return &Once[T]{o.below(owners), o.aged, o.values} }

// Run is the answer kept for key, or fn's, which is kept. A run of a key
// that another runs, here or in another process, waits for it and gets its
// answer; an error of fn keeps nothing and is Run's, so the next run runs fn
// again. A connection lost after fn returned and before its answer was kept
// is OutcomeUnknown.
func (o *Once[T]) Run(ctx context.Context, key string, fn func(ctx context.Context) (T, error)) (T, error) {
	answer, err := o.ask(ctx, o.opener, key, fn)
	return answer.Value, err
}

// Fetch is Run's answer with how old it is. Opened with Fresh, a kept answer
// older than it comes back at once, Stale, and fn may make the next one in
// the background, on a context that the caller's cancel does not reach: its
// answer replaces the kept one, and its error, which no caller sees, keeps it
// until Keep ends.
//
//	book, err := books.Fetch(ctx, userID, func(ctx context.Context) (GradeBook, error) { return cabinet.Book(ctx, userID) })
func (o *Once[T]) Fetch(ctx context.Context, key string, fn func(ctx context.Context) (T, error)) (Fetched[T], error) {
	return o.ask(ctx, o.aged, key, fn)
}

func (o *Once[T]) ask(ctx context.Context, opener *core.Opener, key string, fn func(ctx context.Context) (T, error)) (Fetched[T], error) {
	stream, kept, err := o.claim(ctx, opener, key)
	if err != nil || stream == nil {
		return kept, err
	}
	if kept.Stale {
		go o.build(context.WithoutCancel(ctx), stream, fn)
		return kept, nil
	}
	made, answer, err := o.made(ctx, fn)
	if err != nil {
		// fn's own error is the one to give
		_ = end(ctx, stream, wire.Encode(&wire.KvAnswer{}))
		return Fetched[T]{}, err
	}
	if err := end(ctx, stream, answer); err != nil {
		return Fetched[T]{}, err
	}
	return Fetched[T]{Value: made}, nil
}

// made runs fn and encodes what it answered as the run's last DATA.
func (o *Once[T]) made(ctx context.Context, fn func(ctx context.Context) (T, error)) (T, []byte, error) {
	var zero T
	answer, err := fn(ctx)
	if err != nil {
		return zero, nil, err
	}
	raw, err := o.values.Encode(answer)
	if err != nil {
		return zero, nil, err
	}
	return answer, wire.Encode(&wire.KvAnswer{Found: true, Value: &raw}), nil
}

// build runs a build the core handed over in the background, its caller
// answered already: what fn makes, or not found when it fails, goes back as
// the run's last DATA. A connection lost or a store closed meanwhile ends it
// with nothing kept, and no one hears, since no caller waits for it.
func (o *Once[T]) build(ctx context.Context, stream *core.Stream, fn func(ctx context.Context) (T, error)) {
	_, answer, err := o.made(ctx, fn)
	if err != nil {
		answer = wire.Encode(&wire.KvAnswer{})
	}
	_ = end(ctx, stream, answer)
}

// claim asks for key's run: the answer kept for it, or the run the core
// handed over, which a connection lost before the answer asks for again. A
// run handed over with a kept answer, Stale, is a build in the background.
func (o *Once[T]) claim(ctx context.Context, opener *core.Opener, key string) (*core.Stream, Fetched[T], error) {
	var stream *core.Stream
	var kept Fetched[T]
	err := o.store.Link.Run(ctx, core.Reading, func(c *core.Connection) error {
		handle, err := core.HandleOn(ctx, c, opener)
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
		var offered wire.KvAnswer
		if err := decode(first.Body, &offered); err != nil {
			st.Cancel()
			return err
		}
		if !first.End {
			stream = st
			if offered.Refresh {
				kept, err = o.kept(offered)
				kept.Stale = true
			}
			return err
		}
		if !offered.Found {
			return core.Errorf(core.Protocol, "kv once %s: a run ended without its answer", o.name)
		}
		kept, err = o.kept(offered)
		return err
	})
	return stream, kept, err
}

// kept is an answer the core kept, with its age.
func (o *Once[T]) kept(offered wire.KvAnswer) (Fetched[T], error) {
	var raw wire.Raw
	if offered.Value != nil {
		raw = *offered.Value
	}
	value, err := o.values.Decode(raw)
	var age time.Duration
	if offered.Age != nil {
		age = time.Duration(*offered.Age) * time.Millisecond
	}
	return Fetched[T]{Value: value, Age: age, Stale: offered.Stale}, err
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

// Clear forgets every answer of this branch and of the branches under it, in
// one commit. A run under way meanwhile keeps its answer once it ends.
func (o *Once[T]) Clear(ctx context.Context) error {
	_, err := o.via.Call(ctx, o.opener, wire.MethodKvClear, func(handle uint64) []byte {
		return wire.Encode(&wire.KvBranch{Handle: handle, Under: o.under})
	}, core.Writing)
	return err
}
