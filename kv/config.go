package kv

import (
	"bytes"
	"cmp"
	"context"
	"encoding"
	"encoding/json"
	"errors"
	"fmt"
	"iter"
	"log/slog"
	"maps"
	"reflect"
	"slices"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/tinyshed/tinystore"
)

// Config is an application's settings, a value of T made of layers in the
// order given, each over the one before it, and what Update kept over them all:
//
//	kv.Defaults(Config{Port: 8080}), kv.Defaults(fromYAML)
//	kv.FromEnv("APP", ".env")       APP_PORT=3000
//	config.Update(ctx, func(c *Config) { c.Port = 4000 })
//
// Get reads it from memory. Update checks a change and keeps it field by field
// in kv.db, so that it outlives a restart, and every handle on the config sees
// it at once, in this process and through the server; Reset gives a field back
// to the layers under it.
//
// T is a struct. A field's path is its JSON name, a nested struct's below its
// own: limits.rps. A field tagged `env:"NAME"` reads that variable; another
// reads the prefix and its path in upper snake case, APP_LIMITS_RPS, or the
// file that APP_LIMITS_RPS_FILE names. A field tagged `fixed:"true"` comes
// from the layers alone: Update refuses it, and Sources shows where it came
// from. A field tagged `secret:"true"` is fixed, and Sources hides it. A field
// tagged `required:"true"` is ErrInvalid at OpenConfig while the layers leave
// it its zero value.
type Config[T any] struct {
	hub    *configHub
	name   string
	fields []configField
	base   map[string]any    // the defaults and the environment, a JSON tree
	from   map[string]string // where each path of base came from
	check  func(T) error
	log    *slog.Logger

	mu      sync.Mutex // one refresh or Update of this handle at a time
	current atomic.Pointer[configValue[T]]
}

// configValue is the config as one change of the hub left it
type configValue[T any] struct {
	value   T
	tree    map[string]any
	changes int64             // the hub's change it was built from
	ignored map[string]string // kept paths left out, and why
}

// configField is one leaf of a config's type, which the environment can set
// and Update keeps by its path
type configField struct {
	path     string
	variable string // its env tag, read in place of the prefix and its path
	secret   bool
	fixed    bool // from the layers alone; a secret is fixed too
	required bool
	kind     reflect.Type
}

// ConfigOption is where a config's values come from, and what checks them.
type ConfigOption func(*configSettings)

type configSettings struct {
	layers []configLayer
	check  any
}

// configLayer is a Defaults' values, or a FromEnv's environment when env is set
type configLayer struct {
	values any
	env    *envLayer
}

type envLayer struct {
	prefix string
	files  []string
	lookup func(string) (string, bool) // in place of the files and the process's own
}

// Defaults is a layer of values: a T, which sets every field, or a map, as a
// JSON or YAML library reads a file into one, which sets the fields it names.
// A duration may be text, "30s".
func Defaults(values any) ConfigOption {
	return func(s *configSettings) { s.layers = append(s.layers, configLayer{values: values}) }
}

// FromEnv is a layer of the environment: each field from its variable, files
// first, as a dotenv library reads them, and the process's own over them. A
// file that is not there is skipped.
//
//	PORT=3000  ORIGINS=a.com,b.com  TIMEOUT=1h30m  LIMITS={"rps":5}
func FromEnv(prefix string, files ...string) ConfigOption {
	return func(s *configSettings) {
		s.layers = append(s.layers, configLayer{env: &envLayer{prefix: prefix, files: files}})
	}
}

// FromLookup is a layer of the environment read through lookup alone, the
// process's own variables left as they are, which suits a host that passes
// its environment in and a test that sets its own.
func FromLookup(prefix string, lookup func(name string) (string, bool)) ConfigOption {
	return func(s *configSettings) {
		s.layers = append(s.layers, configLayer{env: &envLayer{prefix: prefix, lookup: lookup}})
	}
}

// Validate checks the config each time it is made, at open and before Update
// keeps a change: a change that fails is refused, and a kept value that fails
// is left out.
func Validate[T any](check func(T) error) ConfigOption {
	return func(s *configSettings) { s.check = check }
}

// OpenConfig opens the config name of kv.db, creating it the first time, and
// makes it from its layers. Defaults and an environment that do not fit T, or
// that fail Validate, are ErrInvalid; a kept value that no longer fits its
// field is left out, logged, and named by Sources.
func OpenConfig[T any](ctx context.Context, state *Store, name string, options ...ConfigOption) (*Config[T], error) {
	var said configSettings
	for _, option := range options {
		option(&said)
	}
	c := &Config[T]{name: name, log: state.log.With("config", name)}
	if err := c.settle(said); err != nil {
		return nil, fmt.Errorf("kv: config %q: %w", name, err)
	}

	hub, err := state.configHub(ctx, name)
	if err != nil {
		return nil, err
	}
	c.hub = hub
	if _, err = c.refresh(); err != nil {
		return nil, fmt.Errorf("kv: config %q: %w", name, err)
	}
	return c, nil
}

// settle reads T's fields and makes the layers under what is kept
func (c *Config[T]) settle(said configSettings) error {
	t := reflect.TypeFor[T]()
	if t.Kind() != reflect.Struct {
		return fmt.Errorf("%w: a config is a struct, not %s", tinystore.ErrInvalid, t)
	}
	if said.check != nil {
		check, ok := said.check.(func(T) error)
		if !ok {
			return fmt.Errorf("%w: Validate of %T for a config of %s", tinystore.ErrInvalid, said.check, t)
		}
		c.check = check
	}
	c.fields = shapeOf(t, "", configField{})

	var zero T
	base, err := treeOf(zero)
	if err != nil {
		return err
	}
	c.base, c.from = base, map[string]string{}
	var env *envLayer // the last, which names a missing field's variable
	var unread []error
	for _, layer := range said.layers {
		if layer.env == nil {
			if err = c.layDefaults(layer.values); err != nil {
				return err
			}
			continue
		}
		env = layer.env
		bad, err := c.layEnvironment(*layer.env)
		if err != nil {
			return err
		}
		unread = append(unread, bad...)
	}
	if len(unread) > 0 {
		return fmt.Errorf("%w: %w", tinystore.ErrInvalid, errors.Join(unread...))
	}
	return c.given(env)
}

// given is ErrInvalid for the required fields the layers left empty, all of
// them, each naming the variable that would give it
func (c *Config[T]) given(env *envLayer) error {
	var missing []error
	for _, f := range c.fields {
		if value, _ := pathIn(c.base, f.path); !f.required || !f.empty(value) {
			continue
		}
		if env == nil {
			missing = append(missing, fmt.Errorf("%s is required", f.path))
		} else {
			missing = append(missing, fmt.Errorf("%s is required: set %s", f.path, f.envName(env.prefix)))
		}
	}
	if len(missing) == 0 {
		return nil
	}
	return fmt.Errorf("%w: %w", tinystore.ErrInvalid, errors.Join(missing...))
}

// layDefaults merges a layer of defaults over base, each field it sets checked
// against its type
func (c *Config[T]) layDefaults(defaults any) error {
	tree, err := treeOf(defaults)
	if err != nil {
		return fmt.Errorf("%w: defaults of %T: %w", tinystore.ErrInvalid, defaults, err)
	}
	for _, f := range c.fields {
		value, ok := pathIn(tree, f.path)
		if !ok {
			continue
		}
		fitted, err := f.fit(value)
		if err != nil {
			return fmt.Errorf("%w: the default of %s: %w", tinystore.ErrInvalid, f.path, err)
		}
		setPath(c.base, f.path, fitted)
		c.from[f.path] = "default"
	}
	return nil
}

// layEnvironment sets each field whose variable, or whose file, is there, and
// returns every variable that does not read, so that one restart shows them all
func (c *Config[T]) layEnvironment(layer envLayer) ([]error, error) {
	lookup := layer.lookup
	if lookup == nil {
		var err error
		if lookup, err = readEnvironment(layer.files); err != nil {
			return nil, err
		}
	}
	var unread []error
	for _, f := range c.fields {
		name := f.envName(layer.prefix)
		text, from, err := variable(lookup, name)
		if err == nil && from == "" {
			continue
		}
		if err == nil {
			err = c.setFromEnv(f, text, from)
		}
		if err != nil {
			unread = append(unread, fmt.Errorf("%s: %w", cmp.Or(from, name), err))
		}
	}
	return unread, nil
}

// setFromEnv sets a field from a variable's text, from naming where it came from
func (c *Config[T]) setFromEnv(f configField, text, from string) error {
	spelled, err := envJSON(text, f.kind)
	if err != nil {
		return err
	}
	fitted, err := f.fit(decodeJSON(spelled))
	if err != nil {
		return err
	}
	setPath(c.base, f.path, fitted)
	c.from[f.path] = "env " + from
	return nil
}

// Get is the config now, built once a change and shared: it is not to be
// changed, which Update does.
func (c *Config[T]) Get() T {
	if built := c.current.Load(); built.changes == c.hub.changes.Load() {
		return built.value
	}
	c.mu.Lock()
	defer c.mu.Unlock()
	built, err := c.refresh()
	if err != nil {
		c.log.Error("the config does not build; it stays as it was", "err", err)
		return c.current.Load().value
	}
	return built.value
}

// refresh makes the config from base and what the hub keeps, leaving out a
// kept value that no longer fits its field, and all of them when they make a
// config Validate refuses
func (c *Config[T]) refresh() (*configValue[T], error) {
	kept, changes := c.hub.snapshot()
	if built := c.current.Load(); built != nil && built.changes == changes {
		return built, nil
	}
	tree, ignored := c.withKept(kept)
	value, err := c.build(tree)
	if err != nil && len(kept) > len(ignored) {
		reason := "the config fails with it: " + err.Error()
		for path := range kept {
			ignored[path] = cmp.Or(ignored[path], reason)
		}
		tree = cloneTree(c.base)
		value, err = c.build(tree)
	}
	if err != nil {
		return nil, err
	}
	for _, path := range slices.Sorted(maps.Keys(ignored)) {
		c.log.Warn("a kept value is left out", "path", path, "why", ignored[path])
	}
	built := &configValue[T]{value: value, tree: tree, changes: changes, ignored: ignored}
	c.current.Store(built)
	return built, nil
}

// withKept is base with each kept value that fits its field over it; a secret
// is never taken from what was kept
func (c *Config[T]) withKept(kept map[string][]byte) (map[string]any, map[string]string) {
	tree, ignored := cloneTree(c.base), map[string]string{}
	for path, spelled := range kept {
		f, ok := c.field(path)
		if !ok {
			ignored[path] = "the config has no such field"
			continue
		}
		if f.secret {
			ignored[path] = "the field is a secret"
			continue
		}
		if f.fixed {
			ignored[path] = "the field is fixed"
			continue
		}
		fitted, err := f.fit(decodeJSON(spelled))
		switch {
		case err != nil:
			ignored[path] = err.Error()
		case f.required && f.empty(fitted):
			ignored[path] = "the field is required"
		default:
			setPath(tree, path, fitted)
		}
	}
	return tree, ignored
}

func (c *Config[T]) build(tree map[string]any) (T, error) {
	var value T
	spelled, err := json.Marshal(tree)
	if err == nil {
		err = json.Unmarshal(spelled, &value)
	}
	if err == nil {
		err = c.checked(value)
	}
	return value, err
}

// checked is what Validate says of value, as ErrInvalid
func (c *Config[T]) checked(value T) error {
	if c.check == nil {
		return nil
	}
	if err := c.check(value); err != nil {
		return fmt.Errorf("%w: %w", tinystore.ErrInvalid, err)
	}
	return nil
}

// Update changes the config: change gets a copy of it, and each field it
// changes is checked, kept, and seen by every handle at once. A change that
// fails Validate, sets a fixed field or a secret, or empties a required field
// is ErrInvalid and keeps nothing.
func (c *Config[T]) Update(ctx context.Context, change func(*T)) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	built, err := c.refresh()
	if err != nil {
		return err
	}
	next, err := c.build(cloneTree(built.tree))
	if err != nil {
		return err
	}
	change(&next)

	set, err := c.changed(built.tree, next)
	if err != nil || len(set) == 0 {
		return err
	}
	if err = c.checked(next); err != nil {
		return fmt.Errorf("kv: config %q: %w", c.name, err)
	}
	return c.hub.change(ctx, set, nil)
}

// changed is the fields next holds otherwise than tree, as JSON by path
func (c *Config[T]) changed(tree map[string]any, next T) (map[string][]byte, error) {
	after, err := treeOf(next)
	if err != nil {
		return nil, err
	}
	set := map[string][]byte{}
	for _, f := range c.fields {
		was, _ := pathIn(tree, f.path)
		now, _ := pathIn(after, f.path)
		nowJSON := spelledTree(now)
		if spelledTree(was) == nowJSON {
			continue
		}
		if f.secret {
			return nil, fmt.Errorf("%w: kv: config %q: %s is a secret, set by the layers alone",
				tinystore.ErrInvalid, c.name, f.path)
		}
		if f.fixed {
			return nil, fmt.Errorf("%w: kv: config %q: %s is fixed, set by the layers alone",
				tinystore.ErrInvalid, c.name, f.path)
		}
		if f.required && f.empty(now) {
			return nil, fmt.Errorf("%w: kv: config %q: %s is required", tinystore.ErrInvalid, c.name, f.path)
		}
		set[f.path] = []byte(nowJSON)
	}
	return set, nil
}

// Reset forgets what Update kept for paths, each a field or a struct of
// fields, so that they take their values from the layers under it again;
// without paths it forgets everything kept.
func (c *Config[T]) Reset(ctx context.Context, paths ...string) error {
	kept, _ := c.hub.snapshot()
	var reset []string
	for _, path := range paths {
		if !slices.ContainsFunc(c.fields, func(f configField) bool { return under(f.path, path) }) {
			return fmt.Errorf("%w: kv: config %q has no field %s", tinystore.ErrInvalid, c.name, path)
		}
	}
	for path := range kept {
		if len(paths) == 0 || slices.ContainsFunc(paths, func(given string) bool { return under(path, given) }) {
			reset = append(reset, path)
		}
	}
	if len(reset) == 0 {
		return nil
	}
	return c.hub.change(ctx, nil, reset)
}

// Watch yields the config now and each time it changes, from this handle or
// any other, until ctx ends. A watcher that falls behind skips to the latest.
func (c *Config[T]) Watch(ctx context.Context) iter.Seq[T] {
	return func(yield func(T) bool) {
		for {
			next := c.hub.waiting()
			if !yield(c.Get()) {
				return
			}
			select {
			case <-ctx.Done():
				return
			case <-next:
			}
		}
	}
}

// Source is where one field's value came from.
type Source struct {
	Path string
	// Value is its JSON, or *** for a secret.
	Value string
	// From is "default", "env NAME", "kept", or "" for T's zero value.
	From string
	// Ignored says why a kept value is left out, when it is.
	Ignored string
}

// Sources says where each field's value came from, in the order of T's
// fields, and why a kept value is left out.
func (c *Config[T]) Sources() []Source {
	c.Get()
	built := c.current.Load()
	kept, _ := c.hub.snapshot()
	sources := make([]Source, 0, len(c.fields))
	for _, f := range c.fields {
		value, _ := pathIn(built.tree, f.path)
		source := Source{Path: f.path, Value: spelledTree(value), From: c.from[f.path], Ignored: built.ignored[f.path]}
		if _, ok := kept[f.path]; ok && source.Ignored == "" {
			source.From = "kept"
		}
		if f.secret {
			source.Value = "***"
		}
		sources = append(sources, source)
	}
	return sources
}

func (c *Config[T]) field(path string) (configField, bool) {
	for _, f := range c.fields {
		if f.path == path {
			return f, true
		}
	}
	return configField{}, false
}

var (
	timeType        = reflect.TypeFor[time.Time]()
	jsonUnmarshaler = reflect.TypeFor[json.Unmarshaler]()
	textUnmarshaler = reflect.TypeFor[encoding.TextUnmarshaler]()
)

// shapeOf is t's leaves, as encoding/json names them, their paths below
// parent: a struct that reads itself, or a time, is a leaf, and another struct
// a group of fields
func shapeOf(t reflect.Type, parent string, inherited configField) []configField {
	var fields []configField
	for i := range t.NumField() {
		f := t.Field(i)
		name, ok := jsonName(f)
		if !ok {
			continue
		}
		path := name
		if parent != "" {
			path = parent + "." + name
		}
		marked := configField{
			secret: inherited.secret || f.Tag.Get("secret") == "true",
			fixed:  inherited.fixed || f.Tag.Get("fixed") == "true",
		}
		if group(f.Type) {
			below := path
			if f.Anonymous && f.Tag.Get("json") == "" {
				below = parent
			}
			fields = append(fields, shapeOf(f.Type, below, marked)...)
			continue
		}
		fields = append(fields, configField{
			path: path, variable: f.Tag.Get("env"), secret: marked.secret, fixed: marked.fixed,
			required: f.Tag.Get("required") == "true", kind: f.Type,
		})
	}
	return fields
}

// envName is the field's variable after prefix: its env tag, or the prefix and its path
func (f configField) envName(prefix string) string {
	return cmp.Or(f.variable, envName(prefix, f.path))
}

// empty is a value its field holds as the zero value of its type, or as an
// empty list or map
func (f configField) empty(value any) bool {
	spelled, err := json.Marshal(value)
	held := reflect.New(f.kind)
	if err != nil || json.Unmarshal(spelled, held.Interface()) != nil {
		return true
	}
	v := held.Elem()
	return v.IsZero() || (v.Kind() == reflect.Slice || v.Kind() == reflect.Map) && v.Len() == 0
}

// jsonName is a field's name in JSON, and false for one JSON leaves out
func jsonName(f reflect.StructField) (string, bool) {
	tag := f.Tag.Get("json")
	if tag == "-" || !f.IsExported() && !f.Anonymous {
		return "", false
	}
	name, _, _ := strings.Cut(tag, ",")
	if name == "" {
		name = f.Name
	}
	return name, true
}

func group(t reflect.Type) bool {
	return t.Kind() == reflect.Struct && t != timeType &&
		!reflect.PointerTo(t).Implements(jsonUnmarshaler) && !reflect.PointerTo(t).Implements(textUnmarshaler)
}

// fit is value as the tree keeps it for this field, refused when the field's
// type cannot hold it; a duration may be text, "30s", which becomes its
// nanoseconds
func (f configField) fit(value any) (any, error) {
	if text, ok := value.(string); ok && f.kind == durationType {
		d, err := time.ParseDuration(text)
		if err != nil {
			return nil, fmt.Errorf("%q is no duration", text)
		}
		value = json.Number(fmt.Sprint(int64(d)))
	}
	spelled, err := json.Marshal(value)
	if err != nil {
		return nil, err
	}
	if err = json.Unmarshal(spelled, reflect.New(f.kind).Interface()); err != nil {
		var mismatch *json.UnmarshalTypeError
		if errors.As(err, &mismatch) {
			return nil, fmt.Errorf("%s is no %s", spelled, f.kind)
		}
		return nil, err
	}
	return value, nil
}

// treeOf is a value as a JSON tree, its numbers kept as they are spelled
func treeOf(value any) (map[string]any, error) {
	spelled, err := json.Marshal(value)
	if err != nil {
		return nil, err
	}
	tree, ok := decodeJSON(spelled).(map[string]any)
	if !ok {
		return nil, fmt.Errorf("%s is no object", spelled)
	}
	return tree, nil
}

// spelledTree is a tree's value as JSON, which a value decoded from JSON
// always has
func spelledTree(value any) string {
	spelled, err := json.Marshal(value)
	if err != nil {
		return "null"
	}
	return string(spelled)
}

func decodeJSON(spelled []byte) any {
	decoder := json.NewDecoder(bytes.NewReader(spelled))
	decoder.UseNumber()
	var value any
	if decoder.Decode(&value) != nil {
		return nil
	}
	return value
}

// pathIn is the value at a dotted path of a tree
func pathIn(tree map[string]any, path string) (any, bool) {
	var at any = tree
	for name := range strings.SplitSeq(path, ".") {
		object, ok := at.(map[string]any)
		if !ok {
			return nil, false
		}
		if at, ok = object[name]; !ok {
			return nil, false
		}
	}
	return at, true
}

// setPath puts a value at a dotted path, making the objects on the way
func setPath(tree map[string]any, path string, value any) {
	names := strings.Split(path, ".")
	for _, name := range names[:len(names)-1] {
		inner, ok := tree[name].(map[string]any)
		if !ok {
			inner = map[string]any{}
			tree[name] = inner
		}
		tree = inner
	}
	tree[names[len(names)-1]] = value
}

func cloneTree(tree map[string]any) map[string]any {
	cloned := make(map[string]any, len(tree))
	for name, value := range tree {
		if inner, ok := value.(map[string]any); ok {
			value = cloneTree(inner)
		}
		cloned[name] = value
	}
	return cloned
}

// under says whether path is given or lies below it
func under(path, given string) bool {
	return path == given || strings.HasPrefix(path, given+".")
}
