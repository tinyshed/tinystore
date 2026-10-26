# config

`tinystore config` shows a config as the store keeps it and changes its live
settings from a shell, with no SDK: turn a limit up while the program runs,
or see why a setting has the value it has.

```sh
tinystore config ./data explain chat
tinystore config ./data update chat model.max_tokens=2048
tinystore config ./data reset chat model.max_tokens
```

## What the store knows

A store keeps two things of a config: the last declaration a program opened
it with, and the changes made to its live settings since. The program's own
files and environment it never sees, so `config` works with what the store
knows:

- the declaration's defaults and rules;
- the changes, each with its version and time;
- a required setting, which the program gives from its files or environment,
  shown as such, without a value.

A config no program has opened in this store is refused: there is no
declaration to read.

## Explain

```sh
tinystore config ./data explain chat
```

```text
config chat · version 3
  model.max_tokens  2048     a change, version 3, 2026-10-26 14:03:05 UTC  over 1024 from the declaration
  model.name        "small"  the declaration
  model.window      30s      the declaration
  region: required, given by the program alone
```

- It reads beside the program or server that holds the directory.
- A secret shows as `***`.
- `--json` gives `{"name", "version", "settings": [{"path", "value",
  "from", "over"}], "notes"}`.

## Update live settings

```sh
tinystore config ./data update chat model.max_tokens=2048
tinystore config ./data update chat limits.window=30s 'model.stop:=["\n"]'
```

```text
updated config chat to version 4: model.max_tokens=2048
```

- `path=value` is read by the setting's kind, as an environment variable is:
  `2048` an integer, `30s` a span, `a,b` a list. `path:=json` gives JSON as
  it is.
- Only a setting the declaration marks live changes, and its rules check the
  value. A setting that isn't live is refused, and so is a misspelt one,
  with its nearest:

```text
tinystore config update: config chat: no setting model.max_tokns; did you mean model.max_tokens?
```

- It goes through the server that holds the directory, or a sidecar it
  starts, so every program holding the config takes the change at once, as
  `config.update` in a program does.

## Reset

```sh
tinystore config ./data reset chat model.max_tokens
tinystore config ./data reset chat
```

A setting it resets goes back to what each program's declaration, files and
environment give. Without a path it drops every change of the config.

## See also

- [config](../config/README.md): declarations, layers, secrets and live
  settings from a program.
