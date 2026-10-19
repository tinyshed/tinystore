// Package channels is messages a program publishes on a named channel, as
// docs/guides/channels/README.md has them: every subscriber listening at that moment
// receives one, in memory and at most once. A subscription's handler is
// called one message at a time; a slow one lets the store's log run ahead and
// is told what it missed, and a connection lost is subscribed again and told
// that messages may have been lost meanwhile.
//
//	messages, err := channels.OpenChannel[Chat](store, "messages")
//	err = messages.Under(roomID).Publish(ctx, Chat{From: "ann", Text: "hi"})
//	for m, err := range messages.Under(roomID).Each(ctx) { … }
package channels

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"iter"
	"strconv"
	"strings"
	"sync"
	"time"

	"tinyshed.org/tinystore"
	"tinyshed.org/tinystore/internal/core"
	"tinyshed.org/tinystore/internal/wire"
)

// quiet is how long a subscription without OnMissed or OnError waits between
// two of TinyStore's own lines.
const quiet = time.Minute

// Message is a message as a subscriber receives it: what was published, and
// the branch it was published under.
type Message[T any] struct {
	Value  T
	Branch []string
}

// Missed is messages a subscriber may have lost: it fell behind its branch's
// log, Reason "behind" and Count how many; or its connection was lost and
// made again, Reason "reconnect", how many unknown.
type Missed struct {
	Reason string
	Count  int
}

// Channel is a channel of messages of one type, or one of its branches.
type Channel[T any] struct {
	store  *core.Store
	name   string
	branch []string
	tx     *core.TxOf
	err    error
}

// OpenChannel opens a channel of messages of T, each written as its JSON.
func OpenChannel[T any](store *tinystore.Store, name string) (*Channel[T], error) {
	if err := core.CheckName(name, "channel"); err != nil {
		return nil, err
	}
	return &Channel[T]{store: store.CoreHome().Store, name: name}, nil
}

// Under is the branch of owners under this one: whose messages these are.
func (c *Channel[T]) Under(owners ...any) *Channel[T] {
	under := *c
	under.branch = append(append([]string(nil), c.branch...), make([]string, 0, len(owners))...)
	for _, owner := range owners {
		text, err := core.KeyText(owner)
		if err != nil && under.err == nil {
			under.err = err
		}
		under.branch = append(under.branch, text)
	}
	return &under
}

// In is the channel inside a transaction, the store's or a database's: its
// messages are handed over once the transaction commits, and never if it
// rolls back.
func (c *Channel[T]) In(tx tinystore.InTx) *Channel[T] {
	in := *c
	of := tx.CoreTx()
	in.tx = &of
	if of.Store != nil && of.Store.Link() != c.store.Link && in.err == nil {
		in.err = core.Errorf(core.Invalid, "%s: a channel of another store, in this transaction", c.shown())
	}
	return &in
}

// Publish hands value to every subscriber of this branch that listens now,
// and of those above it that listen recursively. It answers once the store
// has the message, not once anyone handled it; inside a transaction, once
// the transaction has it.
func (c *Channel[T]) Publish(ctx context.Context, value T) error {
	if c.err != nil {
		return c.err
	}
	text, err := encoded(value)
	if err != nil {
		return core.Wrap(core.Invalid, err, "%s: a message JSON cannot write", c.shown())
	}
	body := wire.Encode(&wire.ChannelsPublish{Name: c.name, Under: c.branch, Value: text})
	switch {
	case c.tx != nil && c.tx.Store != nil:
		return c.tx.Store.After(func(ctx context.Context) error {
			_, err := c.store.Link.Call(ctx, core.Writing, wire.MethodChannelsPublish, body)
			return err
		})
	case c.tx != nil && c.tx.Database != nil:
		_, err := c.tx.Database.Nested(ctx, wire.MethodChannelsPublish, body)
		return err
	}
	_, err = c.store.Link.Call(ctx, core.Writing, wire.MethodChannelsPublish, body)
	return err
}

func encoded(value any) (string, error) {
	var out bytes.Buffer
	encoder := json.NewEncoder(&out)
	encoder.SetEscapeHTML(false)
	if err := encoder.Encode(value); err != nil {
		return "", err
	}
	return strings.TrimSuffix(out.String(), "\n"), nil
}

// shown is the branch as an error names it: channel chat: under "room-7".
func (c *Channel[T]) shown() string {
	if len(c.branch) == 0 {
		return "channel " + c.name
	}
	owners := make([]string, len(c.branch))
	for i, owner := range c.branch {
		owners[i] = strconv.Quote(owner)
	}
	return "channel " + c.name + ": under " + strings.Join(owners, " ")
}

// SubscribeOption is how a subscription listens and what it is told.
type SubscribeOption interface{ subscribe(*told) }

type told struct {
	recursive bool
	onMissed  func(Missed)
	onError   any
}

type optionFunc func(*told)

func (f optionFunc) subscribe(t *told) { f(t) }

// Recursive takes the branches below this one too.
func Recursive() SubscribeOption { return optionFunc(func(t *told) { t.recursive = true }) }

// OnMissed is told what the subscriber may have missed; without it the
// store's log says it, once a minute at most.
func OnMissed(fn func(Missed)) SubscribeOption { return optionFunc(func(t *told) { t.onMissed = fn }) }

// OnError is given what a handler returned, and the message it failed;
// without it the store's log says it, once a minute at most.
func OnError[T any](fn func(err error, m Message[T])) SubscribeOption {
	return optionFunc(func(t *told) { t.onError = fn })
}

func (c *Channel[T]) asked(recursive bool) []byte {
	return wire.Encode(&wire.ChannelsSubscribe{Name: c.name, Under: c.branch, Recursive: recursive})
}

// teller tells what a subscription missed and what its handler returned:
// its own functions, or the store's log once a minute.
type teller[T any] struct {
	shown    string
	onMissed func(Missed)
	onError  func(error, Message[T])
	mu       sync.Mutex
	said     time.Time
}

func (c *Channel[T]) teller(t told) (*teller[T], error) {
	out := &teller[T]{shown: c.shown(), onMissed: t.onMissed}
	if t.onError != nil {
		fn, ok := t.onError.(func(error, Message[T]))
		if !ok {
			return nil, core.Errorf(core.Invalid, "%s: OnError's function takes a Message of another type than the channel's", c.shown())
		}
		out.onError = fn
	}
	return out, nil
}

func (t *teller[T]) missed(missed Missed) {
	if t.onMissed != nil {
		t.onMissed(missed)
		return
	}
	if t.speaks() {
		core.SayOwn("WARN", "a subscriber may have missed messages", "channel", t.shown, "reason", missed.Reason, "count", missed.Count)
	}
}

func (t *teller[T]) failed(err error, m Message[T]) {
	if t.onError != nil {
		t.onError(err, m)
		return
	}
	if t.speaks() {
		core.SayOwn("WARN", "a subscriber's handler failed", "channel", t.shown, "error", err)
	}
}

func (t *teller[T]) speaks() bool {
	t.mu.Lock()
	defer t.mu.Unlock()
	if !t.said.IsZero() && time.Since(t.said) < quiet {
		return false
	}
	t.said = time.Now()
	return true
}

// subscribed opens a subscription's stream and waits for the RESPONSE that says the store has it.
func subscribed(ctx context.Context, store *core.Store, asked []byte) (*core.Stream, error) {
	var stream *core.Stream
	err := store.Link.Run(ctx, core.Reading, func(c *core.Connection) error {
		opened, err := c.Session.Open(ctx, wire.MethodChannelsSubscribe, asked, true)
		if err != nil {
			return err
		}
		if _, err := opened.Next(ctx); err != nil {
			opened.Cancel()
			return err
		}
		stream = opened
		return nil
	})
	return stream, err
}

// again says a subscription ended by its connection subscribes again on another.
func again(err error) bool {
	lost, _ := core.IsConnectionLost(err)
	return lost || core.KindOf(err) == core.Unavailable
}

func messageOf[T any](message wire.ChannelsMessage) (Message[T], error) {
	out := Message[T]{Branch: message.Under}
	if message.Value != "" {
		if err := json.Unmarshal([]byte(message.Value), &out.Value); err != nil {
			return out, core.Wrap(core.Invalid, err, "a message that does not read as %T", out.Value)
		}
	}
	return out, nil
}

// Each is the messages published from now on, as a loop: it listens from
// before its first turn, and leaving the loop, ctx's end or the store's
// close ends it. A connection lost is subscribed again, and OnMissed told.
func (c *Channel[T]) Each(ctx context.Context, opts ...SubscribeOption) iter.Seq2[Message[T], error] {
	var t told
	for _, opt := range opts {
		opt.subscribe(&t)
	}
	return func(yield func(Message[T], error) bool) {
		if c.err != nil {
			yield(Message[T]{}, c.err)
			return
		}
		tell, err := c.teller(t)
		if err != nil {
			yield(Message[T]{}, err)
			return
		}
		asked := c.asked(t.recursive)
		for first := true; ; first = false {
			stream, err := subscribed(ctx, c.store, asked)
			if err != nil {
				if !first && again(err) {
					continue
				}
				yield(Message[T]{}, err)
				return
			}
			if !first {
				tell.missed(Missed{Reason: "reconnect"})
			}
			stop, err := c.each(ctx, stream, tell, yield)
			if stop {
				return
			}
			if !again(err) {
				if !closing(c.store) {
					yield(Message[T]{}, err)
				}
				return
			}
		}
	}
}

// each reads one stream's batches into yield: whether the loop is done, and
// the error that ended the stream.
func (c *Channel[T]) each(ctx context.Context, stream *core.Stream, tell *teller[T], yield func(Message[T], error) bool) (bool, error) {
	forget := c.store.OnClose(core.StopFollowers, func() { stream.CancelAndWait(2 * time.Second) })
	defer forget()
	defer stream.Cancel()
	for {
		event, err := stream.Next(ctx)
		if err != nil {
			if ctx.Err() != nil {
				return true, nil
			}
			return closing(c.store), err
		}
		if event.End {
			return true, nil
		}
		var batch wire.ChannelsBatch
		if err := wire.Decode(event.Body, &batch); err != nil {
			return false, core.Wrap(core.Protocol, err, "a batch that does not read")
		}
		if batch.Missed > 0 {
			tell.missed(Missed{Reason: "behind", Count: int(batch.Missed)})
		}
		for _, each := range batch.Messages {
			m, err := messageOf[T](each)
			if !yield(m, err) {
				return true, nil
			}
		}
		stream.Consumed(len(event.Body))
	}
}

func closing(store *core.Store) bool {
	select {
	case <-store.Closing():
		return true
	default:
		return false
	}
}

// Subscription is a subscription whose handler is called with each message:
// Stop ends it, and the store's close does.
type Subscription struct {
	cancel  context.CancelFunc
	mu      sync.Mutex
	stream  *core.Stream
	stopped bool
	done    chan struct{}
}

type insideKey struct{}

// Subscribe calls handler with each message published from now on, one at a
// time and in order, its ctx ending when the subscription stops. It answers
// once the store has the subscription, so that a message published after it
// arrives; what handler returns goes to OnError.
//
//	sub, err := messages.Under(roomID).Subscribe(ctx, func(ctx context.Context, m channels.Message[Chat]) error {
//		show(m.Value)
//		return nil
//	})
func (c *Channel[T]) Subscribe(ctx context.Context, handler func(ctx context.Context, m Message[T]) error, opts ...SubscribeOption) (*Subscription, error) {
	if c.err != nil {
		return nil, c.err
	}
	var t told
	for _, opt := range opts {
		opt.subscribe(&t)
	}
	tell, err := c.teller(t)
	if err != nil {
		return nil, err
	}
	asked := c.asked(t.recursive)
	stream, err := subscribed(ctx, c.store, asked)
	if err != nil {
		return nil, err
	}
	running, cancel := context.WithCancel(context.WithoutCancel(ctx))
	sub := &Subscription{cancel: cancel, stream: stream, done: make(chan struct{})}
	running = context.WithValue(running, insideKey{}, sub)
	forget := c.store.OnClose(core.StopFollowers, func() { _ = sub.Stop(context.Background()) })
	go func() {
		defer forget()
		defer close(sub.done)
		c.run(running, sub, asked, tell, handler)
	}()
	return sub, nil
}

func (c *Channel[T]) run(ctx context.Context, sub *Subscription, asked []byte, tell *teller[T], handler func(context.Context, Message[T]) error) {
	for {
		sub.mu.Lock()
		stream, stopped := sub.stream, sub.stopped
		sub.mu.Unlock()
		if stopped {
			return
		}
		err := c.deliver(ctx, sub, stream, tell, handler)
		if err == nil || sub.isStopped() || !again(err) {
			if err != nil && !sub.isStopped() && !closing(c.store) {
				core.SayOwn("WARN", "a subscription ended", "channel", tell.shown, "error", err)
			}
			return
		}
		next, err := subscribed(ctx, c.store, asked)
		if err != nil {
			if !sub.isStopped() && !closing(c.store) {
				core.SayOwn("WARN", "a subscription ended", "channel", tell.shown, "error", err)
			}
			return
		}
		sub.mu.Lock()
		sub.stream = next
		sub.mu.Unlock()
		tell.missed(Missed{Reason: "reconnect"})
	}
}

// deliver reads the stream's batches and calls the handler, one message at a time.
func (c *Channel[T]) deliver(ctx context.Context, sub *Subscription, stream *core.Stream, tell *teller[T], handler func(context.Context, Message[T]) error) error {
	for {
		event, err := stream.Next(context.Background())
		if err != nil {
			return err
		}
		if event.End {
			return nil
		}
		var batch wire.ChannelsBatch
		if err := wire.Decode(event.Body, &batch); err != nil {
			return core.Wrap(core.Protocol, err, "a batch that does not read")
		}
		if batch.Missed > 0 && !sub.isStopped() {
			tell.missed(Missed{Reason: "behind", Count: int(batch.Missed)})
		}
		for _, each := range batch.Messages {
			if sub.isStopped() {
				return nil
			}
			m, err := messageOf[T](each)
			if err == nil {
				err = called(ctx, handler, m)
			}
			if err != nil {
				tell.failed(err, m)
			}
		}
		stream.Consumed(len(event.Body))
	}
}

// called runs the handler, a panic in it its error.
func called[T any](ctx context.Context, handler func(context.Context, Message[T]) error, m Message[T]) (err error) {
	defer func() {
		if r := recover(); r != nil {
			err = fmt.Errorf("the handler panicked: %v", r)
		}
	}()
	return handler(ctx, m)
}

func (s *Subscription) isStopped() bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.stopped
}

// Stop ends the subscription: no call starts after it, and it answers once
// the running call ended; inside its own handler, at once.
func (s *Subscription) Stop(ctx context.Context) error {
	s.mu.Lock()
	s.stopped = true
	stream := s.stream
	s.mu.Unlock()
	s.cancel()
	if stream != nil {
		stream.CancelAndWait(2 * time.Second)
	}
	if inside, _ := ctx.Value(insideKey{}).(*Subscription); inside == s {
		return nil
	}
	select {
	case <-s.done:
		return nil
	case <-ctx.Done():
		return ctx.Err()
	}
}
