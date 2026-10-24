# Config API for Rust

Every public type, trait and function of the Config engine in `tinystore::config` and `tinystore::aio::config`, generated from its source. The [Config guide](../../guides/config/README.md) explains how to use them, and the [Bun and Node](../bun/config.md), [Python](../python/config.md) and [Go](../go/config.md) pages list the same API.

## Store

```rust
impl Store
```

### Store.config

```rust
pub fn config<T>(&self, name: &str, declaration: T) -> ConfigBuilder<T>
where
    T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
```

The program's settings as one typed value. `declaration` is every
setting with its default, a `serde` struct whose fields may be marked
`Secret`, `Required` or `Live`; the layers the builder adds go over it
in the order written.

## aio::Store

```rust
impl aio::Store
```

### aio::Store.config

```rust
pub fn config<T>(&self, name: &str, declaration: T) -> ConfigBuilder<T>
where
    T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
```

The program's settings as one typed value, declared as
`crate::Store::config` declares them, its changes awaited.

## Env

```rust
pub struct Env { /* private fields */ }
```

The variables a config reads: the prefix's, those `var` names, from the
process, or from what `from` gives in its place, as a test does.

### Env.prefix

```rust
pub fn prefix(prefix: impl Into<String>) -> Env
```

Variables named by `prefix` and each setting's path: `APP_PORT`.

### Env.var

```rust
pub fn var(self, path: &str, name: &str) -> Env
```

The setting at `path` reads `name` in place of its prefixed variable.

### Env.files

```rust
pub fn files<P: Into<PathBuf>>(self, files: impl IntoIterator<Item = P>) -> Env
```

`.env` files, whose variables count where the process has none; one
that does not exist is skipped, a later one over an earlier.

### Env.from

```rust
pub fn from<K: Into<String>, V: Into<String>>(
    self,
    variables: impl IntoIterator<Item = (K, V)>,
) -> Env
```

These variables in place of the process's: the only environment the
config reads.

## Explained

```rust
pub struct Explained {
    pub settings: Vec<Line>,
    pub notes: Vec<String>,
}
```

Every setting of a config with its value, the layer that gave it and the
values it went over, then what was read and not used, each with why.

## Given

```rust
pub struct Given {
    pub value: String,
    pub from: Origin,
}
```

A value a setting was given, and where it came from.

## Line

```rust
pub struct Line {
    pub path: String,
    pub value: String,
    pub from: String,
    pub over: Vec<String>,
    pub given: Vec<Given>,
}
```

One setting's value and where it came from, as text and as data.

## Origin

```rust
pub enum Origin {
    Declaration,
    Layer { place: usize },
    File { path: String },
    Env { variable: String, dotenv: Option<String>, file: Option<String> },
    Change { version: i64, at: SystemTime },
}
```

Where a value came from. Its text is what a line's `from` says.

```text
Declaration                                          the declaration
Layer { place: 2 }                                   layer 2
File { path: "app.yaml" }                            app.yaml
Env { variable: "APP_PORT" }                         APP_PORT
Env { variable: "APP_PORT", dotenv: ".env" }         APP_PORT in .env
Env { variable: "DB_URL_FILE", file: "/run/db" }     DB_URL_FILE, /run/db
Change { version: 7, at }                            a change, version 7, 2026-10-13 14:03:05 UTC
```

## File

```rust
pub struct File { /* private fields */ }
```

A file a config reads: TOML, YAML or JSON by its extension, `.toml`,
`.yaml` or `.yml`, `.json` or `.jsonc`, a relative path the program's
working directory's. One that does not exist is refused unless it is
optional.

### File.optional

```rust
pub fn optional(path: impl Into<PathBuf>) -> File
```

A file that may be absent: then it declares nothing, and `explain`
says it was absent.

## Config

```rust
pub struct Config<T> { /* private fields */ }
```

A program's settings as one typed value, read from memory, with the
changes made while the program runs to the settings marked `Live`.

### Config.get

```rust
pub fn get(&self) -> Snapshot<T>
```

The settings as they are now: a read of memory, so a program reads
them where it uses them, on every request.

### Config.update

```rust
pub fn update(&self, change: impl FnMut(&mut T)) -> Result<()>
```

Changes live settings: the ones `change` changes are kept in kv.db,
go over every layer until they are reset, and reach every handle of
the config before this returns. All of them are kept or none is, and
another change made meanwhile stays: `change` runs again on the
settings it made, so `|s| s.rps += 1` racing another counts both.

### Config.if_version

```rust
pub fn if_version(&self, version: i64) -> IfVersion<'_, T>
```

A change made only while the config's changes are still at `version`,
as a snapshot shows it; another change made since is `conflict`.

### Config.reset

```rust
pub fn reset(&self, path: &str) -> Result<()>
```

Drops the change kept of the setting at `path`, or of every setting
of the group at it, so that the layers give them again.

### Config.reset_all

```rust
pub fn reset_all(&self) -> Result<()>
```

Drops every change kept of the config.

### Config.watch

```rust
pub fn watch(
    &self,
    watcher: impl Fn(&Snapshot<T>, Option<&Snapshot<T>>) + Send + Sync + 'static,
) -> Watching
```

Calls `watcher` with the settings now, without a previous value, then
with each change and the value before it. It runs on the thread that
made the change, after its commit, and must not block; one that falls
behind gets the newest.

### Config.explain

```rust
pub fn explain(&self) -> Explained
```

Every setting with its value, the layer that gave it and the values it
went over, then what was read and not used.

### Config.name

```rust
pub fn name(&self) -> &str
```

## IfVersion

```rust
pub struct IfVersion<'a, T> { /* private fields */ }
```

A change made only while the config is still at a version:
`config.if_version(shown.version()).update(…)`.

### IfVersion.update

```rust
pub fn update(self, change: impl FnMut(&mut T)) -> Result<()>
```

## Snapshot

```rust
pub struct Snapshot<T> { /* private fields */ }

impl Deref<Target = T>
```

The settings as they were at one version of the config's changes: a
change makes a new one, and one held keeps what it had.

### Snapshot.version

```rust
pub fn version(&self) -> i64
```

The version of the changes it holds, which `if_version` takes.

## Watching

```rust
pub struct Watching { /* private fields */ }
```

A watch of a config, which stops when it is dropped.

## Live

```rust
pub struct Live<T>(_)

impl Deref<Target = T>
```

A setting that may change while the program runs: its change is kept in
the store, goes over every layer until it is reset, and reaches every
process. It reads as its value.

### Live.new

```rust
pub fn new(value: T) -> Self
```

### Live.into_inner

```rust
pub fn into_inner(self) -> T
```

## Required

```rust
pub struct Required<T>(_)

impl Deref<Target = T>
```

A setting with no default: a layer must give it, or the config does not
open. It reads as its value.

### Required.new

```rust
pub fn new(value: T) -> Self
```

### Required.into_inner

```rust
pub fn into_inner(self) -> T
```

## Secret

```rust
pub struct Secret<T> { /* private fields */ }
```

A setting that prints as `***` wherever it goes, `Debug`, `Display`,
serde and the logs, and whose text `reveal` gives. A secret is never kept,
so that only a file or a variable gives it.

`Secret::default()` holds no value, and a layer must give it;
`Secret::new` gives it a default.

### Secret.new

```rust
pub fn new(value: T) -> Self
```

### Secret.reveal

```rust
pub fn reveal(&self) -> &T
```

The secret's value, where the program hands it to what needs it.

## ConfigBuilder

```rust
pub struct ConfigBuilder<T> { /* private fields */ }
```

A config being opened: its name, its declaration, and its layers in the
order the program writes them, each declaring again what came before.

### ConfigBuilder.file

```rust
pub fn file(self, file: impl Into<File>) -> Self
```

A file's settings over what came before: TOML, YAML or JSON by its
extension, refused when it does not exist unless `File::optional`.

### ConfigBuilder.env

```rust
pub fn env(self, env: impl Into<Env>) -> Self
```

The environment's variables over what came before: `Env::prefix("APP")`,
or `"APP"`.

### ConfigBuilder.validate

```rust
pub fn validate(self, validate: impl Fn(&T) -> Result<(), String> + Send + Sync + 'static) -> Self
```

The program's own check of the whole value, at open and before a
change is kept, for what relates two settings; it says why it refuses.

### ConfigBuilder.strict

```rust
pub fn strict(self) -> Self
```

Refuses to open while a kept change no longer fits, rather than
ignoring it and saying so in the log.

### ConfigBuilder.open

```rust
pub fn open(self) -> Result<Config<T>>
```

## aio::config::ConfigBuilder

```rust
pub struct ConfigBuilder<T> { /* private fields */ }
```

A config being opened, as `crate::config::ConfigBuilder`: its
declaration, then its layers in the order written.

### aio::config::ConfigBuilder.file

```rust
pub fn file(self, file: impl Into<File>) -> Self
```

A file's settings over what came before, as
`crate::config::ConfigBuilder::file`.

### aio::config::ConfigBuilder.env

```rust
pub fn env(self, env: impl Into<Env>) -> Self
```

The environment's variables over what came before, as
`crate::config::ConfigBuilder::env`.

### aio::config::ConfigBuilder.validate

```rust
pub fn validate(self, validate: impl Fn(&T) -> Result<(), String> + Send + Sync + 'static) -> Self
```

The program's own check of the whole value, at open and before a
change is kept. It runs on the thread that polls a change's future, or
on the store's pool for a change whose future was dropped.

### aio::config::ConfigBuilder.strict

```rust
pub fn strict(self) -> Self
```

Refuses to open while a kept change no longer fits.

### aio::config::ConfigBuilder.open

```rust
pub fn open(self) -> Result<Config<T>>
```

Opens the config as the blocking builder does: a plain call, made once
at start, which reads its files and the changes kv.db keeps.

## aio::config::Config

```rust
pub struct Config<T> { /* private fields */ }
```

A program's settings as one typed value, as `crate::config::Config`:
read from memory, its changes awaited.

### aio::config::Config.get

```rust
pub fn get(&self) -> Snapshot<T>
```

The settings as they are now: a read of memory, not awaited.

### aio::config::Config.update

```rust
pub fn update(
    &self,
    change: impl FnMut(&mut T) + Send + 'static,
) -> impl Future<Output = Result<()>> + Send + 'static
```

Changes live settings as `crate::config::Config::update` does: kept
in kv.db over every layer until they are reset, and seen by every
handle of the config before the future answers. Another change made
meanwhile stays: `change` runs again on the settings it made, eight
tries at most.

`change` and the config's `validate` run in a poll, on the thread that
polls the future, and the commit on the store's pool. The poll after
the commit hands the change to every handle, which runs each handle's
`validate` and the blocking watchers on that thread too, or on the
store's pool when the future was dropped before that poll. Once
polled, the try submitted commits and reaches every handle whether or
not the future is still awaited; a try after a conflict is made only
while it is polled.

### aio::config::Config.if_version

```rust
pub fn if_version(&self, version: i64) -> IfVersion<'_, T>
```

A change made only while the config's changes are still at `version`,
as a snapshot shows it; another change made since is `conflict`.

### aio::config::Config.reset

```rust
pub fn reset(&self, path: &str) -> impl Future<Output = Result<()>> + Send + 'static
```

Drops the change kept of the setting at `path`, or of every setting of
the group at it, so that the layers give them again. The path is
checked in the first poll, and the commit runs on the store's pool, as
`update`'s does.

### aio::config::Config.reset_all

```rust
pub fn reset_all(&self) -> impl Future<Output = Result<()>> + Send + 'static
```

Drops every change kept of the config, its commit on the store's pool.

### aio::config::Config.watch

```rust
pub fn watch(&self) -> Watching<T>
```

The settings now, then the newest after each change, as a `Stream`
that ends once the store closed, its last item `Err(Closed)`. The
watch starts here, not at the first poll, and one that falls behind
gets the newest, never every version between. It outlives the handle
it was made from; dropped, it stops.

### aio::config::Config.explain

```rust
pub fn explain(&self) -> Explained
```

Every setting with its value, the layer that gave it and the values it
went over: a read of memory.

### aio::config::Config.name

```rust
pub fn name(&self) -> &str
```

### aio::config::Config.blocking

```rust
pub fn blocking(&self) -> &Config<T>
```

The same config, for its blocking calls, a watch with a callback among
them.

## aio::config::IfVersion

```rust
pub struct IfVersion<'a, T> { /* private fields */ }
```

A change made only while the config is still at a version:
`config.if_version(shown.version()).update(…)`.

### aio::config::IfVersion.update

```rust
pub fn update(
    self,
    change: impl FnMut(&mut T) + Send + 'static,
) -> impl Future<Output = Result<()>> + Send + 'static
```

As `Config::update`, one try: `conflict` once the config moved
since the version, and `change` is not run again.

## aio::config::Watching

```rust
pub struct Watching<T> { /* private fields */ }
```

A watch of a config's settings: the settings when it was made, then the
newest after each change, until the store closes, read as a `Stream`.
It stops when dropped.

<!-- Generated by task reference from crates/tinystore through rustdoc. Edit the doc comments there, not this file. -->
