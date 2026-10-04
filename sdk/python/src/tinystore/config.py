"""An application's settings, as Go's kv.Config makes them.

The defaults, then each layer in the order given, then what update kept, each over the one before. The
server keeps what update changed field by field in kv.db and sends it to every store watching the config,
so `value` is read from memory and is new after each change.
"""

from __future__ import annotations

import asyncio
import contextlib
import copy
import dataclasses
import json
import math
import os
import typing
from collections.abc import Mapping
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, Any

from ._connection import Connection, Link, check_name, handle_on
from ._values import convert, to_json
from ._wire.messages import METHODS, KvBucket, KvCall, KvConfigure, KvKept
from .errors import ClosedError, InvalidError

if TYPE_CHECKING:
    from collections.abc import Callable, Sequence


def env_name(prefix: str, path: str) -> str:
    """A field's variable: the prefix and its path in upper snake case, as Go's and Bun's configs name it.

    "APP", "limits.max_rps" → APP_LIMITS_MAX_RPS
    """
    name: list[str] = []
    for i, c in enumerate(path):
        if c in ".-":
            name.append("_")
            continue
        if i > 0 and c.isupper():
            before = path[i - 1]
            lower_after = i + 1 < len(path) and path[i + 1].islower()
            if before.islower() or before.isdigit() or (before.isupper() and lower_after):
                name.append("_")
        name.append(c.upper())
    joined = "".join(name)
    return f"{prefix}_{joined}" if prefix else joined


def parse_dotenv(text: str) -> dict[str, str]:
    """The variables of a .env file, as Go's config reads one.

    NAME=value lines, export before a name, # comments, a comment after a space, double quotes
    reading \\n \\r \\t \\" and \\\\, single quotes reading nothing, a quoted value over lines;
    a later name wins.
    """
    values: dict[str, str] = {}
    line = 1
    while text:
        name, value, rest = _dotenv_assignment(text, line)
        line += text[: len(text) - len(rest)].count("\n")
        if name:
            values[name] = value
        text = rest
    return values


def _dotenv_assignment(text: str, line_number: int) -> tuple[str, str, str]:
    line, _, rest = text.partition("\n")
    trimmed = line.strip()
    if not trimmed or trimmed.startswith("#"):
        return "", "", rest
    equals = line.find("=")
    if equals < 0:
        raise InvalidError(f"line {line_number}: {line!r} is no NAME=value")
    name = line[:equals].strip().removeprefix("export ").strip()
    if not _valid_env_name(name):
        raise InvalidError(f"line {line_number}: {name!r} is no variable's name")
    start = equals + 1
    while start < len(line) and line[start] in " \t":
        start += 1
    if start < len(line) and line[start] in "\"'":
        value, rest = _dotenv_quoted(text[start:], line_number)
        return name, value, rest
    value = line[start:]
    comment = value.find(" #")
    if comment >= 0:
        value = value[:comment]
    return name, value.rstrip(" \t\r"), rest


def _dotenv_quoted(text: str, line_number: int) -> tuple[str, str]:
    quote, out, i = text[0], list[str](), 1
    while i < len(text):
        c = text[i]
        if c == quote:
            _, _, rest = text[i + 1 :].partition("\n")
            return "".join(out), rest
        if c == "\\" and quote == '"' and i + 1 < len(text):
            i += 1
            out.append({"n": "\n", "r": "\r", "t": "\t"}.get(text[i], text[i]))
        else:
            out.append(c)
        i += 1
    raise InvalidError(f"line {line_number}: a value opened with {quote} is never closed")


def _valid_env_name(name: str) -> bool:
    return bool(name) and all(c == "_" or c.isalpha() or (i > 0 and c.isdigit()) for i, c in enumerate(name))


_MARK = "tinystore"


@dataclass(frozen=True, slots=True)
class _Mark:
    secret: bool
    variable: str | None


def secret(variable: str | None = None, *, default: str | None = None) -> Any:
    """A secret field of a config's dataclass, never kept by update and shown as *** by sources().

    It reads its own variable when given one, and is required without a default::

        url: str = secret("DATABASE_URL")
    """
    metadata = {_MARK: _Mark(secret=True, variable=variable)}
    if default is None:
        return dataclasses.field(metadata=metadata)
    return dataclasses.field(default=default, metadata=metadata)


@dataclass(frozen=True, slots=True)
class FromEnv:
    """The environment as a layer of a config, which from_env makes."""

    prefix: str
    files: tuple[str | os.PathLike[str], ...]


def from_env(prefix: str = "", *files: str | os.PathLike[str]) -> FromEnv:
    """The environment as a config's layer: from_env("APP") reads db.pool from APP_DB_POOL, from_env() from DB_POOL.

    The .env files given are read first, the process's own variables over them, a missing file skipped.
    """
    return FromEnv(prefix, files)


@dataclass(frozen=True, slots=True)
class _Field:
    sample: Any
    """its default, or a value of its kind for a field without one"""
    secret: bool
    required: bool
    variable: str | None


@dataclass(frozen=True, slots=True)
class Source:
    """Where a field's value came from."""

    path: str
    value: str
    """its JSON, or *** for a secret"""
    from_: str
    """'default', 'file', 'env NAME' or 'kept'"""
    ignored: str | None = None
    """why a kept value is left out, when it is"""


class Config[T]:
    """A config: open it with `await store.kv.config(name, Type, *layers)`, whose defaults are its own."""

    def __init__(
        self,
        link: Link,
        name: str,
        of: type[T],
        layers: Sequence[Mapping[str, Any] | FromEnv],
        validate: Callable[[T], object] | None,
    ) -> None:
        check_name(name, "config")
        self.name, self._link, self._of, self._validate = name, link, of, validate
        self._open = KvBucket.encode(name=name, config=True)
        self._fields, self._base = _shape(of)
        self._from = {path: "default" for path, f in self._fields.items() if not f.required}
        self._lay(layers)
        self._kept: dict[str, str] = {}
        self._ignored: dict[str, str] = {}
        self._tree = copy.deepcopy(self._base)
        self._value = self._made(self._tree)
        self._listeners: list[Callable[[T], object]] = []
        self._task: asyncio.Task[None] | None = None
        self._stream: Any = None

    @property
    def value(self) -> T:
        """The config now, made anew after each change; update changes it."""
        return self._value

    async def update(self, change: Mapping[str, Any]) -> None:
        """Changes the fields change names, at any depth: each one that changed is checked, kept, and seen at once.

        A change validate refuses, one of a secret, or one leaving a required field empty, is InvalidError and
        keeps nothing.
        """
        nxt = _merged(copy.deepcopy(self._tree), change)
        changed: list[str] = []
        for path, field in self._fields.items():
            now = json.dumps(_path_in(nxt, path), separators=(",", ":"), ensure_ascii=False)
            if now == json.dumps(_path_in(self._tree, path), separators=(",", ":"), ensure_ascii=False):
                continue
            if field.secret:
                raise InvalidError(
                    f"config {self.name}: {path} is a secret, set by the defaults, a file or the environment alone"
                )
            if field.required and _missing(_path_in(nxt, path)):
                raise InvalidError(f"config {self.name}: {path} is required")
            changed += [path, now]
        if not changed:
            return
        self._made(nxt)
        await self._send(changed, [])

    async def reset(self, *paths: str) -> None:
        """Forgets what update kept for paths, each a field or a group of them; all of it without paths."""
        for path in paths:
            if not any(_under(leaf, path) for leaf in self._fields):
                raise InvalidError(f"config {self.name} has no field {path}")
        reset = [kept for kept in self._kept if not paths or any(_under(kept, path) for path in paths)]
        if reset:
            await self._send([], reset)

    def watch(self, fn: Callable[[T], object]) -> Callable[[], None]:
        """Calls fn with the config now and after each change, whoever made it; what it returns stops that."""
        self._listeners.append(fn)
        fn(self._value)
        return lambda: self._listeners.remove(fn) if fn in self._listeners else None

    def sources(self) -> list[Source]:
        """Where each field's value came from, and why a kept value is left out."""
        sources: list[Source] = []
        for path, field in self._fields.items():
            kept = path in self._kept and path not in self._ignored
            value = "***" if field.secret else json.dumps(_path_in(self._tree, path), ensure_ascii=False)
            sources.append(Source(path, value, "kept" if kept else self._from[path], self._ignored.get(path)))
        return sources

    async def start(self) -> None:
        """Follows the config's changes until the store closes; returns once the first state is read."""
        ready: asyncio.Future[None] = asyncio.get_running_loop().create_future()
        self._task = asyncio.create_task(self._follow(ready))
        await ready

    def stop(self) -> None:
        """Stops following the config, as the store does when it closes."""
        if self._stream is not None:
            self._stream.cancel()
        if self._task is not None:
            self._task.cancel()

    async def _follow(self, ready: asyncio.Future[None]) -> None:
        pause = 0.1
        while True:
            try:
                await self._link.run("read", lambda connection: self._watch(connection, ready))
            except asyncio.CancelledError:
                raise
            except Exception as err:
                if not ready.done():
                    ready.set_exception(err)
                    return
                if isinstance(err, ClosedError):
                    return
            await asyncio.sleep(pause)
            pause = min(pause * 2, 5.0)

    async def _watch(self, connection: Connection, ready: asyncio.Future[None]) -> None:
        handle = await handle_on(connection, METHODS["kv.open"], self._open)
        stream = await connection.session.open(METHODS["kv.watch"], KvCall.encode(handle=handle), True)
        self._stream = stream
        await stream.next()
        while True:
            event = await stream.next()
            stream.consumed(len(event.body))
            if event.end:
                return
            fields: list[str] = KvKept.decode(event.body).get("fields") or []
            self._build(dict(zip(fields[::2], fields[1::2], strict=True)))
            if not ready.done():
                ready.set_result(None)

    async def _send(self, set_: list[str], reset: list[str]) -> None:
        async def attempt(connection: Connection) -> None:
            handle = await handle_on(connection, METHODS["kv.open"], self._open)
            body = KvConfigure.encode(handle=handle, set=set_ or None, reset=reset or None)
            await connection.session.call(METHODS["kv.configure"], body)

        await self._link.run("write", attempt)
        kept = dict(self._kept)
        kept.update(zip(set_[::2], set_[1::2], strict=True))
        for path in reset:
            kept.pop(path, None)
        self._build(kept)

    def _build(self, kept: dict[str, str]) -> None:
        """Makes the config from its base and what is kept, leaving out what no longer fits, and tells the watchers."""
        ignored: dict[str, str] = {}
        tree = _with_kept(self._base, self._fields, kept, ignored)
        try:
            value = self._made(tree)
        except InvalidError as err:
            if len(kept) == len(ignored):
                raise
            for path in kept:
                ignored.setdefault(path, f"the config fails with it: {err}")
            tree = copy.deepcopy(self._base)
            value = self._made(tree)
        changed = tree != self._tree
        self._kept, self._ignored, self._tree, self._value = kept, ignored, tree, value
        if changed:
            for listener in list(self._listeners):
                listener(value)

    def _made(self, tree: dict[str, Any]) -> T:
        """The config's type made from a tree, validate's refusal as InvalidError."""
        try:
            value = convert(copy.deepcopy(tree), self._of)
            if self._validate is not None:
                self._validate(value)
        except InvalidError:
            raise
        except Exception as err:
            raise InvalidError(f"config {self.name}: {err}") from err
        return value

    def _lay(self, layers: Sequence[object]) -> None:
        """Lays each layer over the defaults, the one before it under it, and checks what is required."""
        prefix: str | None = None
        for layer in layers:
            if isinstance(layer, FromEnv):
                self._lay_environment(layer)
                prefix = layer.prefix
            elif isinstance(layer, Mapping):
                self._lay_file(typing.cast("Mapping[str, Any]", layer))
            else:
                raise InvalidError(f"config {self.name}: a layer is a file's values or from_env(), not {layer!r}")
        for path, field in self._fields.items():
            if field.required and _missing(_path_in(self._base, path)):
                variable = None if prefix is None else field.variable or env_name(prefix, path)
                needs = "" if variable is None else f": set {variable}"
                raise InvalidError(f"config {self.name}: {path} is required{needs}")

    def _lay_file(self, values: Mapping[str, Any]) -> None:
        for path, value in _leaves_of(dict(values)):
            field = self._fields.get(path)
            if field is None:
                continue
            if not _fits(value, field.sample):
                raise InvalidError(f"the file's {path}: {value!r} is no {_kind(field.sample)}")
            _set_path(self._base, path, value)
            self._from[path] = "file"

    def _lay_environment(self, layer: FromEnv) -> None:
        variables = _read_env_files(layer.files)
        variables.update(os.environ)
        for path, field in self._fields.items():
            name = field.variable or env_name(layer.prefix, path)
            if name in variables:
                _set_path(self._base, path, _read_variable(variables[name], field.sample, name))
                self._from[path] = f"env {name}"


def _shape(of: type) -> tuple[dict[str, _Field], dict[str, Any]]:
    """A config type's fields by their paths, and its defaults as a tree."""
    if dataclasses.is_dataclass(of):
        return _dataclass_shape(of, dataclasses.MISSING, "")
    defaults = json.loads(to_json(of()))
    return {path: _Field(value, False, False, None) for path, value in _leaves_of(defaults)}, defaults


def _dataclass_shape(of: type, given: Any, parent: str) -> tuple[dict[str, _Field], dict[str, Any]]:
    """A dataclass's fields under parent, their defaults those of given when there is one."""
    fields: dict[str, _Field] = {}
    tree: dict[str, Any] = {}
    hints = typing.get_type_hints(of)
    for f in dataclasses.fields(of):
        path = f"{parent}.{f.name}" if parent else f.name
        kind = hints.get(f.name, Any)
        default = _default_of(f) if given is dataclasses.MISSING else getattr(given, f.name)
        if dataclasses.is_dataclass(kind) and isinstance(kind, type):
            inner = default if isinstance(default, kind) else dataclasses.MISSING
            inner_fields, tree[f.name] = _dataclass_shape(kind, inner, path)
            fields |= inner_fields
            continue
        mark = f.metadata.get(_MARK) or _Mark(secret=False, variable=None)
        if default is dataclasses.MISSING:
            fields[path] = _Field(_sample(kind), mark.secret, True, mark.variable)
            continue
        tree[f.name] = json.loads(to_json(default))
        fields[path] = _Field(tree[f.name], mark.secret, False, mark.variable)
    return fields, tree


def _default_of(f: dataclasses.Field[Any]) -> Any:
    if f.default is not dataclasses.MISSING:
        return f.default
    if f.default_factory is not dataclasses.MISSING:
        # a dataclass whose fields have no defaults cannot be made without them
        with contextlib.suppress(TypeError):
            return f.default_factory()
    return dataclasses.MISSING


def _sample(kind: Any) -> Any:
    """A value of a required field's kind, which its variable is read as."""
    if kind in (str, int, float, bool):
        return kind()
    if kind is list or typing.get_origin(kind) is list:
        return []
    return None


def _missing(value: Any) -> bool:
    return value is None or value == ""


def _read_env_files(files: Sequence[str | os.PathLike[str]]) -> dict[str, str]:
    """The variables of .env files, a later file's over an earlier's; a file that is not there is skipped."""
    variables: dict[str, str] = {}
    for path in files:
        with contextlib.suppress(FileNotFoundError):
            variables.update(parse_dotenv(Path(path).read_text(encoding="utf-8")))
    return variables


def _read_variable(text: str, wanted: Any, name: str) -> Any:
    """A variable's text as a value of its default's kind.

    A number, true or false, a list split at its commas or given as JSON, and anything else as JSON:
    8080 ← "3000" → 3000, ["x"] ← "a.com, b.com" → ["a.com", "b.com"]
    """
    trimmed = text.strip()
    if isinstance(wanted, str):
        return text
    if isinstance(wanted, bool):
        if trimmed in ("1", "t", "T", "TRUE", "true", "True"):
            return True
        if trimmed in ("0", "f", "F", "FALSE", "false", "False"):
            return False
        raise InvalidError(f"{name}: {text!r} is not true or false")
    if isinstance(wanted, int):
        try:
            return int(trimmed)
        except ValueError:
            raise InvalidError(f"{name}: {text!r} is no integer") from None
    if isinstance(wanted, float):
        try:
            number = float(trimmed)
        except ValueError:
            number = math.nan
        if not math.isfinite(number):
            raise InvalidError(f"{name}: {text!r} is no number")
        return number
    if isinstance(wanted, list) and not trimmed.startswith("["):
        items = typing.cast("list[Any]", wanted)
        item: Any = items[0] if items else ""
        return [] if not trimmed else [_read_variable(part.strip(), item, name) for part in trimmed.split(",")]
    try:
        value = json.loads(trimmed)
    except ValueError:
        value = _NOT_JSON
    if value is not _NOT_JSON and _fits(value, wanted):
        return value
    if wanted is None:
        return text
    raise InvalidError(f"{name}: {text!r} is no {_kind(wanted)}")


_NOT_JSON = object()


def _leaves_of(value: dict[str, Any], parent: str = "") -> list[tuple[str, Any]]:
    """A tree's fields to the last that is not an object, by their dotted paths."""
    leaves: list[tuple[str, Any]] = []
    for key, inner in value.items():
        path = f"{parent}.{key}" if parent else key
        if isinstance(inner, dict):
            leaves += _leaves_of(inner, path)  # pyright: ignore[reportUnknownArgumentType]
        else:
            leaves.append((path, inner))
    return leaves


def _fits(value: Any, wanted: Any) -> bool:
    """A value fits a field of its default's kind; an int fits a float, a default of None anything."""
    if wanted is None:
        return True
    if isinstance(wanted, bool) or isinstance(value, bool):
        return isinstance(value, bool) and isinstance(wanted, bool)
    if isinstance(wanted, float):
        return isinstance(value, (int, float))
    return isinstance(value, type(wanted))


def _kind(wanted: Any) -> str:
    for kind, name in ((bool, "bool"), (int, "integer"), (float, "number"), (str, "string"), (list, "list")):
        if isinstance(wanted, kind):
            return name
    return "object"


def _with_kept(
    base: dict[str, Any], fields: dict[str, _Field], kept: dict[str, str], ignored: dict[str, str]
) -> dict[str, Any]:
    """The base with each kept value that fits its field over it; a secret is never taken from what was kept."""
    tree = copy.deepcopy(base)
    for path, spelled in kept.items():
        field = fields.get(path)
        if field is None:
            ignored[path] = "the config has no such field"
            continue
        if field.secret:
            ignored[path] = "the field is a secret"
            continue
        try:
            value = json.loads(spelled)
        except ValueError:
            ignored[path] = f"{spelled} is no JSON"
            continue
        if not _fits(value, field.sample):
            ignored[path] = f"{spelled} is no {_kind(field.sample)}"
            continue
        if field.required and _missing(value):
            ignored[path] = "the field is required"
            continue
        _set_path(tree, path, value)
    return tree


def _path_in(tree: Any, path: str) -> Any:
    at: Any = tree
    for name in path.split("."):
        if not isinstance(at, dict):
            return None
        at = typing.cast("dict[str, Any]", at).get(name)
    return at


def _set_path(tree: dict[str, Any], path: str, value: Any) -> None:
    *parents, last = path.split(".")
    for name in parents:
        inner = tree.get(name)
        if not isinstance(inner, dict):
            inner = {}
            tree[name] = inner
        tree = inner  # pyright: ignore[reportUnknownVariableType]
    tree[last] = value


def _merged(tree: dict[str, Any], change: Mapping[str, Any]) -> dict[str, Any]:
    """change merged into tree at every depth; a list is replaced whole."""
    for key, value in change.items():
        inner = tree.get(key)
        if isinstance(value, dict) and isinstance(inner, dict):
            tree[key] = _merged(inner, value)  # pyright: ignore[reportUnknownArgumentType]
        else:
            tree[key] = value
    return tree


def _under(path: str, given: str) -> bool:
    return path == given or path.startswith(given + ".")
