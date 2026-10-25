# Layers and files

A config is read from layers in the order you write them: the declaration
first, with every setting and its default, then objects in code, files and
the environment. For each setting the last layer that gives it wins, as in
`{ ...a, ...b }`.

```ts
const config = await store.config('app', [
	{ port: 8080, origins: ['localhost'], limits: { rps: live(100) }, region: required(String) },
	process.env.NODE_ENV === 'production' && { limits: { rps: 1000 } },
	file('app.toml'),
	file('app.local.toml', { optional: true }),
	env('APP', { vars: { port: 'PORT' } }),
])
```

```python
@dataclass
class Limits:
    rps: int = live(100)


@dataclass(kw_only=True)
class Settings:
    port: int = 8080
    origins: list[str] = field(default_factory=lambda: ["localhost"])
    limits: Limits = field(default_factory=Limits)
    region: str  # no default: required


config = await store.config("app", [
    Settings,
    {"limits": {"rps": 1000}} if production else None,
    file("app.toml"),
    file("app.local.toml", optional=True),
    env("APP", vars={"port": "PORT"}),
])
```

```go
config, err := tinystore.OpenConfig(ctx, store, "app",
	Settings{Port: 8080, Origins: []string{"localhost"}, Limits: Limits{RPS: 100}},
	tinystore.File("app.toml"),
	tinystore.File("app.local.toml", tinystore.Optional()),
	tinystore.Env("APP", tinystore.Var("port", "PORT")))
```

```rust
let config = store
    .config("app", Settings::default())
    .file("app.toml")
    .file(File::optional("app.local.toml"))
    .env(Env::prefix("APP").var("port", "PORT"))
    .open()?;
```

Here `limits.rps` is 100, 1000 in production, then what `app.toml`,
`app.local.toml` and `APP_LIMITS_RPS` say, in that order. A config opens with
every layer read, or fails with one error that lists every problem.

## The declaration

The first layer declares every setting: its name, its type and its default.
A setting's type is its default's: a number, text, a boolean, a list, or a
group of settings, to any depth.

| Language   | The declaration is                                                               |
|------------|----------------------------------------------------------------------------------|
| TypeScript | an object; `live`, `secret`, `required` and the rules mark its settings          |
| Python     | a dataclass; a field without a default is required                               |
| Go         | a struct value with `json` tags, `config:"required,live,min=1"` marks            |
| Rust       | a `serde` value that is `Clone`, `Live<T>`, `Secret<T>` and `Required<T>` fields |

An object after the declaration declares again settings the declaration
has; a key it does not have is `invalid`. `false`, `null` and `undefined` in
the list are skipped, so a layer can depend on a condition.

The config's name, `'app'`, is its identity in the store: programs that share
a store and a name share their [live changes](live.md).

## Files

`file(path)` reads TOML, YAML or JSON by the extension:

| Extension         | Read as                                                                     |
|-------------------|-----------------------------------------------------------------------------|
| `.toml`           | TOML                                                                        |
| `.yaml`, `.yml`   | YAML 1.2                                                                    |
| `.json`, `.jsonc` | JSON with `//` and `/* */` comments and trailing commas, as `tsconfig.json` |

```toml title="app.toml"
port = 9000

[limits]
rps = 1000
```

- A relative path is the working directory's.
- A file that does not exist is `invalid`, naming its path, unless it is
  `optional`: then `explain` says it was absent.
- A key the declaration does not have is `invalid`, with the nearest name:
  `app.toml: prot: no such setting; did you mean port?`. A typo never silently
  does nothing.
- A file is read when the config opens, 1 MiB at most.

## The environment

`env('APP')` reads `APP_` and each setting's path in upper snake case:

| Setting      | Variable          |
|--------------|-------------------|
| `port`       | `APP_PORT`        |
| `limits.rps` | `APP_LIMITS_RPS`  |
| `db.maxIdle` | `APP_DB_MAX_IDLE` |

- A variable is read as its setting's type: a number in decimal, `true` or
  `false`, a list as `a,b,c` or JSON, a map as JSON, text as it is.
- `vars` gives a setting a variable of its own in place of the prefixed one,
  such as the `PORT` and `DATABASE_URL` platforms set.
- A variable with the prefix that names no setting is said once at open and
  listed by `explain`: `APP_PROT=9000` is reported with `did you mean
  APP_PORT?`, never ignored.
- `files: ['.env']` reads `.env` files for the variables the process lacks;
  one that does not exist is skipped.
- `from: { APP_PORT: '9000' }` reads these variables in place of the
  process's, for tests.

```ts
const config = await store.config('app', [decl, env('APP', { from: { APP_REGION: 'eu', APP_PORT: '9000' } })])
```

```python
config = await store.config("app", [Settings, env("APP", from_={"APP_REGION": "eu", "APP_PORT": "9000"})])
```

```go
config, err := tinystore.OpenConfig(ctx, store, "app", defaults,
	tinystore.Env("APP", tinystore.EnvFrom(map[string]string{"APP_REGION": "eu", "APP_PORT": "9000"})))
```

```rust
let config = store.config("app", Settings::default()).env(Env::prefix("APP").from([("APP_REGION", "eu")])).open()?;
```

## Rules

A rule says more about one value than its type, and every layer and change
is checked against it:

```ts
port: int(8080, { min: 1, max: 65535 }),
level: oneOf('info', ['debug', 'warn', 'error']), // read as 'info' | 'debug' | 'warn' | 'error'
timeout: duration('30s'),
hosts: list(String, { default: ['localhost'], min: 1, max: 20 }),
workers: required(int({ min: 1 })),
```

```python
port: Annotated[int, Range(1, 65535)] = 8080
level: Literal["info", "debug", "warn", "error"] = "info"
timeout: timedelta = timedelta(seconds=30)
hosts: Annotated[list[str], Items(1, 20)] = field(default_factory=lambda: ["localhost"])
workers: Annotated[int, Range(min=1)]
```

```go
Port    int           `json:"port" config:"min=1,max=65535"`
Level   string        `json:"level" config:"oneof=info debug warn error"`
Timeout time.Duration `json:"timeout"`
Hosts   []string      `json:"hosts" config:"items=1-20"`
Workers int           `json:"workers" config:"required,min=1"`
```

```rust
port: u16,
level: Level, // an enum: Info, Debug, Warn, Error
timeout: Duration,
hosts: Vec<String>,
workers: Required<NonZeroU32>,
```

```text
config app: APP_LEVEL: "verbose" is not one of info, debug, warn, error
config app: app.toml: port: 70000 is above 65535
```

A duration is written `'1h30m'`, `'250ms'`, of `w`, `d`, `h`, `m`, `s` and
`ms`. A list is one setting: a later layer gives it whole, never merged by
index. What relates two settings, `min` below `max`, is
[`validate`](explain.md)'s.

## Without a store

A Rust program that only reads its settings at start, such as an agent that
runs beside someone else's database, can load them without opening a store.
`config::load` reads the same layers with the same rules, secrets and
errors, and writes nothing to disk: no directory, no `kv.db`.

```rust
let agent = tinystore::config::load("agent", Agent::default())
    .file(File::optional("agent.toml"))
    .env(Env::prefix("AGENT"))
    .open()?;
let hub = agent.get().hub.clone();
```

- A loaded config has no live settings, because only a store keeps a change.
  A `Live` field is refused when it opens, and the error names the field.
- `update` changes nothing, and `explain` says where each value came from, as
  it does for a config opened from a store.
- Only Rust has it so far. Bun, Node, Python and Go reach the config engine
  through a store, so they open one to read their settings.

## Limits

| What              | Limit                      |
|-------------------|----------------------------|
| a config's name   | `[a-z0-9][a-z0-9_-]{0,63}` |
| a setting's path  | 256 bytes                  |
| a setting's value | 16 KiB of JSON             |
| a file            | 1 MiB                      |

## See also

- [Config](README.md): the engine's overview.
- [Secrets and required settings](secrets.md).
- [Settings](../../handbook/chat/settings.md) in the Handbook.
