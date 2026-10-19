# Secrets and required settings

A secret prints as `***` everywhere a value can leak: a log line, an error,
`JSON.stringify` of the whole config. A required setting has no default: a
layer must give it, or the program does not start, with an error that says
which variable or file would.

```ts
const config = await store.config('app', [
	{
		db: { url: secret(String), pool: 10 }, // a secret a layer must give
		sentry: { dsn: secret('') }, // a secret, empty unless a layer gives it
		region: required(String), // also Number and Boolean
	},
	env('APP', { vars: { db: { url: 'DATABASE_URL' } } }),
])

const pool = postgres(config.value.db.url.reveal()) // the text, where it is needed
console.log(config.value) // { db: { url: '***', pool: 10 }, sentry: { dsn: '***' }, region: 'eu' }
```

```python
@dataclass
class Db:
    url: Secret = secret()  # a layer must give it
    pool: int = 10


@dataclass
class Sentry:
    dsn: Secret = secret("")  # empty unless a layer gives it


@dataclass(kw_only=True)
class Settings:
    db: Db = field(default_factory=Db)
    sentry: Sentry = field(default_factory=Sentry)
    region: str  # no default: required


config = await store.config("app", [Settings, env("APP", vars={"db.url": "DATABASE_URL"})])
pool = await asyncpg.create_pool(config.value.db.url.reveal())
```

```go
type DB struct {
	URL  tinystore.Secret `json:"url" config:"required"`
	Pool int              `json:"pool"`
}

type Settings struct {
	DB     DB     `json:"db"`
	Sentry struct {
		DSN tinystore.Secret `json:"dsn"` // empty unless a layer gives it
	} `json:"sentry"`
	Region string `json:"region" config:"required"`
}

config, err := tinystore.OpenConfig(ctx, store, "app", Settings{DB: DB{Pool: 10}},
	tinystore.Env("APP", tinystore.Var("db.url", "DATABASE_URL")))
pool, err := pgxpool.New(ctx, config.Get().DB.URL.Reveal())
```

```rust
#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(default)]
struct Db {
    url: Secret<String>,
    pool: u32,
}

#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(default)]
struct Settings {
    db: Db,
    region: Required<String>,
}

let config = store.config("app", Settings::default()).env(Env::prefix("APP").var("db.url", "DATABASE_URL")).open()?;
let pool = connect(config.get().db.url.reveal())?;
```

## Secrets

- A secret prints as `***` in `console.log`, `JSON.stringify`, a template,
  `util.inspect`, an error, Python's `repr`, Go's `fmt` and `slog`, Rust's
  `Debug` and serde, the store's logs and `explain`. `reveal()` gives its
  text, where the program hands it to what needs it.
- A secret is never kept in the store, so a [live change](live.md) cannot
  set it: only a file, a variable or its `_FILE` gives it.
- An error about a secret names its variable and never quotes its text.
- A secret is text. It cannot be `live`: a value an admin page could set and
  read back is not a secret.

Hiding a secret from print keeps it out of logs and screenshots. It is not
access control: who may read the config at all is the server's tokens'
business.

## Secrets from files

Docker and Kubernetes mount secrets as files. A variable named with `_FILE`
reads the setting from that file, its last newline removed:

```sh
APP_DB_URL_FILE=/run/secrets/db-url
DATABASE_URL_FILE=/run/secrets/db-url
```

Both `APP_DB_URL` and `APP_DB_URL_FILE` set at once is `invalid`: one of them
would be ignored, and which one is a guess. `explain` names the file the
value came from:

```text
db.url      ***    DATABASE_URL_FILE, /run/secrets/db-url
```

## Required settings

```text
config app: region is required: region in app.toml, or set APP_REGION: invalid
```

- A required setting has no default: a layer gives it, or the config does not
  open, and the error names every layer that could have.
- An empty text counts as missing.
- The value a declaration gives is never counted as given: Go's zero value
  and Rust's `Default` do not satisfy `required`.
- Every problem of an open is listed in one error, so a deploy that lacks
  three variables fails once, naming all three.

| Language   | Required                                        | A secret                                          |
|------------|-------------------------------------------------|---------------------------------------------------|
| TypeScript | `required(String)`, `required(int({ min: 1 }))` | `secret(String)` is required; `secret('')` is not |
| Python     | a field without a default                       | `secret()` required; `secret("")` not             |
| Go         | `config:"required"`                             | `tinystore.Secret`                                |
| Rust       | `Required<T>`                                   | `Secret<T>`                                       |

## Marks that nest

`live` goes outermost, around a value, a rule or `required(…)`:

| Written                      | Means                                              |
|------------------------------|----------------------------------------------------|
| `live(100)`                  | 100 by default, changes while running              |
| `live(int(100, { min: 1 }))` | the same, at least 1                               |
| `live(required(String))`     | a layer must give it at start; it may change later |
| `live(secret(…))`            | refused: a secret is never kept                    |
| `required(secret(…))`        | refused: `secret(String)` is required already      |

Read, `required` and `live` leave the type: `required(String)` reads as a
`string`, `live(int(100))` as a `number`, and only a secret reads as a
`Secret`, whose `reveal()` gives its text.

## See also

- [Config](README.md): the engine's overview.
- [Layers and files](layers.md): where a value comes from.
- [Explain and validate](explain.md): seeing which layer gave each setting.
