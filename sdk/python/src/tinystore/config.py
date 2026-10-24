"""A program's settings as one typed value, as docs/guides/config/README.md has it.

A dataclass declares every setting, its type and its default; then objects,
files and the environment, in the order the program writes them, each
declaring again what came before; and the live settings' changes, which the
store keeps and every process sees. The core reads the layers, checks the
rules and keeps the changes. This module spells the declaration as data,
reads the program's own files and variables, which belong to it and not to
the server, and follows the config on one stream that every handle and
watcher of it in the process shares::

    @dataclass(kw_only=True)
    class Settings:
        port: int = 8080
        db: Db = field(default_factory=Db)
        limits: Limits = field(default_factory=Limits)   # rps: int = live(100)
        region: str                                      # no default: required

    config = await store.config("app", [Settings, file("app.yaml"), env("APP")])
"""

from __future__ import annotations

import asyncio
import contextlib
import dataclasses
import inspect
import json
import os
import re
import sys
import types
import typing
import weakref
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from datetime import datetime, timedelta
from pathlib import Path
from typing import TYPE_CHECKING, Annotated, Any, Literal, Self, cast, overload

from ._handles import open_call
from ._session import ConnectionLostError
from ._time import moment, ms
from ._wire.protocol import ConfigOpen, ConfigReset, ConfigUpdate, ConfigValue, Handle, methods
from .errors import CallCancelledError, ConflictError, InvalidError, UnavailableError

if TYPE_CHECKING:
    from collections.abc import AsyncIterator, Awaitable, Callable

    from ._connection import Connection, Idempotence, Link

_ATTEMPTS = 8
"""The tries a change makes while other changes race it."""


class _Missing:
    def __repr__(self) -> str:
        return "required"


_REQUIRED = _Missing()
"""A setting's default when it has none, and a layer must give it."""


class Secret:
    """A setting's text that prints as *** wherever it goes; reveal gives it."""

    __slots__ = ("_text",)

    def __init__(self, text: str) -> None:
        self._text = text

    def reveal(self) -> str:
        """The secret's text, where the program hands it to what needs it."""
        return self._text

    def __repr__(self) -> str:
        return "***"

    def __str__(self) -> str:
        return "***"

    def __format__(self, spec: str) -> str:
        return "***"

    def __eq__(self, other: object) -> bool:
        return isinstance(other, Secret) and other._text == self._text

    def __hash__(self) -> int:
        return hash(self._text)

    def __reduce__(self) -> str | tuple[Any, ...]:
        raise TypeError("a secret is not pickled: reveal() gives its text where it is needed")


@dataclass(frozen=True, eq=False)
class _Mark:
    """What live and secret put where a field's default goes: the default, and what the setting is."""

    default: Any
    live: bool
    secret: bool


@overload
def live[T](default: T, /) -> T: ...
@overload
def live() -> Any: ...
def live(default: Any = _REQUIRED, /) -> Any:
    """A setting that may change while the program runs: ``rps: int = live(100)``.

    Its change is kept in the store, goes over every layer until it is reset,
    and reaches every process; ``live()`` is one a layer must give at start.
    """
    if isinstance(default, _Mark):
        raise InvalidError("a secret is never live" if default.secret else "live twice")
    return _Mark(default, live=True, secret=False)


def secret(default: str | None = None, /) -> Secret:
    """A setting that prints as *** wherever it goes and only a layer gives: ``url: Secret = secret()``.

    ``secret()`` must be given; ``secret("")`` is empty unless a layer gives it.
    """
    if default is not None and not isinstance(default, str):  # pyright: ignore[reportUnnecessaryIsInstance]
        raise InvalidError("a secret is text: secret(), or secret with its default text")
    return cast("Secret", _Mark(_REQUIRED if default is None else default, live=False, secret=True))


@dataclass(frozen=True, slots=True)
class Range:
    """The bounds of a number or a span: ``Annotated[int, Range(1, 65535)]``."""

    min: float | timedelta | str | None = None
    max: float | timedelta | str | None = None


@dataclass(frozen=True, slots=True)
class Items:
    """How many items a list holds at least and at most: ``Annotated[list[str], Items(1, 20)]``."""

    min: int | None = None
    max: int | None = None


@dataclass(frozen=True, slots=True)
class FileLayer:
    """A file a config reads: TOML, YAML or JSON by its extension."""

    path: str
    optional: bool


@dataclass(frozen=True, slots=True)
class EnvLayer:
    """The environment a config reads."""

    prefix: str
    vars: Mapping[str, str]
    """a setting's own variable, by its path"""
    files: tuple[str, ...]
    from_: Mapping[str, str] | None


Layer = type | Mapping[str, Any] | FileLayer | EnvLayer | Literal[False] | None
"""A layer of a config's list: the declaration first, then a dict, a file, the environment, or nothing."""


def file(path: str | os.PathLike[str], *, optional: bool = False) -> FileLayer:
    """A file a config reads, TOML, YAML or JSON by its extension; a relative path is the working directory's.

    One that does not exist is InvalidError unless it is optional: then it
    declares nothing, and explain says so.
    """
    return FileLayer(str(path), optional)


def env(
    prefix: str = "",
    *,
    vars: Mapping[str, str | Mapping[str, Any]] | None = None,
    files: Sequence[str | os.PathLike[str]] = (),
    from_: Mapping[str, str] | None = None,
) -> EnvLayer:
    """The environment a config reads: ``env("APP")`` reads APP_ and each setting's path in upper snake case.

    - ``vars``: a setting's variable of its own by its path, in place of its
      prefixed one: ``{"port": "PORT", "db.url": "DATABASE_URL"}``.
    - ``files``: .env files, whose variables count where the process has none.
    - ``from_``: these variables in place of the process's, as a test gives them.
    """
    return EnvLayer(prefix, _flattened(vars or {}, ""), tuple(str(each) for each in files), from_)


def _flattened(given: Mapping[str, str | Mapping[str, Any]], path: str) -> dict[str, str]:
    out: dict[str, str] = {}
    for key, value in given.items():
        at = key if path == "" else f"{path}.{key}"
        if isinstance(value, str):
            out[at] = value
        else:
            out.update(_flattened(value, at))
    return out


@dataclass(frozen=True, slots=True, repr=False)
class Origin:
    """Where a value came from: its kind, and only the fields that kind names, the others None.

    ``str(origin)`` is the text a line's ``from_`` says: ``the declaration``, ``layer 2``,
    ``app.yaml``, ``APP_PORT in .env``, ``a change, version 7, 2026-10-13 14:03:05 UTC``.
    A kind from a newer server is kept as it was sent, and its text is the kind.
    """

    kind: Literal["declaration", "layer", "file", "env", "change"]
    place: int | None = None
    """a layer's place in the list, the declaration 1"""
    path: str | None = None
    """a file's path"""
    variable: str | None = None
    """an env's variable"""
    dotenv: str | None = None
    """the .env file that set the variable"""
    file: str | None = None
    """the file a NAME_FILE variable named"""
    version: int | None = None
    """a change's version"""
    at: datetime | None = None
    """when a change was made, in UTC"""

    def __str__(self) -> str:
        # the core's text: crates/tinystore/src/kv/config/explain.rs, Origin's Display
        match self.kind:
            case "declaration":
                return "the declaration"
            case "layer":
                return f"layer {self.place}"
            case "file":
                return self.path or ""
            case "env" if self.dotenv is not None:
                return f"{self.variable} in {self.dotenv}"
            case "env" if self.file is not None:
                return f"{self.variable}, {self.file}"
            case "env":
                return self.variable or ""
            case "change":
                at = self.at.strftime("%Y-%m-%d %H:%M:%S") if self.at is not None else ""
                return f"a change, version {self.version}, {at} UTC"
            case _:
                # a kind from a newer server says itself
                return str(self.kind)

    def __repr__(self) -> str:
        fields = ((each.name, getattr(self, each.name)) for each in dataclasses.fields(self))
        return f"Origin({', '.join(f'{name}={value!r}' for name, value in fields if value is not None)})"


@dataclass(frozen=True, slots=True)
class Given:
    """A value a setting was given, and where it came from."""

    value: str
    """as JSON, a span spelled, a secret as ***"""
    from_: Origin


@dataclass(frozen=True, slots=True)
class Line:
    """A setting's line of explain."""

    path: str
    value: str
    """as JSON, a span spelled, a secret as ***"""
    from_: str
    """the layer that gave it: APP_PORT, app.yaml, layer 2, the declaration, a change"""
    over: str
    """the values it went over, newest first"""
    given: tuple[Given, ...] = dataclasses.field(default=(), repr=False)
    """Every value the setting was given, newest first: its own, then the ones it went over. from_ is
    str(given[0].from_). It is empty from a server before 0.1.1, and print(line) leaves it out."""


class Explained(list[Line]):
    """Every setting's line, and in notes what was read and not used, each with why."""

    notes: list[str]

    def __init__(self, lines: list[Line], notes: list[str]) -> None:
        super().__init__(lines)
        self.notes = notes


def _line(sent: dict[str, Any]) -> Line:
    """A line as the store sent it."""
    given = tuple(Given(each.get("value", ""), _origin(each.get("from", {}))) for each in sent.get("given", []))
    over = ", ".join(sent.get("over", []))
    return Line(sent.get("path", ""), sent.get("value", ""), sent.get("from", ""), over, given)


def _origin(sent: dict[str, Any]) -> Origin:
    """Where a value came from, as the store sent it: only the fields its kind names."""
    at = sent.get("at")
    return Origin(
        kind=sent.get("kind", "declaration"),
        place=sent.get("place"),
        path=sent.get("path"),
        variable=sent.get("variable"),
        dotenv=sent.get("dotenv"),
        file=sent.get("file"),
        version=sent.get("version"),
        at=None if at is None else moment(at),
    )


type _Reader = Callable[[Any], Any]
"""What turns a setting's value as the store sends it into what the program reads."""


@dataclass
class _Declared:
    """A declaration as the core reads it, and what reads a value of it back."""

    group: dict[str, Any]
    read: _Reader


def _declared(cls: type, path: str = "", instance: Any = None) -> _Declared:
    """A dataclass's settings: each field a setting or a group, its default the instance's or the field's."""
    if not dataclasses.is_dataclass(cls):
        raise InvalidError(f"{path or 'the declaration'}: {cls!r} is no dataclass")
    hints = typing.get_type_hints(cls, include_extras=True)
    group: dict[str, Any] = {}
    readers: dict[str, _Reader] = {}
    for each in dataclasses.fields(cls):
        if not each.init:
            continue
        at = each.name if path == "" else f"{path}.{each.name}"
        node, readers[each.name] = _node(hints[each.name], _default_of(each, instance), at)
        group[each.name] = node

    def read(value: Any) -> Any:
        given = cast("dict[str, Any]", value) if isinstance(value, dict) else {}
        return cls(**{key: reader(given.get(key)) for key, reader in readers.items()})

    return _Declared(group, read)


def _default_of(field: dataclasses.Field[Any], instance: Any) -> Any:
    if instance is not None:
        return getattr(instance, field.name)
    if field.default is not dataclasses.MISSING:
        return field.default
    if field.default_factory is not dataclasses.MISSING:
        return field.default_factory()
    return _REQUIRED


def _node(hint: Any, default: Any, path: str) -> tuple[dict[str, Any], _Reader]:
    """A field's setting, or its group when its type is a dataclass, and what reads its value."""
    mark = default if isinstance(default, _Mark) else None
    given = default if mark is None else mark.default
    base, rules = _unannotated(hint, path)
    if dataclasses.is_dataclass(base) and isinstance(base, type) and mark is None:
        declared = _declared(base, path, None if given is _REQUIRED else given)
        return {"group": declared.group}, declared.read
    is_secret = base is Secret or (mark is not None and mark.secret)
    if is_secret and base is not Secret:
        raise InvalidError(f"{path}: a secret is read as Secret: {path.rsplit('.', 1)[-1]}: Secret = secret()")
    kind, read = ("text", _secret_of) if is_secret else _kind(base, path)
    described: dict[str, Any] = {"kind": kind, **rules}
    if typing.get_origin(base) is Literal:
        described["oneOf"] = list(typing.get_args(base))
    if given is _REQUIRED:
        described["required"] = True
    else:
        described["default"] = _json_of(given, path)
    if mark is not None and mark.live:
        described["live"] = True
    if is_secret:
        described["secret"] = True
    return described, read


def _unannotated(hint: Any, path: str) -> tuple[Any, dict[str, Any]]:
    """A type without its Annotated, and the rules a Range or Items in it gives."""
    if typing.get_origin(hint) is not Annotated:
        return hint, {}
    base, *extras = typing.get_args(hint)
    rules: dict[str, Any] = {}
    for extra in extras:
        if isinstance(extra, Range):
            spans = base is timedelta
            for name, bound in (("min", extra.min), ("max", extra.max)):
                if bound is not None:
                    rules[name] = _bound(bound, spans, f"{path}: its {name}")
        elif isinstance(extra, Items):
            if typing.get_origin(base) is not list and base is not list:
                raise InvalidError(f"{path}: Items bounds a list")
            rules.update({name: bound for name, bound in (("min", extra.min), ("max", extra.max)) if bound is not None})
    return base, rules


def _bound(bound: float | timedelta | str, spans: bool, what: str) -> float:
    if spans:
        if isinstance(bound, timedelta | str):
            return ms(bound, what)
        raise InvalidError(f"{what} {bound!r}: a span's bound is a timedelta or its text")
    if isinstance(bound, bool) or not isinstance(bound, int | float):
        raise InvalidError(f"{what} {bound!r}: a number")
    return bound


def _kind(base: Any, path: str) -> tuple[Any, _Reader]:
    """A type's kind as the core reads it, and what reads a value of it."""
    origin = typing.get_origin(base)
    if base is bool:
        return "boolean", _same
    if base is int:
        return "integer", _same
    if base is float:
        return "number", _same
    if base is str:
        return "text", _same
    if base is timedelta:
        return "duration", _span_of
    if base is Any:
        return "any", _same
    if origin is Literal:
        if not all(isinstance(each, str) for each in typing.get_args(base)):
            raise InvalidError(f"{path}: a Literal of texts, one of which the setting is")
        return "text", _same
    if base is dict or origin in (dict, Mapping) or base is Mapping:
        return "object", _same
    if base is list or origin is list:
        args = typing.get_args(base)
        return _list_kind(args[0] if args else Any, f"{path}[]")
    if origin in (types.UnionType, typing.Union):
        raise InvalidError(f"{path}: {base}: a setting has one type; one a layer may leave out has a default")
    raise InvalidError(f"{path}: {base!r} is no type a config keeps")


def _list_kind(item: Any, path: str) -> tuple[Any, _Reader]:
    base, rules = _unannotated(item, path)
    if dataclasses.is_dataclass(base) and isinstance(base, type):
        declared = _declared(base, path)
        return {"list": {"group": declared.group}}, _listed(declared.read)
    kind, read = _kind(base, path)
    if isinstance(kind, dict):
        raise InvalidError(f"{path}: a list of lists; a list's items are a value or a group")
    described: dict[str, Any] = {"kind": kind, **rules}
    if typing.get_origin(base) is Literal:
        described["oneOf"] = list(typing.get_args(base))
    return {"list": described}, _listed(read)


def _listed(read: _Reader) -> _Reader:
    return lambda value: [read(each) for each in cast("list[Any]", value)] if isinstance(value, list) else []


def _same(value: Any) -> Any:
    return value


def _span_of(value: Any) -> Any:
    return timedelta(milliseconds=value) if isinstance(value, int | float) else value


def _secret_of(value: Any) -> Secret:
    return Secret(value if isinstance(value, str) else "")


def _json_of(value: Any, path: str) -> Any:
    """A value as JSON writes it: a span its milliseconds, a dataclass its fields, a secret refused."""
    if value is None or isinstance(value, str | int | float | bool):
        return value
    if isinstance(value, timedelta):
        return ms(value, path)
    if isinstance(value, Secret):
        raise InvalidError(f"{path}: a secret is given as its text, never as a Secret")
    if dataclasses.is_dataclass(value) and not isinstance(value, type):
        return {
            each.name: _json_of(getattr(value, each.name), f"{path}.{each.name}") for each in dataclasses.fields(value)
        }
    if isinstance(value, Mapping):
        return {
            str(key): _json_of(inner, f"{path}.{key}")
            for key, inner in cast("Mapping[Any, Any]", value).items()
            if inner is not None
        }
    if isinstance(value, list | tuple):
        return [_json_of(inner, f"{path}[{at}]") for at, inner in enumerate(cast("Sequence[Any]", value))]
    raise InvalidError(f"{path}: {value!r} is no value a config keeps")


def _paths(group: Mapping[str, Any], path: str) -> list[str]:
    """Every setting's path, as the core names its variable."""
    found: list[str] = []
    for key, node in group.items():
        at = key if path == "" else f"{path}.{key}"
        found.extend(_paths(node["group"], at) if "group" in node else [at])
    return found


def env_name(prefix: str, path: str) -> str:
    """A setting's variable: the prefix and its path in upper snake case (testdata/config/env.json).

    ``env_name("APP", "limits.maxRps")`` is APP_LIMITS_MAX_RPS.
    """
    name = ""
    for i, c in enumerate(path):
        if c in ".-":
            name += "_"
            continue
        before = path[i - 1] if i > 0 else None
        after = path[i + 1] if i + 1 < len(path) else None
        if before is not None and c.isupper():
            lower_after = after is not None and after.islower()
            if before.islower() or before.isdigit() or (before.isupper() and lower_after):
                name += "_"
        name += c.upper()
    return name if prefix == "" else f"{prefix}_{name}"


def _read_text(path: str) -> str | None:
    """A file's text, none when it does not exist."""
    try:
        return Path(path).read_text(encoding="utf-8")
    except FileNotFoundError:
        return None
    except (OSError, UnicodeDecodeError) as err:
        raise InvalidError(f"{path}: {err}") from err


def _layer_data(layer: Any, place: int, declared: Mapping[str, Any]) -> dict[str, Any] | None:
    """A layer as the core reads it, its files and variables read here."""
    if layer is None or layer is False:
        return None
    if isinstance(layer, FileLayer):
        text = _read_text(layer.path)
        return {"file": {"name": layer.path, "optional": layer.optional, **({} if text is None else {"text": text})}}
    if isinstance(layer, EnvLayer):
        return {"env": _env_data(layer, declared)}
    if isinstance(layer, Mapping):
        return {"object": _json_of(layer, f"layer {place}"), "place": place}
    raise InvalidError(f"layer {place}: a dict, file(…) or env(…)")


def _env_data(layer: EnvLayer, declared: Mapping[str, Any]) -> dict[str, Any]:
    """What the environment gives, read here.

    The variables of every setting and those with the prefix, never the rest
    of the process's, the files their NAME_FILE names, and the .env files' text.
    """
    windows = sys.platform == "win32"

    def key(name: str) -> str:
        return name.upper() if windows else name

    named = [layer.vars.get(path) or env_name(layer.prefix, path) for path in _paths(declared, "")]
    wanted = {key(each) for name in named for each in (name, f"{name}_FILE")}
    start = key(f"{layer.prefix}_")
    variables: dict[str, str] = {}
    for name, value in (os.environ if layer.from_ is None else layer.from_).items():
        k = key(name)
        if value is not None and (k in wanted or (layer.prefix != "" and k.startswith(start))):  # pyright: ignore[reportUnnecessaryComparison]
            variables[k] = value
    dotenv = [{"name": name, "text": text} for name in layer.files if (text := _read_text(name)) is not None]
    files: dict[str, str] = {}
    for name, path in [*variables.items(), *(found for each in dotenv for found in _file_variables(each["text"]))]:
        if name.endswith("_FILE") and (text := _read_text(path)) is not None:
            files[path] = text
    return {"prefix": layer.prefix, "vars": dict(layer.vars), "variables": variables, "files": files, "dotenv": dotenv}


_FILE_LINE = re.compile(r"^\s*(?:export\s+)?([^\W\d]\w*_FILE)\s*=\s*(.*?)\s*$")


def _file_variables(text: str) -> list[tuple[str, str]]:
    """The NAME_FILE lines of a .env file's text, whose files the core cannot read."""
    found: list[tuple[str, str]] = []
    for line in text.split("\n"):
        match = _FILE_LINE.match(line)
        if match is not None:
            found.append((match[1], re.sub(r"^(['\"])(.*)\1$", r"\2", match[2])))
    return found


def _merged(value: Any, changes: Any) -> Any:
    """A partial value over a whole one: the groups it names merged, anything else replaced."""
    if not isinstance(value, dict) or not isinstance(changes, dict):
        return changes
    out = dict(cast("dict[str, Any]", value))
    for key, change in cast("dict[str, Any]", changes).items():
        out[key] = _merged(out.get(key), change)
    return out


def _plain(value: Any) -> Any:
    """A value a pydantic model reads: a dataclass as its fields, a secret as its text."""
    if isinstance(value, Secret):
        return value.reveal()
    if dataclasses.is_dataclass(value) and not isinstance(value, type):
        return {each.name: _plain(getattr(value, each.name)) for each in dataclasses.fields(value)}
    if isinstance(value, list):
        return [_plain(each) for each in cast("list[Any]", value)]
    return value


class Config[T]:
    """A program's settings as one typed value, read from memory: ``await store.config(name, [Settings, …])``."""

    def __init__(
        self,
        link: Link,
        name: str,
        declared: _Declared,
        opener: bytes,
        check: Callable[[Any], Awaitable[None]] | None,
    ) -> None:
        self.name = name
        self._link = link
        self._declared = declared
        self._open = opener
        self._check = check
        self._handles: weakref.WeakKeyDictionary[Connection, asyncio.Future[int]] = weakref.WeakKeyDictionary()
        self._raw: Any = {}
        self._value: T = declared.read({})
        self._version = 0
        self._taken = 0
        """how many values it took, which a watch follows"""
        self._lines: list[Line] = []
        self._notes: list[str] = []
        self._refused: InvalidError | None = None
        """why validate refused the newest value the store sent, a change another program made"""
        self._taking = asyncio.Lock()
        self._changed: asyncio.Future[None] | None = None
        self._watchers: set[Callable[[T], None]] = set()
        self._stream: Any = None
        self._stopped = False
        self._following: asyncio.Task[None] | None = None

    @classmethod
    async def open(
        cls,
        link: Link,
        name: str,
        layers: Sequence[Any],
        validate: Callable[[Any], object] | type | None,
        strict: bool,
    ) -> Config[T]:
        """Opens a config from its list: the declaration first, then its layers in order."""
        if not layers or not isinstance(layers[0], type):
            raise InvalidError(f"config {name}: the list's first entry is its declaration, a dataclass")
        declared = _declared(layers[0])
        read = [_layer_data(layer, at + 2, declared.group) for at, layer in enumerate(layers[1:])]
        opener = ConfigOpen.encode(
            name=name,
            declaration=json.dumps(declared.group),
            layers=json.dumps([layer for layer in read if layer is not None]),
            strict=strict,
        )
        config = cls(link, name, declared, opener, _checker_of(name, validate))
        await link.run("read", config._handle_on)
        if config._refused is not None:
            raise config._refused
        config._following = asyncio.ensure_future(config._follow())
        return config

    @property
    def value(self) -> T:
        """The settings as they are now: a read of memory, so read it where it is used."""
        return self._value

    @property
    def version(self) -> int:
        """The version of the changes the value holds, which if_version takes."""
        return self._version

    async def update(self, changes: Mapping[str, Any], *, if_version: int | None = None) -> None:
        """Changes live settings: kept in the store, over every layer until reset, seen by every handle.

        The value they make passes validate first; all are kept or none. With
        ``if_version``, one another change came before is ConflictError.
        """
        given = _json_of(changes, "a change")
        for tries in range(1, _ATTEMPTS + 1):
            version = self._version if if_version is None else if_version
            if self._check is not None:
                await self._check(self._declared.read(_merged(self._raw, given)))

            def body(handle: int, version: int = version) -> bytes:
                return ConfigUpdate.encode(handle=handle, version=version, changes=json.dumps(given))

            try:
                await self._call("write", methods["config.update"], body)
                return
            except ConflictError:
                if if_version is not None or tries == _ATTEMPTS:
                    raise
                await self._newer_than(version)

    async def reset(self, path: str = "") -> None:
        """Drops the change kept of the setting or the group at path, or of every setting."""
        await self._call("write", methods["config.reset"], lambda handle: ConfigReset.encode(handle=handle, path=path))

    async def watch(self) -> AsyncIterator[T]:
        """The settings now, then after every change made in this process or another; one behind gets the newest.

        It ends when the store closes.
        """
        seen = -1
        while not self._stopped:
            if self._taken != seen:
                seen = self._taken
                yield self._value
                continue
            if self._changed is None:
                self._changed = asyncio.get_running_loop().create_future()
            await asyncio.shield(self._changed)

    def explain(self) -> Explained:
        """Every setting with its value, the layer that gave it and what it went over; notes, what was not used."""
        refused = [] if self._refused is None else [f"a value validate refused: {self._refused}"]
        return Explained(list(self._lines), [*self._notes, *refused])

    def on_change(self, watcher: Callable[[T], None]) -> None:
        """Calls watcher with the settings now and after every change, for what follows a config inside the SDK."""
        self._watchers.add(watcher)
        watcher(self._value)

    async def stop(self) -> None:
        """Stops following the config: the store closes."""
        self._stopped = True
        self._watchers.clear()
        if self._stream is not None:
            self._stream.cancel()
        self._wake()
        if self._following is not None:
            with contextlib.suppress(BaseException):
                await self._following

    async def _call(self, idempotence: Idempotence, method: int, body: Callable[[int], bytes]) -> None:
        async def attempt(connection: Connection) -> None:
            handle = await self._handle_on(connection)
            await self._take(ConfigValue.decode(await connection.session.call(method, body(handle))))

        await self._link.run(idempotence, attempt)

    async def _handle_on(self, connection: Connection) -> int:
        """The config's handle on a connection, opened there first, its value taken."""
        found = self._handles.get(connection)
        if found is None:
            found = asyncio.ensure_future(self._opened_on(connection))
            self._handles[connection] = found
        try:
            return await asyncio.shield(found)
        except BaseException:
            if self._handles.get(connection) is found and found.done():
                del self._handles[connection]
            raise

    async def _opened_on(self, connection: Connection) -> int:
        opened = ConfigValue.decode(await open_call(connection, methods["config.open"], self._open))
        await self._take(opened)
        return opened.get("handle", 0)

    async def _take(self, answer: dict[str, Any]) -> None:
        """Takes a value the store sent, in the order it came; an older one than this has is left."""
        async with self._taking:
            version = answer.get("version", 0)
            if version < self._version or (version == self._version and self._lines):
                return
            raw = json.loads(answer.get("value") or "{}")
            following = self._declared.read(raw)
            if self._check is not None:
                try:
                    await self._check(following)
                except InvalidError as err:
                    self._refused = err
                    if self._lines:
                        print(f"version {version}: {err}; the value before it stays", file=sys.stderr)
                    return
            self._refused = None
            self._raw, self._value, self._version = raw, following, version
            self._lines = [_line(each) for each in answer.get("lines", [])]
            self._notes = list(answer.get("notes", []))
            self._taken += 1
            self._wake()
            for watcher in list(self._watchers):
                try:
                    watcher(following)
                except Exception as err:
                    print(f"config {self.name}: a watcher raised: {err}", file=sys.stderr)

    def _wake(self) -> None:
        changed, self._changed = self._changed, None
        if changed is not None and not changed.done():
            changed.set_result(None)

    async def _newer_than(self, version: int) -> None:
        """Waits until a version later than version came, or a second passed."""
        deadline = asyncio.get_running_loop().time() + 1
        while self._version <= version and not self._stopped:
            left = deadline - asyncio.get_running_loop().time()
            if left <= 0:
                return
            if self._changed is None:
                self._changed = asyncio.get_running_loop().create_future()
            with contextlib.suppress(TimeoutError):
                await asyncio.wait_for(asyncio.shield(self._changed), left)

    async def _follow(self) -> None:
        """Follows the config on one stream until the store closes, again on the next connection when one is lost."""
        while not self._stopped:
            try:
                stream = await self._link.run("read", self._followed_on)
                if stream is None:
                    return
                while True:
                    event = await stream.next()
                    stream.consumed(len(event.body))
                    if event.end:
                        break
                    await self._take(ConfigValue.decode(event.body))
            except Exception as err:
                if self._stopped:
                    return
                if not isinstance(err, ConnectionLostError | UnavailableError | CallCancelledError):
                    await asyncio.sleep(1)

    async def _followed_on(self, connection: Connection) -> Any:
        handle = await self._handle_on(connection)
        if self._stopped:
            return None
        opened = await connection.session.open(methods["config.follow"], Handle.encode(handle=handle), True)
        # kept before its first answer, so that a stop meanwhile cancels it
        self._stream = opened
        if self._stopped:
            opened.cancel()
            return None
        await opened.next()
        return opened

    async def __aenter__(self) -> Self:
        return self

    async def __aexit__(self, *_: object) -> None:
        await self.stop()


def _checker_of(name: str, validate: Callable[[Any], object] | type | None) -> Callable[[Any], Awaitable[None]] | None:
    """The check a config's validate makes of a whole value, raising InvalidError when it refuses."""
    if validate is None:
        return None
    model = getattr(validate, "model_validate", None)

    async def check(value: Any) -> None:
        try:
            if model is not None:
                model(_plain(value))
                return
            answer = cast("Callable[[Any], object]", validate)(value)
            if inspect.isawaitable(answer):
                await answer
        except InvalidError:
            raise
        except Exception as err:
            raise InvalidError(f"config {name}: {err}") from err

    return check
