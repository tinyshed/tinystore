# Config

The Config engine reads a program's settings as one typed value: a
declaration of every setting and its default, then files and the environment
in the order you write them. Secrets print as `***`, required settings must
be given, and settings marked `live` change while the program runs, for
every process of the store at once.

```ts
const config = await store.config('chat', [
	{ port: 8080, model: { api_key: secret(String), max_tokens: live(1024) } },
	file('chat.toml', { optional: true }),
	env('CHAT', { vars: { port: 'PORT' } }),
])

config.value.model.max_tokens // read on every request
await config.update({ model: { max_tokens: 2048 } }) // every process sees it
console.table(config.explain()) // each setting, its value and where it came from
```

```python
config = await store.config("chat", [
    Settings,
    file("chat.toml", optional=True),
    env("CHAT", vars={"port": "PORT"}),
])

config.value.model.max_tokens
await config.update({"model": {"max_tokens": 2048}})
for line in config.explain():
    print(line)
```

```go
config, err := tinystore.OpenConfig(ctx, store, "chat", defaults,
	tinystore.File("chat.toml", tinystore.Optional()),
	tinystore.Env("CHAT", tinystore.Var("port", "PORT")))

config.Get().Model.MaxTokens
err = config.Update(ctx, func(s *Settings) { s.Model.MaxTokens = 2048 })
for _, line := range config.Explain() {
	fmt.Println(line)
}
```

```rust
let config = store
    .config("chat", Settings::default())
    .file(File::optional("chat.toml"))
    .env(Env::prefix("CHAT").var("port", "PORT"))
    .open()?;

let max_tokens = *config.get().model.max_tokens;
config.update(|s| *s.model.max_tokens = 2048)?;
println!("{}", config.explain());
```

## What it guarantees

- The layers are read in the order written, and for each setting the last
  layer that gives it wins.
- A config opens with every layer read, or fails with one error that lists
  every problem, naming the file or the variable.
- Only a `live` setting changes while the program runs; a change is kept over
  every layer until `reset`, and every process sharing the store sees it.
- A secret prints as `***` everywhere and is never kept in the store.
- A key or a variable that names no setting is said, with its nearest name,
  never silently ignored.

## In these guides

| Page                          | Covers                                              |
|-------------------------------|-----------------------------------------------------|
| Layers and files              | objects, TOML, YAML and JSON files, the environment |
| Secrets and required settings | `secret`, `required`, `_FILE` variables             |
| Live settings                 | `live`, `update`, `reset`, `ifVersion`              |
| Watching                      | `watch` in this process and others                  |
| Explain and validate          | where a value came from, the program's own checks   |

New to Config? [Settings](../../handbook/chat/settings.md) in the Handbook
declares a chat's settings and turns its replies off live.

## Reference

[Bun and Node](../../reference/bun/config.md) ·
[Python](../../reference/python/config.md) ·
[Go](../../reference/go/config.md) ·
[Rust](../../reference/rust/config.md)
