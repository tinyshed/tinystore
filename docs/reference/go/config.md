# Config API for Go

Every public type, function and constant of the Config engine in `tinyshed.org/tinystore`, generated from its source. The [Config guide](../../start/why.md#the-engines) explains how to use them, and the [Bun and Node](../bun/config.md), [Python](../python/config.md) and [Rust](../rust/config.md) pages list the same API.

## Config

```go
type Config[T any] struct {
	// contains filtered or unexported fields
}
```

Config is a program's settings as one typed value, read from memory, which a change of a live setting made in any process replaces whole.

### OpenConfig

```go
func OpenConfig[T any](ctx context.Context, store *Store, name string, defaults T, opts ...ConfigOption) (*Config[T], error)
```

OpenConfig opens a config: defaults declare every setting, its type and its default, then each layer declares again what came before, in the order written, and a live setting's change, kept in the store, goes over them all. It opens once its layers are read, or fails with one Invalid error that lists every problem. The same open in the process shares one stream.

	config, err := tinystore.OpenConfig(ctx, store, "app", Settings{Port: 8080},
		tinystore.File("app.yaml"), tinystore.Env("APP", tinystore.Var("port", "PORT")))

### Config.Explain

```go
func (c *Config[T]) Explain() []Line
```

Explain is every setting with its value, the layer that gave it and what it went over, newest first; Notes is what was read and not used.

### Config.Get

```go
func (c *Config[T]) Get() T
```

Get is the settings as they are now, a copy: read it where it is used.

### Config.Notes

```go
func (c *Config[T]) Notes() []string
```

Notes is what the config read and did not use, each with why: an optional file that was absent, a variable with the prefix that names no setting, a kept change left out, a value validate refused.

### Config.Reset

```go
func (c *Config[T]) Reset(ctx context.Context, path string) error
```

Reset drops the change kept of the setting or the group at path, so that the layers give it again; a path with no setting and no change is Invalid.

### Config.ResetAll

```go
func (c *Config[T]) ResetAll(ctx context.Context) error
```

ResetAll drops every change kept of the config.

### Config.Update

```go
func (c *Config[T]) Update(ctx context.Context, change func(*T), opts ...UpdateOption) error
```

Update changes live settings: change sets them on a copy of the value, and each setting it changed is kept in the store, over every layer until Reset, seen by every handle before Update returns, all of them or none. One that is not live is Invalid. A change raced by another runs change again on the newer value, unless IfVersion was given.

	err = config.Update(ctx, func(s *Settings) { s.Limits.RPS = 200 })

### Config.Version

```go
func (c *Config[T]) Version() int64
```

Version is the version of the changes the value holds, which IfVersion takes.

### Config.Watch

```go
func (c *Config[T]) Watch(ctx context.Context) iter.Seq[T]
```

Watch is the settings now, then after every change made in this process or another, whole and once committed; one that falls behind gets the newest. It ends when ctx ends or the store closes.

	for s := range config.Watch(ctx) {
		limiter.SetRate(s.Limits.RPS)
	}

## ConfigOption

```go
type ConfigOption interface {
	// contains filtered or unexported methods
}
```

ConfigOption is a layer of a config after its declaration, or how it opens.

### Env

```go
func Env(prefix string, opts ...EnvOption) ConfigOption
```

Env reads the environment: prefix\_ and each setting's path in upper snake case, APP\_LIMITS\_RPS for limits.rps, a list as a,b,c or JSON and a map as JSON. NAME\_FILE reads the setting from that file, as Docker and Kubernetes pass secrets; a variable with the prefix that names no setting is said.

### File

```go
func File(path string, opts ...FileOption) ConfigOption
```

File is a TOML, YAML or JSON file the config reads, by its extension: .toml, .yaml or .yml, .json or .jsonc, JSON taking comments and a trailing comma. A relative path is the working directory's; one that does not exist is Invalid unless Optional.

### Strict

```go
func Strict() ConfigOption
```

Strict fails the open while a kept change no longer fits the declaration, rather than ignoring it: for a config whose defaults are worse than not starting.

### Validate

```go
func Validate[T any](check func(T) error) ConfigOption
```

Validate checks the whole value at open and before a change is kept: a change it refuses keeps nothing. What relates two settings, a format or a pattern, is its; T is the config's.

## EnvOption

```go
type EnvOption func(*envLayer)
```

EnvOption is how a config reads the environment.

### DotEnv

```go
func DotEnv(files ...string) EnvOption
```

DotEnv reads these .env files for the variables the process lacks; one that does not exist is skipped.

### EnvFrom

```go
func EnvFrom(variables map[string]string) EnvOption
```

EnvFrom reads these variables in place of the process's, as a test gives them.

### Var

```go
func Var(path, name string) EnvOption
```

Var gives the setting at path a variable of its own in place of its prefixed one, such as the PORT and DATABASE\_URL platforms set.

## FileOption

```go
type FileOption func(*fileLayer)
```

FileOption is how a config reads a file.

### Optional

```go
func Optional() FileOption
```

Optional lets the file be absent: then it declares nothing, and Explain says so.

## Line

```go
type Line struct {
	Path, Value, From, Over string
}
```

Line is a setting's line of Explain: its value as JSON, a span spelled and a secret \*\*\*, the layer that gave it and the values it went over, newest first.

### Line.String

```go
func (l Line) String() string
```

## Secret

```go
type Secret struct {
	// contains filtered or unexported fields
}
```

Secret is a setting's text that prints as \*\*\* wherever it goes: fmt, JSON, slog and errors. Reveal gives the text, where the program hands it to what needs it. Hiding it from print is not access control, which the server's tokens are.

### NewSecret

```go
func NewSecret(text string) Secret
```

NewSecret is a secret of this text, a declaration's default.

### Secret.Format

```go
func (Secret) Format(f fmt.State, _ rune)
```

### Secret.GoString

```go
func (Secret) GoString() string
```

### Secret.LogValue

```go
func (Secret) LogValue() slog.Value
```

### Secret.MarshalJSON

```go
func (Secret) MarshalJSON() ([]byte, error)
```

### Secret.MarshalText

```go
func (Secret) MarshalText() ([]byte, error)
```

### Secret.Reveal

```go
func (s Secret) Reveal() string
```

Reveal is the secret's text.

### Secret.String

```go
func (Secret) String() string
```

### Secret.UnmarshalJSON

```go
func (s *Secret) UnmarshalJSON(text []byte) error
```

## UpdateOption

```go
type UpdateOption func(*int64)
```

UpdateOption is what an update needs of the config.

### IfVersion

```go
func IfVersion(version int64) UpdateOption
```

IfVersion keeps the change only while the config is at this version: ErrConflict when another change came since, so that an admin page does not overwrite a change it never showed.

<!-- Generated by task reference from sdk/go/config.go, sdk/go/config_declare.go, sdk/go/config_layers.go. Edit the doc comments there, not this file. -->
