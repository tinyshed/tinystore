"""An application's settings, as Go's kv.Config makes them.

The defaults, then a file's values, then the environment, then what update kept, each over the one
before. The server keeps what update changed field by field in kv.db and sends it to every store
watching the config, so `value` is read from memory and is new after each change.
"""

from __future__ import annotations

import asyncio
import contextlib
import copy
import json
import math
import os
import typing
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, Any

from ._connection import Connection, Link, check_name, handle_on
from ._values import convert, to_json
from ._wire.messages import METHODS, KvBucket, KvCall, KvConfigure, KvKept
from .errors import ClosedError, InvalidError

if TYPE_CHECKING:
    from collections.abc import Callable, Iterable, Mapping, Sequence


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
    """A config: open it with `await store.kv.config(name, Type)`, whose defaults are its own."""

    def __init__(
        self,
        link: Link,
        name: str,
        of: type[T],
        *,
        file: Mapping[str, Any] | None,
        prefix: str,
        env: Mapping[str, str] | bool,
        env_file: str | os.PathLike[str] | Sequence[str | os.PathLike[str]] | None,
        secret: Iterable[str],
        validate: Callable[[T], object] | None,
    ) -> None:
        check_name(name, "config")
        self.name, self._link, self._of, self._validate = name, link, of, validate
        self._open = KvBucket.encode(name=name, config=True)
        defaults = json.loads(to_json(of()))
        self._leaves = dict(_leaves_of(defaults))
        self._secret = set(secret)
        for path in self._secret:
            if path not in self._leaves:
                raise InvalidError(f"config {name} has no field {path}")
        self._from = dict.fromkeys(self._leaves, "default")
        self._base = self._layered(defaults, file, prefix, env, env_file)
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

        A change validate refuses, or one of a secret, is InvalidError and keeps nothing.
        """
        nxt = _merged(copy.deepcopy(self._tree), change)
        changed: list[str] = []
        for path in self._leaves:
            now = json.dumps(_path_in(nxt, path), separators=(",", ":"), ensure_ascii=False)
            if now == json.dumps(_path_in(self._tree, path), separators=(",", ":"), ensure_ascii=False):
                continue
            if path in self._secret:
                raise InvalidError(
                    f"config {self.name}: {path} is a secret, set by the defaults, the file or the environment alone"
                )
            changed += [path, now]
        if not changed:
            return
        self._made(nxt)
        await self._send(changed, [])

    async def reset(self, *paths: str) -> None:
        """Forgets what update kept for paths, each a field or a group of them; all of it without paths."""
        for path in paths:
            if not any(_under(leaf, path) for leaf in self._leaves):
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
        for path in self._leaves:
            kept = path in self._kept and path not in self._ignored
            value = "***" if path in self._secret else json.dumps(_path_in(self._tree, path), ensure_ascii=False)
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
        tree = _with_kept(self._base, self._leaves, kept, ignored)
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

    def _layered(
        self,
        defaults: dict[str, Any],
        file: Mapping[str, Any] | None,
        prefix: str,
        env: Mapping[str, str] | bool,
        env_file: str | os.PathLike[str] | Sequence[str | os.PathLike[str]] | None,
    ) -> dict[str, Any]:
        base = copy.deepcopy(defaults)
        for path, value in _leaves_of(dict(file or {})):
            if path not in self._leaves:
                continue
            if not _fits(value, self._leaves[path]):
                raise InvalidError(f"the file's {path}: {value!r} is no {_kind(self._leaves[path])}")
            _set_path(base, path, value)
            self._from[path] = "file"
        if env is False:
            return base
        names = dict(env) if isinstance(env, dict) else {}
        variables = _read_env_files(env_file)
        variables.update(os.environ)
        for path, wanted in self._leaves.items():
            name = names.get(path) or env_name(prefix, path)
            if name in variables:
                _set_path(base, path, _from_env(variables[name], wanted, name))
                self._from[path] = f"env {name}"
        return base


def _read_env_files(files: str | os.PathLike[str] | Sequence[str | os.PathLike[str]] | None) -> dict[str, str]:
    """The variables of .env files, a later file's over an earlier's; a file that is not there is skipped."""
    if files is None:
        return {}
    paths = [files] if isinstance(files, (str, os.PathLike)) else list(files)
    variables: dict[str, str] = {}
    for path in paths:
        with contextlib.suppress(FileNotFoundError):
            variables.update(parse_dotenv(Path(path).read_text(encoding="utf-8")))
    return variables


def _from_env(text: str, wanted: Any, name: str) -> Any:
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
        return [] if not trimmed else [_from_env(part.strip(), item, name) for part in trimmed.split(",")]
    try:
        value = json.loads(trimmed)
    except ValueError:
        value = _MISSING
    if value is not _MISSING and _fits(value, wanted):
        return value
    if wanted is None:
        return text
    raise InvalidError(f"{name}: {text!r} is no {_kind(wanted)}")


_MISSING = object()


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
    base: dict[str, Any], leaves: dict[str, Any], kept: dict[str, str], ignored: dict[str, str]
) -> dict[str, Any]:
    tree = copy.deepcopy(base)
    for path, spelled in kept.items():
        if path not in leaves:
            ignored[path] = "the config has no such field"
            continue
        try:
            value = json.loads(spelled)
        except ValueError:
            ignored[path] = f"{spelled} is no JSON"
            continue
        if not _fits(value, leaves[path]):
            ignored[path] = f"{spelled} is no {_kind(leaves[path])}"
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
