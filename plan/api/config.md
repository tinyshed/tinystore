# config: the API book

Accepted by the owner on 13 October 2026, after a newcomer check, a blind
comparison, an independent review and three outside readings (below). A program's settings as
one typed value: declared in code, declared again by files and the
environment, and, for the settings the declaration marks, changed while the
program runs and seen by every process at once. It replaces Go's
`kv.OpenConfig` and the `store.kv.config` of Bun and Python at `e81a050`,
whose DX the owner called bad: an options object whose sources nobody could
rank, then layers ranked by the order of their arguments. The owner's idea
is the shape of this one: a declaration in layers, each declaring again what
the ones before it gave, spelled as each language spells it. [dx.md](../dx.md)
has the rules it follows. TypeScript comes first; Python, Go and Rust follow.
Nothing of it is built.

## What a newcomer learns

| Concept                            | In one line                                                                                   |
|------------------------------------|-----------------------------------------------------------------------------------------------|
| config                             | a program's settings as one typed value, read from memory                                     |
| the declaration                    | the first layer: every setting's name, type and default                                       |
| layer                              | an object in code, a file or the environment; each declares again what came before            |
| `file`                             | a TOML, YAML or JSON file                                                                     |
| `env`                              | environment variables: `APP_LIMITS_RPS` for `limits.rps`                                      |
| `live`                             | a setting that may change while the program runs, kept in the store for every process         |
| `secret`, `required`               | a setting that prints as `***` and is never kept; one that a layer must give                  |
| `int`, `oneOf`, `duration`, `list` | the rules of one value the core checks in every layer: a range, a set, a span, a list's items |
| `explain`                          | every setting, its value, the layer that gave it and what it went over                        |

## Declare a config

```ts
import { env, file, live, required, secret } from '@tinyshed/tinystore'

const config = await store.config('app', [
  {
    port: 8080,
    origins: ['localhost'],
    db: { url: secret(String), pool: 10 },
    limits: { rps: live(100) },
    region: required(String),
  },
  process.env.NODE_ENV === 'production' && { limits: { rps: 1000 } },
  file('app.yaml'),
  file('app.local.yaml', { optional: true }),
  env('APP', { vars: { port: 'PORT', db: { url: 'DATABASE_URL' } } }),
])
```

```python
@dataclass
class Db:
    url: Secret = secret()
    pool: int = 10

@dataclass
class Limits:
    rps: int = live(100)

@dataclass(kw_only=True)
class Settings:
    port: int = 8080
    origins: list[str] = field(default_factory=lambda: ["localhost"])
    db: Db = field(default_factory=Db)
    limits: Limits = field(default_factory=Limits)
    region: str  # no default: required

config = await store.config("app", [
    Settings,
    {"limits": {"rps": 1000}} if production else None,
    file("app.yaml"),
    file("app.local.yaml", optional=True),
    env("APP", vars={"port": "PORT", "db.url": "DATABASE_URL"}),
])
```

```go
type Settings struct {
	Port    int      `json:"port"`
	Origins []string `json:"origins"`
	DB      DB       `json:"db"`
	Limits  Limits   `json:"limits"`
	Region  string   `json:"region" config:"required"`
}

type DB struct {
	URL  tinystore.Secret `json:"url" config:"required"`
	Pool int              `json:"pool"`
}

type Limits struct {
	RPS int `json:"rps" config:"live"`
}

config, err := tinystore.Config(store, "app",
	Settings{Port: 8080, Origins: []string{"localhost"}, DB: DB{Pool: 10}, Limits: Limits{RPS: 100}},
	tinystore.File("app.yaml"),
	tinystore.File("app.local.yaml", tinystore.Optional()),
	tinystore.Env("APP", tinystore.Var("port", "PORT"), tinystore.Var("db.url", "DATABASE_URL")),
)
```

```rust
use tinystore::config::{Env, File, Live, Required, Secret};

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
struct Settings {
    port: u16,
    origins: Vec<String>,
    db: Db,
    limits: Limits,
    region: Required<String>,
}

#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct Db {
    url: Secret<String>,
    pool: u32,
}

#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct Limits {
    rps: Live<u32>,
}

impl Default for Settings {
    fn default() -> Self {
        let db = Db { pool: 10, ..Db::default() };
        let limits = Limits { rps: Live::new(100) };
        Settings { port: 8080, origins: vec!["localhost".into()], db, limits, region: Required::default() }
    }
}

let config = store
    .config("app", Settings::default())
    .file("app.yaml")
    .file(File::optional("app.local.yaml"))
    .env(Env::prefix("APP").var("port", "PORT").var("db.url", "DATABASE_URL"))
    .open()?;
```

- The layers are read in the order written, and for each setting the last
  layer that gives it wins, as in `{ ...a, ...b }`: here `limits.rps` is 100,
  1000 in production, then what app.yaml, app.local.yaml and
  `APP_LIMITS_RPS` say, in that order.
- The name, `'app'`, is the config's identity in the store: programs that
  share a store and a name share their changes. It is
  `[a-z0-9][a-z0-9_-]{0,63}`, as a bucket's.
- A config opens once its layers are read, or fails with one error that
  lists every problem (below).
- Each language spells the list its own way: an array in TypeScript, as
  ESLint's flat config is one; a list in Python; options in order in Go; the
  order of the builder's calls in Rust.

## Layers

### The declaration and objects in code

- The first layer declares every setting: its name, its type and its
  default. A setting's type is its default's: a number, text, a boolean, a
  list, or a group of settings, to any depth.
- An object after it declares again settings the declaration has; a key it
  does not have is `invalid`, naming it, and TypeScript refuses it as it is
  written.
- `false`, `null` and `undefined` in the list are skipped, so that a layer
  can depend on a condition. In an object, a key set to `undefined` declares
  nothing: `{ region: process.env.FLY_REGION }`.
- Python's first layer is the dataclass itself, and its later objects are
  dictionaries. Go and Rust compute their declaration in code, so the layers
  after it are files and the environment.
- In Rust the declaration is a value of a `serde` type that is `Clone`, its
  defaults where `#[serde(default)]` finds them; a `Duration` is a span, and a
  map declared empty, a `HashMap` or a `BTreeMap`, is one setting that a
  layer gives whole. A path is 256 bytes at most.

### Files

- `file(path)` reads TOML, YAML or JSON by the extension: `.toml`, `.yaml`
  or `.yml`, `.json` or `.jsonc`. JSON takes comments, `//` and `/* */`, and
  a comma after the last item, as `tsconfig.json` and VS Code's settings do;
  any JSON reads as it did. A relative path is the program's working
  directory's.
- A file that does not exist is `invalid`, naming its path, unless it is
  `optional`: then it declares nothing, and `explain` says it was absent.
- A key the declaration does not have, or a value of another type, is
  `invalid`, naming the file and the setting.
- A file is read when the config opens.

### The environment

- `env('APP')` reads `APP_` and each setting's path in upper snake case:
  `port` is `APP_PORT`, `limits.rps` `APP_LIMITS_RPS`, `db.maxIdle`
  `APP_DB_MAX_IDLE`. Two settings that would read one variable are
  `invalid`.
- A variable is read as its setting's type: a number in decimal, `true` or
  `false`, a list as `a,b,c` or as JSON, a map as JSON, text as it is; a
  group is given by its settings' variables.
- `vars` gives a setting a variable of its own in place of the prefixed one,
  such as the `PORT` and `DATABASE_URL` platforms set.
- `APP_DB_URL_FILE=/run/secrets/db` reads the setting from that file, its
  last newline removed, as Docker and Kubernetes pass secrets; both
  `APP_DB_URL` and `APP_DB_URL_FILE` set is `invalid`.
- A variable with the prefix that names no setting is logged once at open
  and listed by `explain`: `APP_PROT=9000` is said, not ignored.
- `files: ['.env']` reads `.env` files for the variables the process lacks;
  one that does not exist is skipped. Bun reads `.env` by itself.
- `from: { APP_PORT: '9000' }` reads these variables in place of the
  process's, for tests.

## Live settings

```ts
limits: { rps: live(100) },

await config.update({ limits: { rps: 200 } })  // kept; every handle and process sees it
await config.update({ limits: { rps: 300 } }, { ifVersion: shown })  // conflict if a change came since
await config.reset('limits.rps')               // the layers give it again
await config.reset()                           // every change of the config
```

```python
await config.update({"limits": {"rps": 200}})
await config.reset("limits.rps")
```

```go
err = config.Update(func(s *Settings) { s.Limits.RPS = 200 })
err = config.Reset("limits.rps")
```

```rust
config.update(|s| *s.limits.rps = 200)?;
config.reset("limits.rps")?;
```

- A setting marked `live` may change while the program runs, and nothing
  else may: `update({ port: 9000 })` is `invalid`, and TypeScript refuses it
  as it is written. Settings that only take effect at start, an address or a
  path, are never changed under a running program by mistake.
- A change is kept in the store under the config's name and goes over every
  layer until `reset`: a live setting is its last change, or what the layers
  give. It is the strongest layer on purpose, since `live(100)` lets whoever
  runs the program outrank the code, the files and the environment. A change
  survives a restart, and a deploy that sets `APP_LIMITS_RPS` does not undo
  it: `explain` shows the change over the variable, with when it was made
  and its version, and `reset` lets the layers give the setting again.
- Every handle of the config sees a change before `update` returns, and
  every process sharing the store through a server as soon as it is
  committed.
- `update` keeps the settings it names, and in Go and Rust the ones its
  function changed, never the whole value; all of them or none. Two updates
  of different settings both stay, from two processes too: `{ rps: 200 }`
  and `{ burst: 500 }` make `{ rps: 200, burst: 500 }`, and the value they
  make is validated whole.
- Every change moves the config's `version`. With `ifVersion`, an update
  that another change came before is `conflict`, so that an admin page does
  not overwrite a change it never showed.
- A kept change of a setting the program declares that no longer fits it,
  the setting no longer live, now a secret, of another type or refused by
  `validate`, is ignored and not removed: the open warns about it in the
  log, `explain` lists it with why, it applies again once it fits, and
  `reset` of its path removes it. `strict: true` fails the open instead, for
  a config whose defaults are worse than not starting.
- A kept change of a setting the program does not declare may be another
  program's, so it is neither applied nor warned about; `explain` lists it
  apart, and `reset` of its path removes it.

## Secrets and required settings

```ts
db: { url: secret(String), pool: 10 },   // a layer must give it
sentry: { dsn: secret('') },             // empty unless a layer gives it
region: required(String),                // also Number and Boolean
```

- A secret prints as `***` wherever it goes: `console.log`,
  `JSON.stringify`, a template, Python's `repr`, Go's `fmt`, Rust's `Debug`
  and serde, the logs and `explain`. `reveal()` gives its text, where the
  program hands it to what needs it:
  `postgres(config.value.db.url.reveal())`.
- A secret is never kept, so `update` cannot take it, and only a layer gives
  it: a file, a variable or its `_FILE`.
- An error about a secret names its variable and never quotes its text.
- The tests pass a secret through what prints a value by accident: a nested
  value, `JSON.stringify` of the whole config, a spread, a template,
  `util.inspect`, `structuredClone` and an error. Hiding it from print is not
  access control, which the server's tokens are.
- A required setting has no default: a layer gives it, or the config does
  not open, naming what would: `region is required: set APP_REGION, or region
  in app.yaml`. An empty text counts as missing.
- `secret(String)` is required; `secret('')` has a default. Python's field
  without a default is required, and `secret()` is a required secret. Go
  marks it with `config:"required"`, and its `tinystore.Secret` type is the
  secret. Rust's `Required<T>` and `Secret<T>` hold no value by default.
- The value a declaration gives is never counted as given: Go's zero value
  and Rust's `Default` do not satisfy `required`.
- The marks nest one way, `live` outermost, around a value, a rule or
  `required(…)`: `live(100)`, `live(int(100, { min: 1 }))`, and
  `live(required(String))`, a setting a layer must give at start that may
  change later. A secret is never live, so `live(secret(…))` does not
  compile and the core refuses it, as it refuses `required(secret(…))`,
  since `secret(String)` is required already, and a secret of anything but
  text. Rust's `Live<Required<T>>` is the same, and `Live<Secret<T>>` is
  refused at open.
- Read, `required` and `live` leave the type: `required(String)` reads as
  `string`, `live(int(100))` as `number`, and only a secret reads as
  `Secret`, whose `reveal()` gives its text.

## Rules

```ts
port: int(8080, { min: 1, max: 65535 }),
level: oneOf('info', ['debug', 'warn', 'error']),            // 'info' | 'debug' | 'warn' | 'error'
timeout: duration('30s'),
origins: ['localhost'],                                      // a list of text
peers: list(String),                                         // a list with nothing by default
hosts: list(String, { default: ['localhost'], min: 1, max: 20 }),
upstreams: list({ host: required(String), port: int(80) }, { min: 1 }),
workers: required(int({ min: 1 })),                          // a rule without its default
limits: { rps: live(int(100, { min: 0 })) },
```

```text
config app: APP_LEVEL: "verbose" is not one of info, debug, warn, error
config app: app.yaml: port: 70000 is above 65535
config app: app.yaml: upstreams[0]: host is required
```

- A setting's type is its default's, and four rules say more about one
  value: `int`, a whole number in a range; `oneOf`, one of a set of texts;
  `duration`, a span; `list`, the type of a list's items and how many it
  holds. Every layer and every change is checked against them, and an error
  names the variable or the file.
- Every rule takes its default first, as `live` and `secret` do: `oneOf`
  its default and then the other values it allows, so that a new default is
  written once. TypeScript reads the set as its own type, not `string`.
- A rule without its default goes in `required`, as a type does:
  `required(int({ min: 1 }))`, `required(oneOf(['eu', 'us']))`.
- `list`'s `min` and `max` count its items, and its default is in its
  options, since its first argument is its items' type. A list written as a
  value, `['localhost']`, needs no `list`.
- A duration is written as the SDK writes one, `'1h30m'`, `'250ms'`, of
  `w`, `d`, `h`, `m`, `s` and `ms`, and kept as milliseconds; one past what
  64 bits of milliseconds hold is `invalid`. A program reads it as its
  language's span: TypeScript's `DurationText`, which every call of the SDK
  takes and `ms()` turns into a number, Rust's `Duration`, Go's
  `time.Duration`, Python's `timedelta`.
- A list is one setting, its items of one type: a later layer and a change
  give it whole, never merged by index. A variable gives one as `a,b,c` or as JSON, and a list of
  groups only as JSON. Its items' type is its default's items', or what
  `list` names when it has none. An item of groups is checked as the
  declaration is: a setting of it without a default is required in every
  item, and one with a default takes it where an item leaves it out.
- What relates two settings, `min` below `max`, is `validate`'s; the rules
  are what one value says of itself. A format, `url`, `email` or a pattern,
  is `validate`'s too.
- Each language writes them its own way and the core reads one kind of rule:
  Rust's types, `u16`, `NonZeroU32`, an `enum`, `Duration`; Go's
  `config:"min=1,max=65535"` and `config:"oneof=debug info warn error"`
  beside `time.Duration`; Python's `Literal[…]`, `Annotated[int, Range(1,
  65535)]` and `timedelta`.

## Read

```ts
config.value.limits.rps          // 100, or what a layer or a change gives
config.value.db.url.reveal()
config.version                   // the version of the changes the value holds
```

```python
config.value.limits.rps
```

```go
settings := config.Get() // a copy
```

```rust
let settings = config.get(); // a Snapshot<Settings>, its version in settings.version()
```

- A read is a read of memory: read the value where it is used, on every
  request.
- `value` is the settings as they are now, a frozen object that a change
  replaces whole: a reference kept keeps what it had, and
  `const { rps } = config.value.limits` does not follow a change; `watch`
  does. Rust's `get()` is a `Snapshot` of the same, which reads as the
  settings, and Go's `Get()` a copy.
- `value` and `version` change together, so an admin page that shows one and
  sends the other with `ifVersion` changes what it showed.

## Watch

```ts
const stop = config.watch((next, previous) => limiter.setRate(next.limits.rps))
stop()
```

```python
async for settings in config.watch():
    limiter.set_rate(settings.limits.rps)
```

```go
for s := range config.Watch(ctx) {
	limiter.SetRate(s.Limits.RPS)
}
```

```rust
let watching = config.watch(move |next, _previous| limiter.set_rate(*next.limits.rps)); // stops when dropped
```

- The function gets the settings now, with no `previous`, then after every
  change, made in this process or another; one that falls behind gets the
  newest. The first call lets a function that applies a setting do nothing
  else at start; one that answers changes alone returns while `previous` is
  undefined.
- Every handle and every watcher of a config in one process share one
  stream: the store sends a change once, and the process hands the new
  value to each of them.
- A watcher sees a change whole and once it is committed, never a value half
  changed.
- A handle whose connection was lost reads the config again when it is
  back, and calls its watchers if the config changed meanwhile.
- In Rust the function runs on the thread that made the change, once it is
  committed, and must not block; a watcher that changes the config itself is
  called again after it returns, with each version in order. Rust's reset of
  every change is `reset_all()`.

## Explain

```ts
console.table(config.explain())
```

```text
path        value  from                                   over
port        9000   PORT                                   8000 app.yaml, 8080 the declaration
db.url      ***    DATABASE_URL
limits.rps  200    a change, 12 October 14:03, version 7  1000 layer 2, 100 the declaration
region      eu     app.yaml
```

- Every setting, its value, the layer that gave it and the values it went
  over, newest first. An object in code is named by its place in the list.
- After the settings, what was read and not used, each with why: an
  optional file that was absent, a variable with the prefix that names no
  setting, a kept change left out. The open logs them once.

## Validate

```ts
const config = await store.config('app', layers, { validate: Settings }) // zod, valibot, or a function
```

```rust
let config = store
    .config("app", Settings::default())
    .validate(|s| if *s.limits.rps == 0 { Err("limits.rps is 0".into()) } else { Ok(()) })
    .open()?;
```

- `validate` sees the whole value at open and before a change is kept; a
  change it refuses keeps nothing. It takes a Standard Schema, such as zod's
  or valibot's, or a function that throws.
- At open, the kept changes are applied in the order they were made: each
  that leaves the value valid stays, and each that does not is ignored, as
  one that no longer fits is. A stricter release still starts, unless the
  config is `strict`.

## Errors

Every error is `invalid`, one at open listing every problem:

```text
config app: APP_PORT: "eighty" is not a number
config app: region is required: set APP_REGION, or region in app.yaml
config app: app.yaml: prot: no such setting
config app: app.local.yaml: no such file; optional lets it be absent
config app: APP_DB_URL and APP_DB_URL_FILE are both set
```

and one a change:

```text
config app: port is not live
config app: limits.rps: "fast" is not a number
config app: reset limits.rsp: no such setting, and no change of it is kept
```

An update whose `ifVersion` another change came before is `conflict`, and a
call after the store closed is `closed`.

## Bounds

|                                          |                |
|------------------------------------------|----------------|
| A setting's path                         | 256 bytes      |
| A setting's value, kept or in a variable | 16 KiB of JSON |
| What a config keeps of its changes       | 256 KiB        |
| A file                                   | 1 MiB          |

## Through a server

- Through a private child, a sidecar or a remote server, the SDK reads its
  own files and variables, the prefix's, those `vars` name and their `_FILE`
  files, and sends them with the declaration. The server reads neither its
  own environment nor its files for a client's config, since they belong to
  the program and not to the server.
- A process follows a config's changes on one stream, however many handles
  and watchers it holds, and gives each of them every change: the SDK opens a
  config once for the same name, declaration, layers and `validate`, every
  call of `store.config` for it sharing that handle and its stream, and a
  declaration of its own opens one of its own.
- A change answers with the value it made, so that the program that made it
  sees it at once whatever its stream carries; one made on a version another
  change passed is `conflict`, and the SDK makes it again on the newer value,
  its `validate` run again, unless `ifVersion` was given.
- A value another program's change made that this program's `validate`
  refuses leaves the value before it, says so on standard error, and
  `explain` lists it.
- In TypeScript `explain()` is a list of lines for `console.table`, each its
  path, value, layer and what it went over, and its `notes` property what
  was read and not used; a number written as a value, `port: 8080`, is any
  number, and `int` a whole one.
- A token that only reads opens, reads and watches a config; a change takes
  a token that writes.

### One config, several programs

Through a server, several programs open one config, each with its own
declaration, and may be two versions of one program during a deploy, or two
programs that read different settings of it. Nothing joins their
declarations into one; the store keeps the changes, and each program reads
them through its own declaration.

```text
program A, version 1:  limits: { rps: live(int(100, { max: 1000 })) }
program B, version 2:  limits: { rps: live(int(100, { max: 500 })) }

A: update({ limits: { rps: 800 } })
config app: limits.rps: 800 is above 500, as another program that has the config open declares
```

- A declaration reaches the core as data: each setting's path, type,
  default, rules, and whether it is live or a secret. The core keeps it
  beside the handle while the handle is open.
- A change is checked against the declaration of every handle that has the
  config open when it comes, and one any of them refuses keeps nothing: a
  value out of a range one declares, a setting one declares a secret or not
  live. So the programs that have the config open when a change comes read
  it alike.
- `validate`, a function of the program, runs in the program that makes the
  change, before it is sent; the core checks what is data, and the function
  what is not.
- A declaration answers for the settings it declares. A kept change of a
  setting a program does not declare is another program's: that program
  neither applies it nor warns about it, and `explain` lists it apart.
- A program that opens later, with stricter rules, may read a change apart:
  it ignores a kept change its declaration refuses and warns about it, as
  above, until it fits or is reset, so that a deploy that tightens a rule
  still starts; `strict` makes it refuse to open instead. While the two
  versions run, the earlier one reads the change and the later its layers,
  and `explain` says so in each.
- No program's `validate` is run for another's change when it is made: a
  rule only a function knows holds the changes of the program that has it,
  and the rules that are data hold everyone's. A program whose `validate`
  refuses a change another made ignores it and says so, as it does at
  open.
- Two declarations that give one setting different types are said once in
  the log of the program that opens second; a change of that setting then
  fits neither, and the error says so.
- The store keeps the last declaration a config was opened with, so that
  `tinystore config` can show and check a config while no program runs.

## The logger's config

```ts
const log = store.logger('api', { level: 'info' })
const settings = await store.config('log', [{ level: live('info') }])
await settings.update({ level: 'debug' })   // every logger of the store, in every process
await settings.reset('level')               // each logger's own level again
```

```rust
let log = store.logger("api").level(Level::INFO).layer()?;
let settings = store.config("log", Log { level: Live::new("info".to_owned()) }).open()?;
settings.update(|log| *log.level = "debug".to_owned())?;
```

A store's logger reads its options from the config `log`, which it opens as
it is made and follows while it runs:

| Setting  | Allows                                          | Its default                                      |
|----------|-------------------------------------------------|--------------------------------------------------|
| `level`  | `trace`, `debug`, `info`, `warn`, `error`; live | the code's `level`, or `trace`: every level      |
| `format` | `pretty`, `json`, `off`                         | the code's `console`, or what its terminal chose |
| `time`   | `clock`, `full`, `off`                          | the code's `time`, or `clock`                    |

- The code's options are the declaration's defaults, and `LOG_LEVEL`,
  `LOG_FORMAT` and `LOG_TIME` the environment's layer over them, after the
  logger's prefix: `env('APP')` reads `APP_LOG_LEVEL`. They read as they
  did: a value none of them can mean is ignored and said once, and never
  refuses the config; a console the code turned off stays off.
- Only the level is live. A change reaches every logger of the store at
  once, in every process, over its code and its environment, until it is
  reset. A level no logger means is refused while one has the config open,
  and one kept is ignored and said, as a kept change that no longer fits is.
- `trace` is the level below `debug` that Rust's `tracing` logs at, and the
  default of a logger told none, so that it takes every line as it did; the
  store's own lines show from `info` up at it.
- The logger of the console alone has no store and no config, and reads its
  variables itself.
- An SDK's logger opens its config in the background, since a logger is
  made without waiting: a line logged before it is open takes the code's and
  the environment's level. A server older than the config engine leaves it
  there, said once.
- Its declaration lists each set its default first, as `oneOf` does, so
  that a refusal names the set as the logger that refused it declares it.

## Slices

1. The Rust core: the declaration from a serde value, with `Secret`,
   `Required` and `Live`; objects, files and the environment; changes kept
   in `kv.db`; `update`, `reset`, `watch`, `explain`, `validate`; the
   errors and bounds; GATES' config lines in Rust. The core parses every
   file, so that no program brings a parser and every language reads YAML
   alike: TOML through `toml`, YAML 1.2 through `yaml-rust2` without its
   encodings, JSON through serde_json once the core has taken out its
   comments and trailing commas, each replaced by spaces so that an error's
   line and column are the file's, behind a `config` feature that is a
   default as the engines are. The two crates add 373 KiB to a release
   build, 216 KiB to 589 KiB for a program that parses into a
   `serde_json::Value` (Windows 11, x86_64-pc-windows-msvc, Rust 1.99.0,
   `cargo build --release`, 13 October), beside the 7.3 MiB of
   `tinystore_ffi.dll`; `serde-saphyr` took 1.1 MiB alone, most of it the
   encodings of `encoding_rs`.
2. The wire and the Bun SDK: `protocol/config.wire`, the declaration as
   data, the SDK reading its files and variables, a change checked against
   every open declaration, and types that infer the value from the
   declaration, `oneOf`'s set among them, let `update` take live settings
   only and `vars` declared paths only.
3. The logger's config.

Python and Go come with their bindings.

## Later

- Files read again when they change, as Kubernetes updates a mounted
  ConfigMap.
- `tinystore config`: list, show and reset a config's changes, in phase 4.
- Who made a change, beside when.
- A change that expires, for an override during an incident:
  `update(…, { ttl: '1h' })`.
- The declaration's fingerprint kept with each change, so that `explain` says
  a change came from another version of the program. A warning whenever two
  versions declare a config differently would sound at every deploy; the
  check of each change against the declaration that reads it is where the
  harm is.
- The command line's flags as a layer.

## Newcomer check, 12 October

A Haiku agent that had read nothing about TinyStore read 15 call sites of the
first draft, in which the changes made at run time were a layer, `live()`,
placed in the list. It read the stack right: `APP_PORT` over app.yaml, an
`update` that the environment after `live()` went over, the two swapped when
`env` moved above `live()`, and an `update` refused without it. What it
doubted, and what changed:

| It asked                                                         | Change                                                         |
|------------------------------------------------------------------|----------------------------------------------------------------|
| whether `value` is a snapshot or follows changes                 | a frozen object that a change replaces; `watch` follows        |
| whether `watch` calls at once                                    | now, then after every change                                   |
| whether `reset` catches a typo                                   | a path with no setting and no kept change is `invalid`         |
| whether Rust's `update` keeps the whole struct and so freezes it | it keeps what the function changed                             |
| `APP_DB_POOL` or `APP_DB__POOL`                                  | one underscore; two settings reading one variable is `invalid` |
| how Rust's `watch`, a blocking iterator, stops                   | a function, and a guard that stops it when dropped             |
| whether a secret is hidden anywhere but `explain`                | it prints as `***` everywhere, and `reveal()` gives it         |

## Review, 12 October

Two more Haiku agents with no other context: one compared eight choices
blind, one reviewed the whole API as a developer choosing a library.

| They found                                                                                                            | Change                                                                                                     |
|-----------------------------------------------------------------------------------------------------------------------|------------------------------------------------------------------------------------------------------------|
| `live()` written last lets a change made a month ago beat a deploy's `PORT`, and its place decides two things at once | `live` marks a setting: only it changes at run time, its change over every layer; nothing else is kept     |
| two versions of a program that place `live()` apart rank one kept change two ways during a deploy                     | a change goes over every layer in every process; the list orders code, files and the environment alone     |
| a secret printed by `console.log`, `JSON.stringify`, `repr` and `fmt`                                                 | `***` everywhere and `reveal()`, as pydantic's `SecretStr` and Rust's `secrecy` do                         |
| a missing file starting the program on its defaults                                                                   | `invalid`, naming the path, unless `optional`                                                              |
| `APP_PROT=9000` ignored without a word                                                                                | logged once at open and listed by `explain`                                                                |
| a prefix taken from the config's name moves every variable when the name changes                                      | `env('APP')`, written                                                                                      |
| `secret()` untyped beside `required(String)`; a zero value or `Default` taken as given                                | `secret(String)`; Rust's `Required<T>` and `Secret<T>`, Go's `config:"required"`; a default is never given |
| Rust's secrets and required settings as paths in the builder, where a typo leaks                                      | types in the struct                                                                                        |
| `names` for a setting's own variable; Go's `Name` read as the config's                                                | `vars`, and Go's `Var`                                                                                     |
| two objects in code both called `code` by `explain`                                                                   | the declaration, and the others by their place                                                             |
| tests racing on the process's variables                                                                               | `from`                                                                                                     |
| a kept change the new `validate` refuses keeping the program from starting                                            | it opens without the changes, logs them, and `explain` lists them                                          |
| the config's name never explained                                                                                     | it is the config's identity in the store, shared by every program that opens it                            |

Kept: `explain`, which the blind comparison ranked first and the review
would call `sources`; the list over a fixed order, which the blind comparison
chose for the deploy above, since a change is no longer a layer; `required`.
Later: reading files again, the command that lists and resets changes, who
made a change.

## An outside reading, 12 October

The owner passed on a reading of the second draft, which approved its shape:
the layers in order, `live` on the setting, `secret` and `reveal()`,
`required`, `explain`. What it asked, and what changed:

| It asked                                                                 | Change                                                                                    |
|--------------------------------------------------------------------------|-------------------------------------------------------------------------------------------|
| a change made long ago still outranks a deploy's variable                | said on purpose, the strongest layer; `explain` gives its time and version; expiry, later |
| two updates of one group from two processes must both stay               | each keeps only the settings it names, and the value they make is validated whole         |
| an admin page must not overwrite a newer change                          | `version` and `ifVersion`, `conflict` when another change came first                      |
| a stale change dropped in silence is worse than an error                 | ignored, not removed: warned at open, listed, applied again once it fits; `strict` fails  |
| one bad change must not take the good ones with it                       | the changes apply in the order made, each kept that leaves the value valid                |
| a secret printed by a spread, a nested value, an error                   | the tests pass a secret through each; an error never quotes one                           |
| `watch` with the value before, never half a value, and a way to catch up | `(next, previous)`, a change whole once committed, read again after a lost connection     |

A second reading, of the rules, approved `int`, `oneOf`, `duration` and
`list` in the first slice, a list's length beside them, and `validate` for
what relates two settings:

| It asked                                                           | Change                                                                          |
|--------------------------------------------------------------------|---------------------------------------------------------------------------------|
| `oneOf` must not widen to `string`                                 | it gives TypeScript the set's own type                                          |
| a duration as a span, the same in every language                   | `DurationText`, `Duration`, `time.Duration`, `timedelta`; milliseconds, 64 bits |
| an item's field without a default must not become empty text       | required in every item, as in the declaration                                   |
| the server checks what is data, the program's function stays in it | so; and a change is checked against every open declaration                      |
| a string's length                                                  | `validate`'s, with formats and patterns, so that the rules stay four            |

The owner asked how programs that share a config through a server agree on
its declaration: they do not join it, and a change is checked against all
of them, above. A third reading, of that and of the rules' spelling:

| It asked                                                                  | Change                                                                                     |
|---------------------------------------------------------------------------|--------------------------------------------------------------------------------------------|
| `oneOf`'s default last, where every other rule takes it first             | `oneOf('info', ['debug', 'warn', 'error'])`                                                |
| `host: String` beside `required(String)`, two ways for one thing          | `required(String)` in a list's items too, and `required` around a rule without its default |
| a list's length                                                           | `list(String, { default, min, max })`, `min` and `max` counting items                      |
| "never read one change two ways" is not so for a program that opens later | the book says who reads it apart, and that `strict` refuses to open                        |
| another program's `validate` is never run                                 | said: a function's rule holds its own program's changes                                    |

`enum`, which the owner asked about, is a word JavaScript reserves, Rust's
keyword and Python's module, so `oneOf` stays. A fourth reading asked how
the marks nest, and the owner that a process keep one stream a config for
all its watchers; both are above, under Secrets and required settings and
under Watch.

Kept: `watch` calls at once, without the `immediate` flag asked for, since a
watcher that forgets the flag applies nothing until the first change, and a
call too many is the smaller harm; `previous` says which call is the first.
Later: the declaration's fingerprint kept with each change, a change that
expires.

## Was, in Go

| `e81a050`                                                                     | Now                                                           |
|-------------------------------------------------------------------------------|---------------------------------------------------------------|
| `kv.OpenConfig[T](ctx, state, name, …)`, `store.kv.config(name, defaults, …)` | `store.config(name, [declaration, …layers])`                  |
| `kv.Defaults(v)`                                                              | the declaration, the first layer                              |
| a file the program parsed and passed as a layer                               | `file(path)`, parsed by the core                              |
| `fromEnv(prefix, …files)`, `kv.FromEnv`                                       | `env(prefix, { files })`                                      |
| `kv.FromLookup(prefix, lookup)`                                               | `env(prefix, { from })`                                       |
| `secret('NAME', default)`, `env:"NAME" secret:"true"`                         | `secret(String)` or `secret(default)`, its variable in `vars` |
| `required(Number)`, `required:"true"`                                         | `required(Number)`, `config:"required"`, `Required<T>`        |
| `fixed(x)`, `fixed:"true"`                                                    | what is not `live`                                            |
| `update` over every layer, on any setting but a fixed one or a secret         | `update` on `live` settings only, over every layer            |
| `validate(schema)`, `kv.Validate(fn)`                                         | the `validate` option                                         |
| `sources()`, `Sources()`                                                      | `explain()`                                                   |
