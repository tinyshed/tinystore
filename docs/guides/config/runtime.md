# Settings whose shape is known at run time

Some settings take their shape from something your program learns only as it
runs, such as the plugin an instance uses. The Config engine reads them like
any other settings: a field chooses the group, and each instance's settings
are read, checked and masked as the group its plugin names. A Rust or Go
program declares such settings as data, in the JSON every SDK sends.

```ts
const postgres = {
	host: 'localhost',
	port: int(5432, { min: 1, max: 65535 }),
	database: required(String),
	password: secret(String),
}
const stripe = { key: secret(String) }
const config = await store.config('dashbin', [
	{ instance: entries({ plugin: required(String), settings: chosenBy('plugin', { postgres, stripe }) }) },
	file('dashbin.toml', { secrets: false }),
	env('DASHBIN'),
])
const settings = config.value.instance.aimee?.settings // postgres's settings
```

```python
@dataclass(kw_only=True)
class Postgres:
    host: str = "localhost"
    port: Annotated[int, Range(1, 65535)] = 5432
    database: str
    password: Secret = secret()


@dataclass
class Stripe:
    key: Secret = secret()


@dataclass(kw_only=True)
class Instance:
    plugin: str
    settings: Postgres | Stripe = chosen_by("plugin", {"postgres": Postgres, "stripe": Stripe})


@dataclass
class Dashbin:
    instance: Entries[Instance] = entries()


config = await store.config("dashbin", [Dashbin, file("dashbin.toml", secrets=False), env("DASHBIN")])
settings = config.value.instance["aimee"].settings  # a Postgres
```

```go
declaration := json.RawMessage(`{
	"instance": { "entries": {
		"plugin": { "kind": "text", "required": true },
		"settings": { "chosenBy": "plugin", "groups": {
			"postgres": {
				"host": { "kind": "text", "default": "localhost" },
				"port": { "kind": "integer", "default": 5432, "min": 1, "max": 65535 },
				"database": { "kind": "text", "required": true },
				"password": { "kind": "text", "secret": true, "required": true }
			},
			"stripe": { "key": { "kind": "text", "secret": true, "required": true } }
		} }
	} }
}`)
config, err := tinystore.OpenConfigJSON(ctx, store, "dashbin", declaration,
	tinystore.File("dashbin.toml", tinystore.NoSecrets()), tinystore.Env("DASHBIN"))
settings := config.Get()["instance"].(map[string]any)["aimee"].(map[string]any)["settings"]
```

```rust
let postgres = json!({
    "host": { "kind": "text", "default": "localhost" },
    "port": { "kind": "integer", "default": 5432, "min": 1, "max": 65535 },
    "database": { "kind": "text", "required": true },
    "password": { "kind": "text", "secret": true, "required": true },
});
let stripe = json!({ "key": { "kind": "text", "secret": true, "required": true } });
let declaration = Declaration::from_json(json!({
    "instance": { "entries": {
        "plugin": { "kind": "text", "required": true },
        "settings": { "chosenBy": "plugin", "groups": { "postgres": postgres, "stripe": stripe } },
    } },
}))?;
let config = store.config("dashbin", declaration).file(File::from("dashbin.toml").secrets(false)).env("DASHBIN").open()?;
let settings = &config.get()["instance"]["aimee"]["settings"]; // a serde_json::Value
```

```toml title="dashbin.toml"
[instance.aimee]
plugin = "postgres"

[instance.aimee.settings]
host = "127.0.0.1"
database = "aimee"
```

```sh
DASHBIN_INSTANCE_AIMEE_SETTINGS_PASSWORD_FILE=/run/secrets/aimee
```

Here `aimee` uses the postgres plugin, so its settings are postgres's: the
port takes its default, 5432, and the password is a secret that its `_FILE`
variable reads. `dashbin.toml` is committed beside the code, so it is marked
to [refuse secrets](layers.md#keep-secrets-out-of-a-committed-file): a
password written in it fails the config, whichever plugin declares it. `explain` gives each field its own line:

```text
instance.aimee.plugin             "postgres"   dashbin.toml
instance.aimee.settings.database  "aimee"      dashbin.toml
instance.aimee.settings.host      "127.0.0.1"  dashbin.toml                                                       over "localhost" from the declaration
instance.aimee.settings.password  ***          DASHBIN_INSTANCE_AIMEE_SETTINGS_PASSWORD_FILE, /run/secrets/aimee
instance.aimee.settings.port      5432         the declaration
```

## A group chosen by a setting

`chosenBy / chosen_by / "chosenBy" / "chosenBy"` declares a group whose
settings are those of one of several groups. A text setting beside it, here
`plugin`, chooses the group by its name. The chosen group's settings follow
the rules of any group: defaults, required settings, secrets, and a variable
per setting with its `_FILE`, such as `DASHBIN_INSTANCE_AIMEE_SETTINGS_PORT`.
A key that the chosen group does not have is refused with the nearest name,
so a key of another plugin's group is refused too.

```text
config dashbin: dashbin.toml: instance.aimee.settings.hots: no such setting; did you mean instance.aimee.settings.host?
config dashbin: dashbin.toml: instance.aimee.plugin: "postgress" is not one of postgres, stripe; did you mean postgres?
```

The setting that chooses accepts only the names of the groups, so a typo in
it fails the config, whichever layer gives it. It is text, and it is neither
a secret nor live. A chosen group's settings are never live either, because
only the layers choose a group.

A chosen group stands among settings, at the top of the declaration, in a
group, or in an entry as above. It can't stand in a list's item or in
another chosen group.

## When a later layer chooses another group

A later layer can choose another group, for example a variable that switches
an instance to another plugin. The config then reads the group that is chosen
after every layer. What earlier layers gave the other group is dropped, and
`explain` says so in its notes:

```sh
DASHBIN_INSTANCE_AIMEE_PLUGIN=mysql
```

```text
instance.aimee.plugin             "mysql"      DASHBIN_INSTANCE_AIMEE_PLUGIN             over "postgres" from dashbin.toml
instance.aimee.settings.host      "localhost"  the declaration
instance.aimee.settings.password  ***          DASHBIN_INSTANCE_AIMEE_SETTINGS_PASSWORD
instance.aimee.settings.port      3306         the declaration
instance.aimee.settings: what dashbin.toml gave as postgres is dropped: instance.aimee.plugin is mysql, from DASHBIN_INSTANCE_AIMEE_PLUGIN
```

Each layer reads the chosen group's settings as the group that it chooses
itself, or else as the group that the layers before it chose. A value that
was given for the group that is chosen at the end is kept, even if a layer in
between chose another group.

## A declaration as data

In Bun, a declaration is already data: an object that your program can build
as it runs. In Python, `dataclasses.make_dataclass` builds a dataclass as the
program runs. A Rust or Go program declares its settings as JSON instead:
`Declaration::from_json` in Rust, which `store.config` and `config::load`
take, and `OpenConfigJSON` in Go.

| In the JSON                                 | Declares                                                                                                                 |
|---------------------------------------------|--------------------------------------------------------------------------------------------------------------------------|
| `"kind"`                                    | `text`, `integer`, `number`, `boolean`, `duration`, `object` or `any`                                                    |
| `"kind": { "list": { "kind": "text" } }`    | a list, whose items are values of a kind or a `{ "group": … }`                                                           |
| `"default"`                                 | the value when no layer gives one; a duration as `"30s"`                                                                 |
| `"default": null`                           | a setting with [no value](secrets.md#settings-with-no-value) until a layer gives one, of any kind                        |
| `"required": true`                          | a setting that a layer must give, which has no default                                                                   |
| `"secret": true`                            | text that prints as `***`                                                                                                |
| `"live": true`                              | a setting that changes while the program runs, with a store                                                              |
| `"min"`, `"max"`                            | the bounds of a number, a duration or the length of a list                                                               |
| `"oneOf"`                                   | the texts that a setting accepts                                                                                         |
| `{ "group": { … } }`                        | a group of settings                                                                                                      |
| `{ "entries": { … } }`                      | a map of [entries](layers.md#entries), each the group of settings it names                                               |
| `{ "chosenBy": "plugin", "groups": { … } }` | a group that the text setting `plugin` beside it chooses by name                                                         |
| `"secrets": false`, in a file's layer       | a file that [refuses secrets](layers.md#keep-secrets-out-of-a-committed-file), as `file(…, { secrets: false })` sends it |

A declaration that means nothing, such as a setting with neither a default
nor `required`, is refused when it is read, and the error names the setting.
Give it a default, `required`, or `"default": null` for no value.

The value is data too. A Rust program gets a `serde_json::Value`, in which a
duration is written as serde writes a `Duration`, so a chosen group's value
deserializes into the plugin's own type. A Go program gets a
`map[string]any`, in which a secret is a `tinystore.Secret`, a duration a
`time.Duration`, an integer an `int64` and a number a `float64`.

> [!WARNING]
> In the value of a declaration as data, a Rust program reads a secret as
> plain text. Hand it to what needs it, and don't print the whole value. In
> Bun, Python and Go, the value holds a secret as a `Secret`, which prints as
> `***`.

## See also

- [Layers and files](layers.md): files, the environment and entries.
- [Secrets and required settings](secrets.md).
- [Explain and validate](explain.md): where each value came from.
