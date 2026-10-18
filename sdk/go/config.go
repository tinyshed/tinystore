package tinystore

import (
	"context"
	"encoding/json"
	"fmt"
	"iter"
	"log/slog"
	"reflect"
	"strings"
	"sync"

	"tinyshed.org/tinystore/internal/core"
	"tinyshed.org/tinystore/internal/wire"
)

// Secret is a setting's text that prints as *** wherever it goes: fmt, JSON,
// slog and errors. Reveal gives the text, where the program hands it to what
// needs it. Hiding it from print is not access control, which the server's
// tokens are.
type Secret struct{ text string }

// NewSecret is a secret of this text, a declaration's default.
func NewSecret(text string) Secret { return Secret{text: text} }

// Reveal is the secret's text.
func (s Secret) Reveal() string { return s.text }

func (Secret) String() string                     { return "***" }
func (Secret) GoString() string                   { return "***" }
func (Secret) Format(f fmt.State, _ rune)         { _, _ = f.Write([]byte("***")) }
func (Secret) MarshalJSON() ([]byte, error)       { return []byte(`"***"`), nil }
func (Secret) MarshalText() ([]byte, error)       { return []byte("***"), nil }
func (Secret) LogValue() slog.Value               { return slog.StringValue("***") }
func (s *Secret) UnmarshalJSON(text []byte) error { return json.Unmarshal(text, &s.text) }

// ConfigOption is a layer of a config after its declaration, or how it opens.
type ConfigOption interface{ config(*configOptions) }

type configOptions struct {
	layers   []layer
	strict   bool
	validate any
}

// layer is a file or the environment, read when the config opens.
type layer interface {
	data(paths []string) (map[string]any, error)
}

type fileLayer struct {
	path     string
	optional bool
}

func (f fileLayer) config(o *configOptions) { o.layers = append(o.layers, f) }

// FileOption is how a config reads a file.
type FileOption func(*fileLayer)

// Optional lets the file be absent: then it declares nothing, and Explain says so.
func Optional() FileOption { return func(f *fileLayer) { f.optional = true } }

// File is a TOML, YAML or JSON file the config reads, by its extension:
// .toml, .yaml or .yml, .json or .jsonc, JSON taking comments and a trailing
// comma. A relative path is the working directory's; one that does not
// exist is Invalid unless Optional.
func File(path string, opts ...FileOption) ConfigOption {
	f := fileLayer{path: path}
	for _, opt := range opts {
		opt(&f)
	}
	return f
}

type envLayer struct {
	prefix string
	vars   map[string]string
	files  []string
	from   map[string]string
}

func (e envLayer) config(o *configOptions) { o.layers = append(o.layers, e) }

// EnvOption is how a config reads the environment.
type EnvOption func(*envLayer)

// Var gives the setting at path a variable of its own in place of its
// prefixed one, such as the PORT and DATABASE_URL platforms set.
func Var(path, name string) EnvOption {
	return func(e *envLayer) { e.vars[path] = name }
}

// DotEnv reads these .env files for the variables the process lacks; one
// that does not exist is skipped.
func DotEnv(files ...string) EnvOption {
	return func(e *envLayer) { e.files = append(e.files, files...) }
}

// EnvFrom reads these variables in place of the process's, as a test gives them.
func EnvFrom(variables map[string]string) EnvOption {
	return func(e *envLayer) { e.from = variables }
}

// Env reads the environment: prefix_ and each setting's path in upper snake
// case, APP_LIMITS_RPS for limits.rps, a list as a,b,c or JSON and a map as
// JSON. NAME_FILE reads the setting from that file, as Docker and Kubernetes
// pass secrets; a variable with the prefix that names no setting is said.
func Env(prefix string, opts ...EnvOption) ConfigOption {
	e := envLayer{prefix: prefix, vars: map[string]string{}}
	for _, opt := range opts {
		opt(&e)
	}
	return e
}

type strict bool

func (s strict) config(o *configOptions) { o.strict = bool(s) }

// Strict fails the open while a kept change no longer fits the declaration,
// rather than ignoring it: for a config whose defaults are worse than not starting.
func Strict() ConfigOption { return strict(true) }

type validate struct{ check any }

func (v validate) config(o *configOptions) { o.validate = v.check }

// Validate checks the whole value at open and before a change is kept: a
// change it refuses keeps nothing. What relates two settings, a format or a
// pattern, is its; T is the config's.
func Validate[T any](check func(T) error) ConfigOption { return validate{check} }

// UpdateOption is what an update needs of the config.
type UpdateOption func(*int64)

// IfVersion keeps the change only while the config is at this version:
// ErrConflict when another change came since, so that an admin page does not
// overwrite a change it never showed.
func IfVersion(version int64) UpdateOption { return func(v *int64) { *v = version } }

// Line is a setting's line of Explain: its value as JSON, a span spelled and
// a secret ***, the layer that gave it and the values it went over, newest first.
type Line struct {
	Path, Value, From, Over string
}

func (l Line) String() string {
	if l.Over == "" {
		return l.Path + "  " + l.Value + "  " + l.From
	}
	return l.Path + "  " + l.Value + "  " + l.From + "  over " + l.Over
}

// Config is a program's settings as one typed value, read from memory, which
// a change of a live setting made in any process replaces whole.
type Config[T any] struct {
	hub      *core.Config
	declared *declared

	mu      sync.Mutex
	at      int // the hub's values taken when value was read
	value   T
	reading error
}

// OpenConfig opens a config: defaults declare every setting, its type and its
// default, then each layer declares again what came before, in the order
// written, and a live setting's change, kept in the store, goes over them all.
// It opens once its layers are read, or fails with one Invalid error that
// lists every problem. The same open in the process shares one stream.
//
//	config, err := tinystore.OpenConfig(ctx, store, "app", Settings{Port: 8080},
//		tinystore.File("app.yaml"), tinystore.Env("APP", tinystore.Var("port", "PORT")))
func OpenConfig[T any](ctx context.Context, store *Store, name string, defaults T, opts ...ConfigOption) (*Config[T], error) {
	var o configOptions
	for _, opt := range opts {
		opt.config(&o)
	}
	if err := core.CheckName(name, "config"); err != nil {
		return nil, err
	}
	d, err := declare(reflect.ValueOf(defaults))
	if err != nil {
		return nil, core.Wrap(core.Invalid, err, "config %s", name)
	}
	check, err := checkOf[T](name, d, o.validate)
	if err != nil {
		return nil, err
	}
	layers, err := layersData(d, o.layers)
	if err != nil {
		return nil, core.Wrap(core.Invalid, err, "config %s", name)
	}
	message := wire.ConfigOpen{Name: name, Declaration: string(mustJSON(d.data)), Layers: string(mustJSON(layers)), Strict: o.strict}
	hub, err := sharedConfig(ctx, store.core, message, check)
	if err != nil {
		return nil, err
	}
	return &Config[T]{hub: hub, declared: d, at: -1}, nil
}

// sharedConfig is the config of this open, opened once a process for the
// same declaration and layers. One with a validate opens its own, since two
// closures of one function are told apart by nothing Go compares.
func sharedConfig(ctx context.Context, store *core.Store, message wire.ConfigOpen, check func(json.RawMessage) error) (*core.Config, error) {
	if check != nil {
		return core.OpenConfig(ctx, store, message, check)
	}
	return core.SharedConfig(ctx, store, message)
}

// checkOf is a validate as the hub runs it on a whole value's JSON.
func checkOf[T any](name string, d *declared, given any) (func(json.RawMessage) error, error) {
	if given == nil {
		return nil, nil
	}
	fn, ok := given.(func(T) error)
	if !ok {
		return nil, core.Errorf(core.Invalid, "config %s: Validate's function takes a %s, not what %T takes", name, reflect.TypeFor[T](), given)
	}
	return func(value json.RawMessage) error {
		settings, err := valueOf[T](d, value)
		if err != nil {
			return err
		}
		if err := fn(settings); err != nil {
			if core.KindOf(err) == core.Invalid {
				return err
			}
			return core.Wrap(core.Invalid, err, "config %s", name)
		}
		return nil
	}, nil
}

// Get is the settings as they are now, a copy: read it where it is used.
func (c *Config[T]) Get() T {
	c.mu.Lock()
	defer c.mu.Unlock()
	value, _, taken := c.hub.Value()
	if taken != c.at {
		c.value, c.reading = valueOf[T](c.declared, value)
		c.at = taken
	}
	return copied(c.declared, c.value)
}

// Version is the version of the changes the value holds, which IfVersion takes.
func (c *Config[T]) Version() int64 {
	_, version, _ := c.hub.Value()
	return version
}

// Update changes live settings: change sets them on a copy of the value, and
// each setting it changed is kept in the store, over every layer until
// Reset, seen by every handle before Update returns, all of them or none.
// One that is not live is Invalid. A change raced by another runs change
// again on the newer value, unless IfVersion was given.
//
//	err = config.Update(ctx, func(s *Settings) { s.Limits.RPS = 200 })
func (c *Config[T]) Update(ctx context.Context, change func(*T), opts ...UpdateOption) error {
	var ifVersion *int64
	for _, opt := range opts {
		version := int64(0)
		opt(&version)
		ifVersion = &version
	}
	return c.hub.Update(ctx, ifVersion, func(value json.RawMessage) (json.RawMessage, error) {
		before, err := valueOf[T](c.declared, value)
		if err != nil {
			return nil, err
		}
		after := copied(c.declared, before)
		change(&after)
		changes := changesOf(c.declared.root, reflect.ValueOf(&before).Elem(), reflect.ValueOf(&after).Elem())
		if changes == nil {
			return nil, nil
		}
		return mustJSON(changes), nil
	})
}

// Reset drops the change kept of the setting or the group at path, so that
// the layers give it again; a path with no setting and no change is Invalid.
func (c *Config[T]) Reset(ctx context.Context, path string) error {
	if path == "" {
		return core.Errorf(core.Invalid, "config %s: reset of no path: ResetAll drops every change", c.hub.Name)
	}
	return c.hub.Reset(ctx, path)
}

// ResetAll drops every change kept of the config.
func (c *Config[T]) ResetAll(ctx context.Context) error { return c.hub.Reset(ctx, "") }

// Watch is the settings now, then after every change made in this process
// or another, whole and once committed; one that falls behind gets the
// newest. It ends when ctx ends or the store closes.
//
//	for s := range config.Watch(ctx) {
//		limiter.SetRate(s.Limits.RPS)
//	}
func (c *Config[T]) Watch(ctx context.Context) iter.Seq[T] {
	return func(yield func(T) bool) {
		seen := -1
		for {
			changed := c.hub.Changed()
			_, _, taken := c.hub.Value()
			if taken != seen {
				seen = taken
				if !yield(c.Get()) {
					return
				}
				continue
			}
			if c.hub.Stopped() {
				return
			}
			select {
			case <-changed:
			case <-ctx.Done():
				return
			}
		}
	}
}

// Explain is every setting with its value, the layer that gave it and what
// it went over, newest first; Notes is what was read and not used.
func (c *Config[T]) Explain() []Line {
	lines, _ := c.hub.Explained()
	out := make([]Line, len(lines))
	for i, line := range lines {
		out[i] = Line{Path: line.Path, Value: line.Value, From: line.From, Over: strings.Join(line.Over, ", ")}
	}
	return out
}

// Notes is what the config read and did not use, each with why: an optional
// file that was absent, a variable with the prefix that names no setting, a
// kept change left out, a value validate refused.
func (c *Config[T]) Notes() []string {
	_, notes := c.hub.Explained()
	return notes
}
