# Watching

Most settings are read where they are used, on every request, and need no
watching: a read is a read of memory. A watch is for a setting that something
else holds a copy of: a limiter's rate, a pool's size, a logger's level. It
calls your function with the settings now, then after every change, made in
this process or another.

```ts
const stop = config.watch((next, previous) => limiter.setRate(next.limits.rps))
stop()
```

```python
async for settings in config.watch():  # the settings now, then after every change
    limiter.set_rate(settings.limits.rps)
```

```go
for s := range config.Watch(ctx) { // until ctx ends
	limiter.SetRate(s.Limits.RPS)
}
```

```rust
let watching = config.watch(move |next, _previous| limiter.set_rate(*next.limits.rps)); // stops when dropped
```

## What a watcher gets

| Call  | `next`                      | `previous`             |
|-------|-----------------------------|------------------------|
| first | the settings now            | none                   |
| later | the settings after a change | the settings before it |

- The first call lets a function that applies a setting do nothing else at
  start. One that answers changes alone returns while `previous` is absent.
- A watcher sees a change whole and once it is committed, never a value half
  changed.
- One that falls behind gets the newest value, not every one between.
- A connection that was lost reads the config again when it is back, and
  calls its watchers if the config changed meanwhile.

In Rust the function runs on the thread that made the change, once it is
committed, and must not block. A watcher that changes the config itself is
called again after it returns, with each version in order.

## Reading without watching

```ts
app.get('/chat', req => {
	if (config.value.replies.paused) return paused() // read on every request
})
```

```python
@app.get("/chat")
async def chat():
    if config.value.replies.paused:  # read on every request
        return paused()
```

```go
http.HandleFunc("/chat", func(w http.ResponseWriter, r *http.Request) {
	if config.Get().Replies.Paused { // read on every request
		paused(w)
	}
})
```

```rust
if *config.get().replies.paused {
    return paused(); // read on every request
}
```

`value` is a frozen object that a change replaces whole. A reference kept
keeps what it had: `const { rps } = config.value.limits` does not follow a
change. Read `config.value` where the setting is used, or watch it.

| Language   | A read is                                                                |
|------------|--------------------------------------------------------------------------|
| TypeScript | `config.value`, frozen; `config.version` beside it                       |
| Python     | `config.value`; `config.version`                                         |
| Go         | `config.Get()`, a copy; `config.Version()`                               |
| Rust       | `config.get()`, a `Snapshot` that reads as the settings; its `version()` |

## One stream a process

Every handle and every watcher of a config in one process share one stream:
the store sends a change once, and the process hands the new value to each.
Opening the same config twice, with the same declaration and layers, costs
nothing more.

## See also

- [Config](README.md): the engine's overview.
- [Live settings](live.md): what can change.
