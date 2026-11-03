# Settings

The chat needs a port, a model's name, an API key and a few limits. Some come
from a file, some from the environment, and some must change while the chat
runs, such as a switch that turns replies off when the model's provider has
an outage. This chapter declares the settings once, reads them from every
source in a fixed order, and changes the live ones for every process at once.
It uses the Config engine.

## Declare the settings

```ts
import { env, file, int, live, secret } from '@tinyshed/tinystore'

const config = await store.config('chat', [
	{
		port: 8080,
		model: {
			name: 'claude-sonnet-5-5',
			api_key: secret(String),
			max_tokens: live(int(1024, { min: 1, max: 8192 })),
		},
		replies: { enabled: live(true) },
	},
	file('chat.toml', { optional: true }),
	env('CHAT', { vars: { port: 'PORT' } }),
])
```

```python
@dataclass
class Model:
    name: str = "claude-sonnet-5-5"
    api_key: Secret = secret()
    max_tokens: Annotated[int, Range(1, 8192)] = live(1024)


@dataclass
class Replies:
    enabled: bool = live(True)


@dataclass
class Settings:
    port: int = 8080
    model: Model = field(default_factory=Model)
    replies: Replies = field(default_factory=Replies)


config = await store.config("chat", [
    Settings,
    file("chat.toml", optional=True),
    env("CHAT", vars={"port": "PORT"}),
])
```

```go
type Settings struct {
	Port    int     `json:"port"`
	Model   Model   `json:"model"`
	Replies Replies `json:"replies"`
}

type Model struct {
	Name      string           `json:"name"`
	APIKey    tinystore.Secret `json:"api_key" config:"required"`
	MaxTokens int              `json:"max_tokens" config:"live,min=1,max=8192"`
}

type Replies struct {
	Enabled bool `json:"enabled" config:"live"`
}

config, err := tinystore.OpenConfig(ctx, store, "chat",
	Settings{Port: 8080, Model: Model{Name: "claude-sonnet-5-5", MaxTokens: 1024}, Replies: Replies{Enabled: true}},
	tinystore.File("chat.toml", tinystore.Optional()),
	tinystore.Env("CHAT", tinystore.Var("port", "PORT")),
)
```

```rust
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
struct Settings {
    port: u16,
    model: Model,
    replies: Replies,
}

#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(default)]
struct Model {
    name: String,
    api_key: Secret<String>,
    max_tokens: Live<u32>,
}

#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(default)]
struct Replies {
    enabled: Live<bool>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            port: 8080,
            model: Model { name: "claude-sonnet-5-5".into(), max_tokens: Live::new(1024), ..Model::default() },
            replies: Replies { enabled: Live::new(true) },
        }
    }
}

let config = store
    .config("chat", Settings::default())
    .file(File::optional("chat.toml"))
    .env(Env::prefix("CHAT").var("port", "PORT"))
    .open()?;
```

The first layer is the declaration: every setting's name, its type and its
default. A setting's name is the same in the file, in the environment and in
every language, so the TypeScript declaration spells `max_tokens` as the
file does. Each layer after it may give some of the settings again, and for each
setting the last layer that gives it wins, as in `{ ...a, ...b }`:

| Layer                | Gives                                                      |
|----------------------|------------------------------------------------------------|
| the declaration      | `port` 8080, the model's name, `max_tokens` 1024           |
| `chat.toml`          | what the file sets, if the file exists                     |
| the environment      | `PORT`, `CHAT_MODEL_NAME`, `CHAT_MODEL_API_KEY`, and so on |
| a change, for `live` | what an admin set while the chat runs                      |

A file is TOML, YAML or JSON by its extension:

```toml title="chat.toml"
[model]
name = "claude-haiku-5-5"
max_tokens = 2048
```

The environment is read as the prefix and each setting's path in upper snake
case, `CHAT_MODEL_API_KEY`, and `vars` gives `port` the `PORT` a hosting
platform sets ([The environment](../../guides/config/layers.md#the-environment)).
`CHAT_MODEL_API_KEY_FILE` reads the key from a file, as Docker and
Kubernetes pass secrets ([Secrets from
files](../../guides/config/secrets.md#secrets-from-files)).

The config opens only when every layer reads, or fails with one error that
lists every problem:

```text
config chat: model.api_key is required: set CHAT_MODEL_API_KEY, or model.api_key in chat.toml
config chat: chat.toml: model.max_tokns: no such setting; did you mean model.max_tokens?
config chat: CHAT_MODEL_MAX_TOKENS: "lots" is not a number
```

## Read a setting

```ts
config.value.model.max_tokens // 2048, from chat.toml
config.value.model.api_key.reveal() // the key's text, for the model's client
```

```python
config.value.model.max_tokens  # 2048, from chat.toml
config.value.model.api_key.reveal()  # the key's text, for the model's client
```

```go
settings := config.Get() // a copy
settings.Model.MaxTokens // 2048, from chat.toml
settings.Model.APIKey.Reveal() // the key's text, for the model's client
```

```rust
let settings = config.get(); // a snapshot
*settings.model.max_tokens // 2048, from chat.toml
settings.model.api_key.reveal() // the key's text, for the model's client
```

A read is a read of memory, so read the value where you use it, on every
request, instead of copying it at start. Then a change reaches the next
request with no restart.

`api_key` is a secret. It prints as `***` everywhere, from `console.log` to
`explain`, and `reveal()` gives its text at the one place that needs it. A
secret is never kept in the store, so only a file or a variable can give it
([Secrets](../../guides/config/secrets.md#secrets)).

## Change a live setting

The model's provider has an outage. An admin turns replies off for every
process of the chat:

```ts
await config.update({ replies: { enabled: false } })
await config.update({ port: 9000 }) // InvalidError: port is not live
await config.reset('replies.enabled') // the layers give it again
```

```python
await config.update({"replies": {"enabled": False}})
await config.update({"port": 9000})  # InvalidError: port is not live
await config.reset("replies.enabled")  # the layers give it again
```

```go
err = config.Update(ctx, func(s *Settings) { s.Replies.Enabled = false })
err = config.Update(ctx, func(s *Settings) { s.Port = 9000 }) // invalid: port is not live
err = config.Reset(ctx, "replies.enabled")                    // the layers give it again
```

```rust
config.update(|s| *s.replies.enabled = false)?;
config.reset("replies.enabled")?; // the layers give it again
```

Only a setting marked `live` can change while the chat runs ([Only what is
live changes](../../guides/config/live.md#only-what-is-live-changes)). A
change is kept in `kv.db`, survives a restart and goes over every layer until
`reset`. Every process connected to the store sees it as soon as it is
committed. A change that breaks a rule, `max_tokens: 99999`, is refused and
keeps nothing.

The reply worker checks the switch on every job, and snoozes while it is off:

```ts
replies.work(async (reply, run) => {
	if (!config.value.replies.enabled) return run.snooze('1m')
	// …write the reply
})
```

```python
async def write_reply(reply: Reply, run: tinystore.Run) -> tinystore.Outcome | None:
    if not config.value.replies.enabled:
        return run.snooze("1m")
    ...  # write the reply
```

```go
err = replies.Work(ctx, func(ctx context.Context, reply Reply, run *jobs.Run) error {
	if !config.Get().Replies.Enabled {
		return run.Snooze(time.Minute)
	}
	return writeReply(ctx, reply, run)
})
```

```rust
let worker = replies.work(move |reply: Reply, run| -> Result<Outcome, AppError> {
    if !*config.get().replies.enabled {
        return Ok(run.snooze(Duration::from_mins(1)));
    }
    write_reply(reply, run)
})?;
```

A job snoozed does not use up an attempt, so no reply fails because of the
outage. When the admin turns replies back on, the waiting jobs run within a
minute.

To react to a change as it happens, watch the config:

```ts
const stop = config.watch(next => limiter.setMaxTokens(next.model.max_tokens))
```

```python
async for settings in config.watch():
    limiter.set_max_tokens(settings.model.max_tokens)
```

```go
for settings := range config.Watch(ctx) {
	limiter.SetMaxTokens(settings.Model.MaxTokens)
}
```

```rust
let watching = config.watch(move |next, _previous| limiter.set_max_tokens(*next.model.max_tokens));
```

The function gets the settings now, then again after every change, made in
this process or in another.

## Where did this value come from?

```ts
console.table(config.explain())
```

```python
for line in config.explain():
    print(line)
```

```go
for _, line := range config.Explain() {
	fmt.Println(line)
}
```

```rust
println!("{}", config.explain());
```

With `PORT=9000` and `CHAT_MODEL_API_KEY` set, and replies turned off, each
language prints:

```text for=bun
┌───┬──────────────────┬────────────────────┬──────────────────────────────────────────────┬──────────────────────────────────────────┐
│   │ path             │ value              │ from                                         │ over                                     │
├───┼──────────────────┼────────────────────┼──────────────────────────────────────────────┼──────────────────────────────────────────┤
│ 0 │ model.api_key    │ ***                │ CHAT_MODEL_API_KEY                           │                                          │
│ 1 │ model.max_tokens │ 2048               │ chat.toml                                    │ 1024 from the declaration                │
│ 2 │ model.name       │ "claude-haiku-5-5" │ chat.toml                                    │ "claude-sonnet-5-5" from the declaration │
│ 3 │ port             │ 9000               │ PORT                                         │ 8080 from the declaration                │
│ 4 │ replies.enabled  │ false              │ a change, version 1, 2026-10-24 09:34:52 UTC │ true from the declaration                │
└───┴──────────────────┴────────────────────┴──────────────────────────────────────────────┴──────────────────────────────────────────┘
```

```text for=python
Line(path='model.api_key', value='***', from_='CHAT_MODEL_API_KEY', over='')
Line(path='model.max_tokens', value='2048', from_='chat.toml', over='1024 from the declaration')
Line(path='model.name', value='"claude-haiku-5-5"', from_='chat.toml', over='"claude-sonnet-5-5" from the declaration')
Line(path='port', value='9000', from_='PORT', over='8080 from the declaration')
Line(path='replies.enabled', value='false', from_='a change, version 1, 2026-10-24 09:34:55 UTC', over='true from the declaration')
```

```text for=go
model.api_key  ***  CHAT_MODEL_API_KEY
model.max_tokens  2048  chat.toml  over 1024 from the declaration
model.name  "claude-haiku-5-5"  chat.toml  over "claude-sonnet-5-5" from the declaration
port  9000  PORT  over 8080 from the declaration
replies.enabled  false  a change, version 1, 2026-10-24 09:35:00 UTC  over true from the declaration
```

```text for=rust
model.api_key     ***                 CHAT_MODEL_API_KEY
model.max_tokens  2048                chat.toml                                     over 1024 from the declaration
model.name        "claude-haiku-5-5"  chat.toml                                     over "claude-sonnet-5-5" from the declaration
port              9000                PORT                                          over 8080 from the declaration
replies.enabled   false               a change, version 1, 2026-10-24 09:35:00 UTC  over true from the declaration
```

`explain` lists every setting, its value, the layer that gave it and the
values it went over. After the settings it lists what was read and not used,
such as a variable `CHAT_PROT=9000` that names no setting, so a typo is
never silently ignored ([What was read and not
used](../../guides/config/explain.md#what-was-read-and-not-used)). Each
line's `given` holds the same as data, such as the version and the time of a
change: see [Explain and validate](../../guides/config/explain.md#each-value-as-data).

## Why this design?

**Why layers in an order you write.** Many config libraries merge sources in
an order that depends on how they were called, or on rules nobody remembers.
Then "why is the port 9000?" takes an afternoon. Here the order is the list
you wrote, top to bottom, and `explain` answers the question in one line.

**Why a change wins over the environment.** An admin who turns replies off
during an outage must not be overruled by the next deploy, whose environment
still says `true`: see [A change outranks every
layer](../../guides/config/live.md#a-change-outranks-every-layer).

**Why only `live` settings change, and one config serves every process.** A
port read at start and changed in the store would show a value the program
is not using, and four processes that each read their own file would
disagree for minutes after a change. [Live
settings](../../guides/config/live.md) covers both.

## See also

- [Replies in the background](replies.md): the worker that the switch
  pauses.
- [Layers and files](../../guides/config/layers.md#limits-and-defaults): the bounds of a
  config, and [Durations](../../guides/config/layers.md#rules) the units a
  duration takes.
- [Config API reference](../../reference/bun/config.md): every call and rule of
  configs.
