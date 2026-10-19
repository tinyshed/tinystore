# Explain and validate

"Why is the rate 200 when the file says 1000?" `explain` answers it: every
setting, its value, the layer that gave it and every value it went over.
`validate` runs the program's own checks, the ones a single setting's rule
cannot say, on the whole value at open and before every change.

```ts
console.table(config.explain())
```

```python
for line in config.explain():  # Line(path, value, from_, over)
    print(line)
```

```go
for _, line := range config.Explain() { // tinystore.Line{Path, Value, From, Over}
	fmt.Println(line)
}
```

```rust
println!("{}", config.explain());
```

```text
path        value  from                                   over
port        9000   PORT                                   8000 app.toml, 8080 the declaration
db.url      ***    DATABASE_URL_FILE, /run/secrets/db-url
limits.rps  200    a change, 12 October 14:03, version 7  1000 app.toml, 100 the declaration
region      eu     APP_REGION
```

Each line is newest first: the layer that won, then what it went over. An
object in code is named by its place in the list, `layer 2`. Secrets show as
`***` here too.

## What was read and not used

After the settings, `explain` lists what was read and not used, each with
why. The open logs each once:

```text
app.local.toml: absent
APP_PROT: a variable with the prefix that names no setting; did you mean APP_PORT?
limits.rps: a kept change of 800 is ignored: above 500
```

In TypeScript these are the `notes` of what `explain()` returns, in Rust
`Explained.notes`. A typo in a variable is said, never silently ignored, and
the nearest setting's name is offered when one is an edit or two away.

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
