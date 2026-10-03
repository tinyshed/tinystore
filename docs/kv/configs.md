# Configs

A config holds your application's settings. It starts from defaults in your
code, adds values from a file and from environment variables, and keeps the
changes you make at run time. Every change reaches every process that uses the
store at once, without a restart.

## Read the settings

```ts
const config = await store.kv.config('app', {
	port: 8080,
	origins: ['localhost'],
	limits: { rps: 100 },
	databaseUrl: '',
}, { prefix: 'APP', secret: ['databaseUrl'] })

config.value.port // 8080, or the value of APP_PORT
```

```python
from dataclasses import dataclass, field


@dataclass
class Limits:
    rps: int = 100


@dataclass
class Settings:
    port: int = 8080
    origins: list[str] = field(default_factory=lambda: ["localhost"])
    limits: Limits = field(default_factory=Limits)
    database_url: str = ""


config = await store.kv.config("app", Settings, prefix="APP", env_file=".env", secret=["database_url"])

config.value.port  # 8080, or the value of APP_PORT
```

```go
type Limits struct {
	RPS int `json:"rps"`
}

type Settings struct {
	Port        int      `json:"port"`
	Origins     []string `json:"origins"`
	Limits      Limits   `json:"limits"`
	DatabaseURL string   `json:"databaseUrl" secret:"true"`
}

state, err := kv.Open(ctx, store, kv.Options{})
config, err := kv.OpenConfig[Settings](ctx, state, "app",
	kv.Defaults(Settings{Port: 8080, Origins: []string{"localhost"}, Limits: Limits{RPS: 100}}),
	kv.FromEnv("APP", ".env"))

port := config.Get().Port // 8080, or the value of APP_PORT
```

Reading a config is a memory read, so you can read it on every request.

## Where values come from

Each layer overrides the one before it:

1. **Defaults** from your code.
2. **A file**, such as a YAML or TOML file that you parse yourself and pass in:
   `file` in Bun and Python, a second `kv.Defaults(…)` in Go.
3. **Environment variables**, including a `.env` file.
4. **Changes** made at run time with `update`, which are stored in KV.

An environment variable is named after the setting's path in upper snake
case, after the prefix: `limits.rps` becomes `APP_LIMITS_RPS`. A variable is
read according to the setting's type: a number, `true` or `false`, a duration
such as `1h30m`, a list such as `a.com,b.com`, or JSON. A variable that can't be
read as its type fails when the config opens, with the variable's name in the
error.

To use a variable with another name, map it: `env: { databaseUrl: 'DATABASE_URL' }`
in Bun, `env={"database_url": "DATABASE_URL"}` in Python, or the tag
`env:"DATABASE_URL"` in Go. Bun loads `.env` by itself. Node doesn't, so run
it with `node --env-file=.env`. In Python, pass `env_file`, and in Go the
files to `kv.FromEnv`.

## Change a setting at run time

```ts
await config.update({ limits: { rps: 200 } }) // stored, and seen everywhere at once
await config.reset('limits.rps')              // back to the value from the layers below
```

```python
await config.update({"limits": {"rps": 200}})  # stored, and seen everywhere at once
await config.reset("limits.rps")               # back to the value from the layers below
```

```go
err = config.Update(ctx, func(s *Settings) { s.Limits.RPS = 200 }) // stored, and seen everywhere at once
err = config.Reset(ctx, "limits.rps")                              // back to the value from the layers below
```

A change is stored field by field. If you later change a default in your code,
every field that nobody changed at run time gets the new default after the
next deploy.

A change is checked before it is stored, and a change that fails the check
stores nothing. Add your own check with `schema` in Bun, `validate` in Python
or `kv.Validate` in Go.

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

## Secrets

A setting marked as secret only comes from defaults and the environment.
`update` refuses to change it, so a secret is never stored in `kv.db`. Mark
secrets with `secret` in Bun and Python, and the tag `secret:"true"` in Go.

## When a stored value no longer fits

If you change a setting's type in your code, a value stored earlier may no
longer fit it. That value is skipped and logged, and the program still starts.
`sources()` (`Sources()` in Go) lists where each setting's value came from,
and why a stored value was skipped.

## Limits and defaults

| | |
|---|---|
| A setting's path | 256 bytes |
| A setting's value | 16 KiB of JSON |
| All stored changes of a config | 256 KiB |

## See also

- [kv/README.md](../../kv/README.md#config): the full contract of configs.
