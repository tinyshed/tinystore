package core

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"os"
	"sync"
	"time"

	"tinyshed.org/tinystore/internal/wire"
)

// What a config is to this process: the value the store sent, as JSON, its
// version and explain, followed on one stream that every handle and watcher
// of it shares. The core reads the layers, checks the rules and keeps the
// changes; a typed config of the root package, and a store's logger, read
// their values from here.

// configTries is how many times a change goes again while other changes race it.
const configTries = 8

// ConfigFollowWait is how long a store's close waits for a config's stream to end.
const ConfigFollowWait = 2 * time.Second

// Config is a config of the store as this process follows it.
type Config struct {
	store *Store
	Name  string
	open  []byte
	// check is the program's validate of a whole value, as JSON; nil for none.
	check func(value json.RawMessage) error

	// opening is held by whoever opens the handle on a connection, so that
	// calls meeting a new connection open it once
	opening sync.Mutex
	conn    *Connection
	handle  uint64

	// taking is held while a value is taken, so that values go in the order they came
	taking sync.Mutex

	mu       sync.Mutex
	value    json.RawMessage
	version  int64
	lines    []wire.ConfigLine
	notes    []string
	refused  error
	taken    int
	changed  chan struct{}
	watchers map[int]func(json.RawMessage)
	nextID   int
	stopped  bool
	stream   *Stream
	followed chan struct{}
}

// OpenConfig opens a config from the message that opens it: its declaration
// and layers as data. It follows the config from then on until the store
// closes; its error is the open's, or what check refused of its value.
func OpenConfig(ctx context.Context, store *Store, message wire.ConfigOpen, check func(json.RawMessage) error) (*Config, error) {
	c := &Config{
		store: store, Name: message.Name, open: wire.Encode(&message), check: check,
		changed: make(chan struct{}), watchers: map[int]func(json.RawMessage){}, followed: make(chan struct{}),
	}
	err := store.Link.Run(ctx, Reading, func(conn *Connection) error {
		_, err := c.handleOn(ctx, conn)
		return err
	})
	if err != nil {
		return nil, err
	}
	c.mu.Lock()
	refused := c.refused
	c.mu.Unlock()
	if refused != nil {
		return nil, refused
	}
	store.OnClose(StopFollowers, c.stop)
	go c.follow()
	return c, nil
}

// Value is the config's value now, its version and how many values it took.
func (c *Config) Value() (value json.RawMessage, version int64, taken int) {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.value, c.version, c.taken
}

// Explained is every setting's line, and what was read and not used, a
// value the program's validate refused among it.
func (c *Config) Explained() ([]wire.ConfigLine, []string) {
	c.mu.Lock()
	defer c.mu.Unlock()
	notes := append([]string(nil), c.notes...)
	if c.refused != nil {
		notes = append(notes, "a value validate refused: "+c.refused.Error())
	}
	return append([]wire.ConfigLine(nil), c.lines...), notes
}

// Changed is closed once a value after the one Value gave is taken, or the
// config stops following.
func (c *Config) Changed() <-chan struct{} {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.changed
}

// Stopped says the config follows no more: its store closed.
func (c *Config) Stopped() bool {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.stopped
}

// OnChange calls watcher with the value now and after every change, until
// what it gives is called; a watcher must not block.
func (c *Config) OnChange(watcher func(json.RawMessage)) (stop func()) {
	c.mu.Lock()
	c.nextID++
	id := c.nextID
	c.watchers[id] = watcher
	value := c.value
	c.mu.Unlock()
	watcher(value)
	return func() {
		c.mu.Lock()
		delete(c.watchers, id)
		c.mu.Unlock()
	}
}

// Update keeps a change of live settings, changes in the declaration's
// shape. The value it makes passes check first, all of it or none; with
// ifVersion, one another change came before is Conflict. Without it, a
// change raced by another goes again on the newer value, as made says.
func (c *Config) Update(ctx context.Context, ifVersion *int64, made func(value json.RawMessage) (json.RawMessage, error)) error {
	for tries := 1; ; tries++ {
		value, version, _ := c.Value()
		if ifVersion != nil {
			version = *ifVersion
		}
		changes, err := made(value)
		if err != nil || changes == nil {
			return err
		}
		if c.check != nil {
			merged, err := mergedJSON(value, changes)
			if err != nil {
				return err
			}
			if err := c.check(merged); err != nil {
				return err
			}
		}
		err = c.call(ctx, Writing, wire.MethodConfigUpdate, func(handle uint64) []byte {
			return wire.Encode(&wire.ConfigUpdate{Handle: handle, Version: version, Changes: string(changes)})
		})
		if KindOf(err) != Conflict || ifVersion != nil || tries == configTries {
			return err
		}
		c.newerThan(ctx, version)
	}
}

// Reset drops the change kept of the setting or the group at path, or of every setting for "".
func (c *Config) Reset(ctx context.Context, path string) error {
	return c.call(ctx, Writing, wire.MethodConfigReset, func(handle uint64) []byte {
		return wire.Encode(&wire.ConfigReset{Handle: handle, Path: path})
	})
}

func (c *Config) call(ctx context.Context, idempotence Idempotence, method uint16, body func(handle uint64) []byte) error {
	return c.store.Link.Run(ctx, idempotence, func(conn *Connection) error {
		handle, err := c.handleOn(ctx, conn)
		if err != nil {
			return err
		}
		answer, err := conn.Session.Call(ctx, method, body(handle))
		if err != nil {
			return err
		}
		var value wire.ConfigValue
		if err := wire.Decode(answer, &value); err != nil {
			return Wrap(Protocol, err, "a config's value that does not read")
		}
		c.take(value)
		return nil
	})
}

// handleOn is the config's handle on a connection, opened there first and its value taken.
func (c *Config) handleOn(ctx context.Context, conn *Connection) (uint64, error) {
	c.opening.Lock()
	defer c.opening.Unlock()
	if c.conn == conn {
		return c.handle, nil
	}
	answer, err := conn.Session.Call(ctx, wire.MethodConfigOpen, c.open)
	if err != nil {
		return 0, err
	}
	var opened wire.ConfigValue
	if err := wire.Decode(answer, &opened); err != nil {
		return 0, Wrap(Protocol, err, "a config's value that does not read")
	}
	c.take(opened)
	c.conn, c.handle = conn, opened.Handle
	return opened.Handle, nil
}

// take takes a value the store sent, in the order it came; one older than
// the config has is left, and one check refuses leaves the value before it.
func (c *Config) take(answer wire.ConfigValue) {
	c.taking.Lock()
	defer c.taking.Unlock()
	c.mu.Lock()
	stale := answer.Version < c.version || (answer.Version == c.version && c.lines != nil)
	first := c.lines == nil
	c.mu.Unlock()
	if stale {
		return
	}
	value := json.RawMessage(answer.Value)
	if len(value) == 0 {
		value = json.RawMessage("{}")
	}
	if c.check != nil {
		if err := c.check(value); err != nil {
			c.mu.Lock()
			c.refused = err
			c.mu.Unlock()
			if !first {
				fmt.Fprintf(os.Stderr, "config %s: version %d: %v; the value before it stays\n", c.Name, answer.Version, err)
			}
			return
		}
	}
	c.mu.Lock()
	c.refused = nil
	c.value, c.version, c.lines, c.notes = value, answer.Version, answer.Lines, answer.Notes
	if c.lines == nil {
		c.lines = []wire.ConfigLine{}
	}
	c.taken++
	close(c.changed)
	c.changed = make(chan struct{})
	watchers := make([]func(json.RawMessage), 0, len(c.watchers))
	for _, watcher := range c.watchers {
		watchers = append(watchers, watcher)
	}
	c.mu.Unlock()
	for _, watcher := range watchers {
		watcher(value)
	}
}

// newerThan waits until a version later than version came, a second at most.
func (c *Config) newerThan(ctx context.Context, version int64) {
	deadline := time.After(time.Second)
	for {
		c.mu.Lock()
		newer, changed := c.version > version || c.stopped, c.changed
		c.mu.Unlock()
		if newer {
			return
		}
		select {
		case <-changed:
		case <-deadline:
			return
		case <-ctx.Done():
			return
		}
	}
}

// follow follows the config on one stream until the store closes, again on
// the next connection when one is lost.
func (c *Config) follow() {
	defer close(c.followed)
	ctx := context.Background()
	for !c.Stopped() {
		var stream *Stream
		err := c.store.Link.Run(ctx, Reading, func(conn *Connection) error {
			handle, err := c.handleOn(ctx, conn)
			if err != nil {
				return err
			}
			opened, err := conn.Session.Open(ctx, wire.MethodConfigFollow, wire.Encode(&wire.Handle{Handle: handle}), true)
			if err != nil {
				return err
			}
			// kept before its first answer, so that a stop meanwhile cancels it
			c.mu.Lock()
			stopped := c.stopped
			c.stream = opened
			c.mu.Unlock()
			if stopped {
				opened.Cancel()
				return Errorf(Cancelled, "the config stopped")
			}
			if _, err := opened.Next(ctx); err != nil {
				return err
			}
			stream = opened
			return nil
		})
		if err == nil {
			err = c.read(ctx, stream)
		}
		if c.Stopped() {
			return
		}
		if lost, _ := IsConnectionLost(err); !lost && KindOf(err) != Unavailable && KindOf(err) != Cancelled {
			time.Sleep(time.Second)
		}
	}
}

// read takes each value a follow's stream brings until it ends.
func (c *Config) read(ctx context.Context, stream *Stream) error {
	for {
		event, err := stream.Next(ctx)
		if err != nil {
			return err
		}
		stream.Consumed(len(event.Body))
		if event.End {
			return nil
		}
		var value wire.ConfigValue
		if err := wire.Decode(event.Body, &value); err != nil {
			return Wrap(Protocol, err, "a config's value that does not read")
		}
		c.take(value)
	}
}

// stop stops following the config, as the store closes: its stream's end
// waited for, so that a private child or a sidecar lets go at once.
func (c *Config) stop() {
	c.mu.Lock()
	c.stopped = true
	c.watchers = map[int]func(json.RawMessage){}
	stream := c.stream
	close(c.changed)
	c.changed = make(chan struct{})
	c.mu.Unlock()
	if stream != nil {
		stream.CancelAndWait(ConfigFollowWait)
	}
	select {
	case <-c.followed:
	case <-time.After(ConfigFollowWait):
	}
}

// mergedJSON is a partial value over a whole one: the groups it names
// merged, anything else replaced.
func mergedJSON(value, changes json.RawMessage) (json.RawMessage, error) {
	whole, err := exactJSON(value)
	if err != nil {
		return nil, Wrap(Protocol, err, "a config's value that does not read")
	}
	partial, err := exactJSON(changes)
	if err != nil {
		return nil, Wrap(Invalid, err, "a change that is no JSON")
	}
	return json.Marshal(merged(whole, partial))
}

// exactJSON is JSON decoded with its numbers as they were written.
func exactJSON(text json.RawMessage) (any, error) {
	decoder := json.NewDecoder(bytes.NewReader(text))
	decoder.UseNumber()
	var value any
	err := decoder.Decode(&value)
	return value, err
}

func merged(value, changes any) any {
	whole, ok := value.(map[string]any)
	partial, given := changes.(map[string]any)
	if !ok || !given {
		return changes
	}
	out := make(map[string]any, len(whole))
	for key, inner := range whole {
		out[key] = inner
	}
	for key, change := range partial {
		out[key] = merged(out[key], change)
	}
	return out
}

// SharedConfig is the config of this open, opened once a process for the
// same declaration and layers, every handle and watcher sharing its stream.
func SharedConfig(ctx context.Context, store *Store, message wire.ConfigOpen) (*Config, error) {
	key := fmt.Sprintf("config\x00%s\x00%s\x00%s\x00%v", message.Name, message.Declaration, message.Layers, message.Strict)
	opening := store.Kept(key, func() any { return &openingConfig{} }).(*openingConfig)
	opening.mu.Lock()
	defer opening.mu.Unlock()
	if opening.config != nil {
		return opening.config, nil
	}
	config, err := OpenConfig(ctx, store, message, nil)
	if err != nil {
		store.Unkept(key, opening)
		return nil, err
	}
	opening.config = config
	return config, nil
}

type openingConfig struct {
	mu     sync.Mutex
	config *Config
}
