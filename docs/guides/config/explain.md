# Explain and validate

"Why is the rate 200 when the file says 1000?" `explain` answers it: every
setting, its value, the layer that gave it and every value it went over.
`validate` runs the program's own checks, the ones a single setting's rule
cannot say, on the whole value at open and before every change.

```ts
console.table(config.explain())
```

```python
for line in config.explain():  # Line(path, value, from_, over, given)
    print(line)
```

```go
for _, line := range config.Explain() { // tinystore.Line{Path, Value, From, Over, Given}
	fmt.Println(line)
}
```

```rust
println!("{}", config.explain());
```

For a config that reads `app.toml` and the variables `PORT`,
`DATABASE_URL_FILE` and `APP_REGION`, after a change of `limits.rps`, each
language prints:

```text for=bun
┌───┬────────────┬───────┬──────────────────────────────────────────────┬───────────────────────────────────────────────┐
│   │ path       │ value │ from                                         │ over                                          │
├───┼────────────┼───────┼──────────────────────────────────────────────┼───────────────────────────────────────────────┤
│ 0 │ db.url     │ ***   │ DATABASE_URL_FILE, secrets/db-url            │                                               │
│ 1 │ limits.rps │ 200   │ a change, version 1, 2026-10-24 09:34:52 UTC │ 1000 from app.toml, 100 from the declaration  │
│ 2 │ port       │ 9000  │ PORT                                         │ 8000 from app.toml, 8080 from the declaration │
│ 3 │ region     │ "eu"  │ APP_REGION                                   │                                               │
└───┴────────────┴───────┴──────────────────────────────────────────────┴───────────────────────────────────────────────┘
```

```text for=python
Line(path='db.url', value='***', from_='DATABASE_URL_FILE, secrets/db-url', over='')
Line(path='limits.rps', value='200', from_='a change, version 1, 2026-10-24 09:34:55 UTC', over='1000 from app.toml, 100 from the declaration')
Line(path='port', value='9000', from_='PORT', over='8000 from app.toml, 8080 from the declaration')
Line(path='region', value='"eu"', from_='APP_REGION', over='')
```

```text for=go
db.url  ***  DATABASE_URL_FILE, secrets/db-url
limits.rps  200  a change, version 1, 2026-10-24 09:34:59 UTC  over 1000 from app.toml, 100 from the declaration
port  9000  PORT  over 8000 from app.toml, 8080 from the declaration
region  "eu"  APP_REGION
```

```text for=rust
db.url      ***   DATABASE_URL_FILE, secrets/db-url
limits.rps  200   a change, version 1, 2026-10-24 09:35:00 UTC  over 1000 from app.toml, 100 from the declaration
port        9000  PORT                                          over 8000 from app.toml, 8080 from the declaration
region      "eu"  APP_REGION
```

`over` is newest first: the layer that won, then what it went over. An object
in code is named by its place in the list, `layer 2`. Secrets show as `***`
here too.

## Each value as data

Each line also has `given`: every value the setting was given, newest first.
The first item is the line's own value, and the rest are the values it went
over. Each item has its `value`, as `explain` shows it, and `from`, which says
where the value came from as data. Use it to show a setting's history in your
own words, instead of parsing the text:

```ts
const rps = config.explain().find(line => line.path === 'limits.rps')
const [own, under] = rps?.given ?? []
if (own?.from.kind === 'change') {
	own.from.version // 1
	own.from.at // a Date: when the change was made
	under?.value // '1000': what a reset gives back
	under?.from // { kind: 'file', path: 'app.toml' }
}
```

```python
rps = next(line for line in config.explain() if line.path == "limits.rps")
if len(rps.given) > 1 and rps.given[0].from_.kind == "change":
    own, under = rps.given[0], rps.given[1]
    own.from_.version  # 1
    own.from_.at  # a datetime in UTC: when the change was made
    under.value  # "1000": what a reset gives back
    under.from_  # Origin(kind='file', path='app.toml')
```

```go
for _, line := range config.Explain() {
	if line.Path != "limits.rps" || len(line.Given) < 2 {
		continue
	}
	own, under := line.Given[0], line.Given[1]
	if own.From.Kind == tinystore.OriginChange {
		own.From.Version // 1
		own.From.At      // a time.Time: when the change was made
		under.Value      // "1000": what a reset gives back
		under.From       // tinystore.Origin{Kind: tinystore.OriginFile, Path: "app.toml"}
	}
}
```

```rust
use tinystore::config::Origin;

let explained = config.explain();
let rps = explained.settings.iter().find(|line| line.path == "limits.rps").unwrap();
if let [own, under, ..] = rps.given.as_slice()
    && let Origin::Change { version, at, .. } = &own.from
{
    // version is 1, and at is a SystemTime: when the change was made
    // under.value is "1000": what a reset gives back
    // under.from is Origin::File { path: "app.toml" }
}
```

`from` has a `kind`, and only the fields that its kind names:

| `kind`        | Fields                              | Its text                                                    |
|---------------|-------------------------------------|-------------------------------------------------------------|
| `declaration` |                                     | `the declaration`                                           |
| `layer`       | `place`, where the declaration is 1 | `layer 2`                                                   |
| `file`        | `path`                              | `app.toml`                                                  |
| `env`         | `variable`, and `dotenv` or `file`  | `PORT`, `PORT in .env`, `DATABASE_URL_FILE, secrets/db-url` |
| `change`      | `version` and `at`                  | `a change, version 1, 2026-10-24 09:35:00 UTC`              |

- `dotenv` is the `.env` file that set the variable, and `file` is the file
  that a `NAME_FILE` variable named. At most one of the two is set.
- `from` is `from_` in Python and `From` in Go. In Go the kinds are the
  constants `tinystore.OriginDeclaration`, `OriginLayer`, `OriginFile`,
  `OriginEnv` and `OriginChange`. In Rust they are the variants of `Origin`.
  `Origin` and each of its variants are `#[non_exhaustive]`, so a `match` on
  it needs a `_` arm, and a variant's pattern needs `..`, as in
  `Origin::File { path, .. }`.
- A newer server may send a `kind` that your SDK does not know. The SDK keeps
  it as it was sent, and its text is the kind, so give a `switch` on `kind` a
  default case.
- `at` is a `Date` in TypeScript, a `datetime` in UTC in Python, a
  `time.Time` in Go and a `SystemTime` in Rust.
- The first item's `from`, as text, is the line's `from`:
  `String(given[0].from)`, `str(given[0].from_)`, `Given[0].From.String()` and
  `given[0].from.to_string()`.
- In TypeScript, `console.table` and a spread such as `{ ...line }` leave
  `given` out, so the table keeps its four columns, and `JSON.stringify` keeps
  it. Python's `print(line)` leaves it out too.

> [!NOTE]
> **A server before 0.1.1 sends no `given`**
> An SDK from 0.1.1 that reaches an older sidecar, child or server gets an
> empty `given`. Check its length before you read `given[0]`, as the code
> above does, or Go panics and TypeScript reads `undefined`.

## What was read and not used

After the settings, `explain` lists what was read and not used, each with
why. The open logs each once:

```text
app.local.toml: absent
APP_PROT: a variable with the prefix that names no setting; did you mean APP_PORT?
limits.rps: the change of version 1 is ignored: 800 is above 500
```

These are the `notes` of what `explain()` returns in TypeScript and Python,
`Notes()` in Go and `Explained.notes` in Rust. A typo in a variable is said,
never silently ignored, and the nearest setting's name is offered when one is
an edit or two away.

## Validate

```ts
const config = await store.config('app', layers, { validate: Settings }) // a zod or valibot schema, or a function
```

```python
def check(settings: Settings) -> None:
    if settings.limits.burst < settings.limits.rps:
        raise ValueError("limits.burst is below limits.rps")


config = await store.config("app", layers, validate=check)  # or a pydantic model
```

```go
config, err := tinystore.OpenConfig(ctx, store, "app", defaults, tinystore.Validate(func(s Settings) error {
	if s.Limits.Burst < s.Limits.RPS {
		return errors.New("limits.burst is below limits.rps")
	}
	return nil
}))
```

```rust
let config = store
    .config("app", Settings::default())
    .validate(|s| if *s.limits.burst < *s.limits.rps { Err("limits.burst is below limits.rps".into()) } else { Ok(()) })
    .open()?;
```

- `validate` sees the whole value at open and before a change is kept; a
  change it refuses keeps nothing and fails `invalid` with its message.
- It takes a Standard Schema, zod's or valibot's, a pydantic model in Python,
  or a function that throws.
- What one value says of itself, a range, a set, a list's length, is a
  [rule](layers.md#rules), which the core checks in every layer and names the
  variable or file. What relates two settings, or a format, a URL, an email,
  is `validate`'s.

At open, kept changes are applied in the order they were made: each that
leaves the value valid stays, and each that does not is ignored and said, so
a release with a stricter `validate` still starts, unless the config is
`strict`.

`validate` runs in the program that makes a change, since it is the
program's code. Another program's change it refuses leaves the value before
it, says so, and `explain` lists it.

## Errors

Every error of an open is `invalid`, and one error lists every problem:

```text
config app: app.toml: prot: no such setting; did you mean port?; APP_PORT: "eighty" is not a number;
  region is required: region in app.toml, or set APP_REGION: invalid
```

and one a change:

```text
config app: port is not live: invalid
config app: limits.rps: "fast" is not a number: invalid
config app: changed since the version the change was made on: conflict
```

An error about a secret names its variable and never its text.

## See also

- [Config](README.md): the engine's overview.
- [Layers and files](layers.md): the layers `explain` names.
