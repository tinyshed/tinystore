# Config API for Python

Every public class, function and type of the Config engine in `tinystore`, generated from its source. The [Config guide](../../guides/config/README.md) explains how to use them, and the [Bun and Node](../bun/config.md), [Go](../go/config.md) and [Rust](../rust/config.md) pages list the same API.

## Secret

```python
class Secret:
    ...
```

A setting's text that prints as *** wherever it goes; reveal gives it.

### Secret.reveal

```python
def reveal() -> str: ...
```

The secret's text, where the program hands it to what needs it.

## live

```python
@overload
def live[T](default: T, /) -> T: ...
```

## live

```python
@overload
def live() -> Any: ...
```

## live

```python
def live(default: Any = _REQUIRED, /) -> Any: ...
```

A setting that may change while the program runs: ``rps: int = live(100)``.

Its change is kept in the store, goes over every layer until it is reset,
and reaches every process; ``live()`` is one a layer must give at start.

## secret

```python
def secret(default: str | None = None, /) -> Secret: ...
```

A setting that prints as *** wherever it goes and only a layer gives: ``url: Secret = secret()``.

``secret()`` must be given; ``secret("")`` is empty unless a layer gives it.

## Range

```python
@dataclass(frozen=True, slots=True)
class Range:
    min: float | timedelta | str | None = None
    max: float | timedelta | str | None = None
```

The bounds of a number or a span: ``Annotated[int, Range(1, 65535)]``.

## Items

```python
@dataclass(frozen=True, slots=True)
class Items:
    min: int | None = None
    max: int | None = None
```

How many items a list holds at least and at most: ``Annotated[list[str], Items(1, 20)]``.

## file

```python
def file(path: str | os.PathLike[str], *, optional: bool = False) -> FileLayer: ...
```

A file a config reads, TOML, YAML or JSON by its extension; a relative path is the working directory's.

One that does not exist is InvalidError unless it is optional: then it
declares nothing, and explain says so.

## env

```python
def env(
    prefix: str = '',
    *,
    vars: Mapping[str, str | Mapping[str, Any]] | None = None,
    files: Sequence[str | os.PathLike[str]] = (),
    from_: Mapping[str, str] | None = None,
) -> EnvLayer: ...
```

The environment a config reads: ``env("APP")`` reads APP_ and each setting's path in upper snake case.

- ``vars``: a setting's variable of its own by its path, in place of its
  prefixed one: ``{"port": "PORT", "db.url": "DATABASE_URL"}``.
- ``files``: .env files, whose variables count where the process has none.
- ``from_``: these variables in place of the process's, as a test gives them.

## Origin

```python
@dataclass(frozen=True, slots=True, repr=False)
class Origin:
    kind: Literal['declaration', 'layer', 'file', 'env', 'change']
    place: int | None = None  # a layer's place in the list, the declaration 1
    path: str | None = None  # a file's path
    variable: str | None = None  # an env's variable
    dotenv: str | None = None  # the .env file that set the variable
    file: str | None = None  # the file a NAME_FILE variable named
    version: int | None = None  # a change's version
    at: datetime | None = None  # when a change was made, in UTC
```

Where a value came from: its kind, and only the fields that kind names, the others None.

``str(origin)`` is the text a line's ``from_`` says: ``the declaration``, ``layer 2``,
``app.yaml``, ``APP_PORT in .env``, ``a change, version 7, 2026-10-13 14:03:05 UTC``.
A kind from a newer server is kept as it was sent, and its text is the kind.

## Given

```python
@dataclass(frozen=True, slots=True)
class Given:
    value: str  # as JSON, a span spelled, a secret as ***
    from_: Origin
```

A value a setting was given, and where it came from.

## Line

```python
@dataclass(frozen=True, slots=True)
class Line:
    path: str
    value: str  # as JSON, a span spelled, a secret as ***
    from_: str  # the layer that gave it: APP_PORT, app.yaml, layer 2, the declaration, a change
    over: str  # the values it went over, newest first
    given: tuple[Given, ...] = dataclasses.field(default=(), repr=False)  # Every value the setting was given, newest first: its own, then the ones it went over. from_ is str(given[0].from_). It is empty from a server before 0.1.1, and print(line) leaves it out.
```

A setting's line of explain.

## Explained

```python
class Explained(list[Line]):
    notes: list[str]
```

Every setting's line, and in notes what was read and not used, each with why.

## Config

```python
class Config[T]:
    name: str
```

A program's settings as one typed value, read from memory: ``await store.config(name, [Settings, …])``.

### Config.open

```python
@classmethod
async def open(
    link: Link,
    name: str,
    layers: Sequence[Any],
    validate: Callable[[Any], object] | type | None,
    strict: bool,
) -> Config[T]: ...
```

Opens a config from its list: the declaration first, then its layers in order.

### Config.value

```python
@property
def value() -> T: ...
```

The settings as they are now: a read of memory, so read it where it is used.

### Config.version

```python
@property
def version() -> int: ...
```

The version of the changes the value holds, which if_version takes.

### Config.update

```python
async def update(
    changes: Mapping[str, Any],
    *,
    if_version: int | None = None,
) -> None: ...
```

Changes live settings: kept in the store, over every layer until reset, seen by every handle.

The value they make passes validate first; all are kept or none. With
``if_version``, one another change came before is ConflictError.

### Config.reset

```python
async def reset(path: str = '') -> None: ...
```

Drops the change kept of the setting or the group at path, or of every setting.

### Config.watch

```python
async def watch() -> AsyncIterator[T]: ...
```

The settings now, then after every change made in this process or another; one behind gets the newest.

It ends when the store closes.

### Config.explain

```python
def explain() -> Explained: ...
```

Every setting with its value, the layer that gave it and what it went over; notes, what was not used.

### Config.on_change

```python
def on_change(watcher: Callable[[T], None]) -> None: ...
```

Calls watcher with the settings now and after every change, for what follows a config inside the SDK.

### Config.stop

```python
async def stop() -> None: ...
```

Stops following the config: the store closes.

<!-- Generated by task reference from sdk/python/src/tinystore/config.py. Edit the doc comments there, not this file. -->
