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

| Page                                        | Covers                                                                       |
|---------------------------------------------|------------------------------------------------------------------------------|
| [Layers and files](layers.md)               | objects, TOML, YAML and JSON files, the environment, loading without a store |
| [Secrets and required settings](secrets.md) | `secret`, `required`, `_FILE` variables                                      |
| [Live settings](live.md)                    | `live`, `update`, `reset`, `ifVersion`                                       |
| [Watching](watching.md)                     | `watch` in this process and others                                           |
| [Explain and validate](explain.md)          | where a value came from, the program's own checks                            |

New to Config? [Settings](../../handbook/chat/settings.md) in the Handbook
declares a chat's settings and turns its replies off live.

## Async Rust

With the `async` feature, a Rust program opens the same config from
`tinystore::aio::Store`, and its changes and its watch are awaited. This
section shows only Rust, because in Bun, Node and Python every call is
already awaited, and in Go a call blocks its goroutine, which costs little.

```rust
use futures::StreamExt; // next() on a Stream
use tinystore::aio::Store;

let store = Store::open("data", Options::default())?;
let config = store.config("chat", Settings::default()).file(File::optional("chat.toml")).open()?;

let max_tokens = *config.get().model.max_tokens; // not awaited: the settings are in memory
config.update(|s| *s.model.max_tokens = 2048).await?; // blocking: config.update(...)?
config.reset("model.max_tokens").await?; // blocking: config.reset(...)?

let mut watching = config.watch(); // the settings now, then after each change
while let Some(settings) = watching.next().await {
    let settings = settings?; // a closed error once the store closes
    println!("max_tokens is {}", *settings.model.max_tokens);
}
```

- `get` and `explain` are plain calls, because they read memory.
- `update`, `if_version(…).update`, `reset` and `reset_all` return futures.
  Your closure and the config's `validate` run when the future is first
  polled, on your executor's thread. The commit runs on the store's own
  threads, and every handle sees the change before the future returns.
- After the commit, the next poll hands the change to every handle. So every
  handle's `validate` and the blocking watchers (`config.blocking().watch`)
  run on your executor's thread, or on the store's threads if you dropped the
  future. A retry after a conflict also runs your closure in a later poll.
- Once its future is polled, a change commits even if you drop the future.
  A future that you drop before its first poll changes nothing.
- `watch()` returns a `Stream`, and it starts watching when you call it. If
  your loop falls behind, it gets the newest settings, never every version
  between. When the store closes, the stream returns a closed error
  (`ErrorKind::Closed`) and then ends.
- A watch with a callback is the blocking handle's: `config.blocking().watch(…)`.

[Rust with async](../../handbook/start/rust-async.md) explains where the work
runs and what dropping a future does.

## Reference

[Bun and Node](../../reference/bun/config.md) ·
[Python](../../reference/python/config.md) ·
[Go](../../reference/go/config.md) ·
[Rust](../../reference/rust/config.md)
