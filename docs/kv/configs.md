# Configs

A config holds your application's settings in the KV engine. It starts from
defaults in your code, adds your files and environment variables in the order
you pass them, and keeps the changes you make at run time. Every change reaches
every process that uses the store at once, without a restart.

## Read the settings

```ts
import { fromEnv, secret } from '@tinyshed/tinystore'
import file from './config.yaml'

const config = await store.kv.config('app', {
	port: 8080,
	origins: ['localhost'],
	db: { url: secret('DATABASE_URL'), pool: 10 },
	limits: { rps: 100 },
}, file, fromEnv('APP'))

config.value.port   // 8080, or the file's port, or APP_PORT
config.value.db.url // from DATABASE_URL
```

```python
import tomllib
from dataclasses import dataclass, field

from tinystore import from_env, secret


@dataclass
class Db:
    url: str = secret("DATABASE_URL")
    pool: int = 10


@dataclass
class Limits:
    rps: int = 100


@dataclass(kw_only=True)
class Settings:
    port: int = 8080
    origins: list[str] = field(default_factory=lambda: ["localhost"])
    db: Db
    limits: Limits = field(default_factory=Limits)


with open("config.toml", "rb") as f:
    file = tomllib.load(f)

config = await store.kv.config("app", Settings, file, from_env("APP", ".env"))

config.value.port  # 8080, or the file's port, or APP_PORT
config.value.db.url  # from DATABASE_URL
```

```go
type DB struct {
	URL  string `json:"url" env:"DATABASE_URL" secret:"true" required:"true"`
	Pool int    `json:"pool"`
}

type Limits struct {
	RPS int `json:"rps"`
}

type Settings struct {
	Port    int      `json:"port"`
	Origins []string `json:"origins"`
	DB      DB       `json:"db"`
	Limits  Limits   `json:"limits"`
}

var file map[string]any
err = yaml.Unmarshal(text, &file) // the YAML library you use

state, err := kv.Open(ctx, store, kv.Options{})
config, err := kv.OpenConfig[Settings](ctx, state, "app",
	kv.Defaults(Settings{Port: 8080, Origins: []string{"localhost"}, DB: DB{Pool: 10}, Limits: Limits{RPS: 100}}),
	kv.Defaults(file),
	kv.FromEnv("APP", ".env"))

port := config.Get().Port // 8080, or the file's port, or APP_PORT
```

Reading a config is a memory read, so you can read it on every request.

## Where values come from

Each layer goes over the one before it, in the order you pass them:

1. **The defaults** in your code.
2. **Each layer you pass**: a file's values, such as a YAML or TOML file that
   you parse yourself, and the environment with `fromEnv` (`from_env`,
   `kv.FromEnv`).
3. **The changes** that `update` stored, over all of them.

In the example above, the file goes over the defaults, and the environment
goes over the file. Pass `fromEnv` before the file, and the file wins. A config
reads no environment variable unless you pass `fromEnv`.

Settings can be grouped in nested objects, to any depth. Each setting reads the
variable with its path in upper snake case, after the prefix:

| Setting                              | `fromEnv('APP')` reads | `fromEnv()` reads |
|--------------------------------------|------------------------|-------------------|
| `port`                               | `APP_PORT`             | `PORT`            |
| `db.pool`                            | `APP_DB_POOL`          | `DB_POOL`         |
| `limits.maxRps`                      | `APP_LIMITS_MAX_RPS`   | `LIMITS_MAX_RPS`  |
| `db.url`, a `secret('DATABASE_URL')` | `DATABASE_URL`         | `DATABASE_URL`    |

A variable is read according to the setting's type: a number, `true` or
`false`, a duration such as `1h30m`, a list such as `a.com,b.com`, or JSON. A
variable that can't be read as its type fails when the config opens, with the
variable's name in the error. The error lists every such variable at once, so
one restart shows all the typos:

```text
config app: APP_PORT: "eighty" is no number
APP_SHUTDOWN_TIMEOUT: "5 sec" is no duration
```

Any setting can also come from a file: `APP_DB_PASSWORD_FILE=/run/secrets/db`
reads `db.password` from that file, the way Docker and Kubernetes pass
secrets. The last newline of the file is removed. Setting both `APP_DB_PASSWORD`
and `APP_DB_PASSWORD_FILE` is an error, because one of them would be ignored.

To read `.env` files, pass them after the prefix: `fromEnv('APP', '.env')`. The
process's own variables win over the files, and a missing file is skipped, so
the same code runs in production without a `.env` file. Bun also loads `.env`
by itself.

In Go, `kv.FromLookup("APP", lookup)` reads the variables through your own
function instead of the process's environment. Use it when your program passes
its environment around explicitly, and in tests that shouldn't depend on the
machine.

## Secrets and required settings

```ts
import { fromEnv, required, secret } from '@tinyshed/tinystore'

const config = await store.kv.config('app', {
	db: { url: secret('DATABASE_URL'), pool: 10 }, // required
	sentryDsn: secret('SENTRY_DSN', ''),           // optional, empty by default
	region: required(),                            // a string
	workers: required(Number),
}, fromEnv('APP'))
```

```python
@dataclass(kw_only=True)
class Settings:
    db_url: str = secret("DATABASE_URL")  # required
    sentry_dsn: str = secret("SENTRY_DSN", default="")  # optional, empty by default
    region: str  # no default: required
    workers: int


config = await store.kv.config("app", Settings, from_env("APP"))
```

```go
type Settings struct {
	DatabaseURL string `json:"databaseUrl" env:"DATABASE_URL" secret:"true" required:"true"`
	SentryDSN   string `json:"sentryDsn" env:"SENTRY_DSN" secret:"true"`
	Region      string `json:"region" required:"true"`
	Workers     int    `json:"workers" required:"true"`
}

config, err := kv.OpenConfig[Settings](ctx, state, "app", kv.FromEnv("APP"))
```

A secret comes from its variable, a file or its default, and never from
`update`. `update` refuses to change it, so a secret is never stored in
`kv.db`, and `sources()` shows its value as `***`. Without a variable's name, a
secret reads the prefix and its path like any other setting.

If no layer gives a required setting a value, the config doesn't open. It
fails with an invalid error (`ErrInvalid`, `InvalidError`) that names the
variable to set:

```text
config app: region is required: set APP_REGION
```

An empty string counts as missing, so a `.env` line like `DATABASE_URL=` fails
too. In Go, the zero value of the field's type counts as missing, as Go
validators treat it. A secret without a default is required. In Python, a
field without a default is required, and `kw_only=True` lets those fields come
in any order. `update` refuses to empty a required setting.

## Settings the operator controls

```ts
import { fixed, fromEnv } from '@tinyshed/tinystore'

const config = await store.kv.config('app', {
	addr: fixed(':8080'),   // only the defaults, a file or APP_ADDR set it
	instanceName: 'notes',  // the interface may change it
}, fromEnv('APP'))
```

```python
from tinystore import fixed, from_env


@dataclass
class Settings:
    addr: str = fixed(":8080")  # only the defaults, a file or APP_ADDR set it
    instance_name: str = "notes"  # the interface may change it


config = await store.kv.config("app", Settings, from_env("APP"))
```

```go
type Settings struct {
	Addr         string `json:"addr" fixed:"true"` // only the defaults, a file or APP_ADDR set it
	InstanceName string `json:"instanceName"`      // the interface may change it
}

config, err := kv.OpenConfig[Settings](ctx, state, "app", kv.Defaults(Settings{Addr: ":8080"}), kv.FromEnv("APP"))
```

Some settings belong to whoever runs the program, such as the address to
listen on or a TLS certificate's path. Mark them fixed: `update` refuses to
change a fixed setting, a stored value of it is skipped, and `sources()` shows
its value and where it came from, such as `env APP_ADDR`. Unlike a secret, a
fixed setting stays visible. A fixed setting without a default is required:
`fixed(required(Number))` in Bun, `fixed()` in Python.

## Change a setting at run time

```ts
await config.update({ limits: { rps: 200 } }) // stored, and seen everywhere at once
await config.reset('limits.rps')              // back to the value from the layers below
await config.reset('limits')                  // every field of limits
```

```python
await config.update({"limits": {"rps": 200}})  # stored, and seen everywhere at once
await config.reset("limits.rps")  # back to the value from the layers below
await config.reset("limits")  # every field of limits
```

```go
err = config.Update(ctx, func(s *Settings) { s.Limits.RPS = 200 }) // stored, and seen everywhere at once
err = config.Reset(ctx, "limits.rps")                              // back to the value from the layers below
err = config.Reset(ctx, "limits")                                  // every field of limits
```

`update` changes the fields it names and keeps the rest of their group. A
change is stored field by field. If you later change a default in your code,
every field that nobody changed at run time gets the new default after the
next deploy.

A change is checked before it is stored, and a change that fails the check
stores nothing. Add your own check with `validate(schema)` in Bun, which takes
a zod, valibot or other Standard Schema, with `validate=` in Python, or with
`kv.Validate(fn)` in Go:

```ts
import { validate } from '@tinyshed/tinystore'

const config = await store.kv.config('app', defaults, file, fromEnv('APP'), validate(SettingsSchema))
```

```python
config = await store.kv.config("app", Settings, file, from_env("APP"), validate=check_settings)
```

```go
config, err := kv.OpenConfig[Settings](ctx, state, "app", kv.Defaults(defaults), kv.Validate(checkSettings))
```

## React to changes

```ts
const stop = config.watch(settings => server.setRateLimit(settings.limits.rps))
```

```python
stop = config.watch(lambda settings: server.set_rate_limit(settings.limits.rps))
```

```go
for settings := range config.Watch(ctx) {
	server.SetRateLimit(settings.Limits.RPS)
}
```

A watcher gets the current settings first, and then the new settings after
every change. A change made in one process reaches the watchers of every
process that uses the store. A watcher that falls behind skips to the latest
settings.

## When a stored value no longer fits

If you change a setting's type in your code, a value stored earlier may no
longer fit it. That value is skipped and logged, and the program still starts.
A stored value of a setting that is now a secret is skipped too. `sources()`
(`Sources()` in Go) lists where each setting's value came from, and why a
stored value was skipped.

## Limits and defaults

|                                |                |
|--------------------------------|----------------|
| A setting's path               | 256 bytes      |
| A setting's value              | 16 KiB of JSON |
| All stored changes of a config | 256 KiB        |

A config keeps its changes in the KV engine's file, `kv.db`. A program that
uses KV for nothing else still opens that file for its config.

## See also

- [kv/README.md](../../kv/README.md#config): the full contract of configs.
