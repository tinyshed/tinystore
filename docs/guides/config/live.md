# Live settings

A setting marked `live` may change while the program runs, and nothing else
may. A change is kept in the store over every layer until it is reset, so it
survives a restart, and every process that shares the store sees it before
`update` returns.

```ts
const config = await store.config('app', [{ port: 8080, limits: { rps: live(100), burst: live(50) } }])

await config.update({ limits: { rps: 200 } }) // kept; every handle and process sees it
await config.reset('limits.rps') // the layers give it again
await config.reset() // every change of the config
```

```python
@dataclass
class Limits:
    rps: int = live(100)
    burst: int = live(50)


await config.update({"limits": {"rps": 200}})
await config.reset("limits.rps")
await config.reset()
```

```go
type Limits struct {
	RPS   int `json:"rps" config:"live"`
	Burst int `json:"burst" config:"live"`
}

err = config.Update(ctx, func(s *Settings) { s.Limits.RPS = 200 })
err = config.Reset(ctx, "limits.rps")
err = config.ResetAll(ctx)
```

```rust
#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(default)]
struct Limits {
    rps: Live<u32>,
    burst: Live<u32>,
}

config.update(|s| *s.limits.rps = 200)?;
config.reset("limits.rps")?;
config.reset_all()?;
```

## Only what is live changes

```text
config app: port is not live: invalid
```

`update({ port: 9000 })` is `invalid`, and TypeScript refuses it as it is
written. Settings that take effect only at start, an address, a path, a pool
size, are never changed under a running program by mistake: mark live only
what the program reads on every use.

## A change outranks every layer

A kept change is the strongest layer on purpose: `live(100)` lets whoever
runs the program, through an admin page of yours, outrank the
code, the files and the environment. A deploy that sets `APP_LIMITS_RPS`
does not undo it, as [`explain`](explain.md) shows in each language:

```text for=bun
┌───┬──────────────┬───────┬──────────────────────────────────────────────┬────────────────────────────────────────────────────┐
│   │ path         │ value │ from                                         │ over                                               │
├───┼──────────────┼───────┼──────────────────────────────────────────────┼────────────────────────────────────────────────────┤
│ 0 │ limits.burst │ 50    │ the declaration                              │                                                    │
│ 1 │ limits.rps   │ 200   │ a change, version 1, 2026-10-24 09:34:52 UTC │ 1000 from APP_LIMITS_RPS, 100 from the declaration │
│ 2 │ port         │ 8080  │ the declaration                              │                                                    │
└───┴──────────────┴───────┴──────────────────────────────────────────────┴────────────────────────────────────────────────────┘
```

```text for=python
Line(path='limits.burst', value='50', from_='the declaration', over='')
Line(path='limits.rps', value='200', from_='a change, version 1, 2026-10-24 09:34:55 UTC', over='1000 from APP_LIMITS_RPS, 100 from the declaration')
Line(path='port', value='8080', from_='the declaration', over='')
```

```text for=go
limits.burst  50  the declaration
limits.rps  200  a change, version 1, 2026-10-24 09:35:00 UTC  over 1000 from APP_LIMITS_RPS, 100 from the declaration
port  8080  the declaration
```

```text for=rust
limits.burst  50    the declaration
limits.rps    200   a change, version 1, 2026-10-24 09:35:00 UTC  over 1000 from APP_LIMITS_RPS, 100 from the declaration
port          8080  the declaration
```

`reset` removes the change and lets the layers give the setting again.

## Two changes, two settings

`update` keeps the settings it names, and in Go and Rust the ones its function
changed, never the whole value. Two updates of different settings both stay,
from two processes too: `{ rps: 200 }` and `{ burst: 500 }` make
`{ rps: 200, burst: 500 }`. An update keeps all its settings or none, and the
value they make is validated whole.

## Versions

Every change moves the config's `version`. An admin page that shows a value
sends the version it showed, and an update another change came before is
`conflict`:

```ts
const shown = config.version
// … the admin edits the form …
await config.update({ limits: { rps: 300 } }, { ifVersion: shown }) // ConflictError if someone changed it meanwhile
```

```python
shown = config.version
await config.update({"limits": {"rps": 300}}, if_version=shown)
```

```go
shown := config.Version()
err = config.Update(ctx, func(s *Settings) { s.Limits.RPS = 300 }, tinystore.IfVersion(shown))
```

```rust
let shown = config.get().version();
config.if_version(shown).update(|s| *s.limits.rps = 300)?;
```

`value` and `version` change together, so what was shown and what is sent
always match.

## A change that no longer fits

A deploy can tighten a rule: `rps` was any number, now at most 500, and 800 is
kept. A kept change that no longer fits, the setting no longer live, now a
secret, of another type or refused by `validate`, is ignored, not removed:

- the open warns about it in the log, once;
- `explain` lists it with why;
- it applies again once it fits, and `reset` of its path removes it.

So a stricter release still starts. `strict: true` fails the open instead,
for a config whose defaults are worse than not starting.

## Several programs, one config

Through a server, several programs open one config, each with its own
declaration: two versions of one program during a deploy, or a web process
and a worker that read different settings. A change is checked against the
declaration of every handle open when it comes, and one any of them refuses
keeps nothing:

```text
config app: limits.rps: 800 is above 500, as another program that has the config open declares
```

A kept change of a setting a program does not declare may be another
program's: it is neither applied nor warned about, and `explain` lists it
apart.

## Limits

| What                               | Limit          |
|------------------------------------|----------------|
| a setting's value, kept            | 16 KiB of JSON |
| what a config keeps of its changes | 256 KiB        |

## See also

- [Config](README.md): the engine's overview.
- [Watching](watching.md): applying a change the moment it is made.
- [Turn replies off live](../../handbook/chat/settings.md) in the Handbook.
