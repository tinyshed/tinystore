"""Python values as the store keeps them, by the type a handle is opened with.

A bucket keeps a value as the core keeps a Rust type, so that every language
reads the others' rows: integers and booleans as integers, and a row of bytes
that says by its first byte what wrote it: 1 text, 2 bytes, 3 JSON, 4 a
float's eight bytes. A row read as another kind than wrote it is corrupt,
never a plausible wrong value. JSON reads back into its
type: a dataclass from its fields' names, a TypedDict as the dict it is, a
model with ``model_validate`` as pydantic reads it; lists, dicts, unions,
enums and datetimes follow their annotations. Unknown keys are ignored, as
Go's encoding/json ignores them.
"""

from __future__ import annotations

import dataclasses
import enum
import json
import struct
import types
import typing
import uuid
from datetime import date, datetime
from typing import Any, Protocol

from ._wire.codec import Raw, text_of
from .errors import CorruptError, InvalidError

_INT64 = range(-(1 << 63), 1 << 63)

# the first byte of a row of bytes: the kind of value that wrote it
_TEXT, _BYTES, _JSON, _FLOAT = 1, 2, 3, 4
_KINDS = {1: "text", 2: "bytes", 3: "JSON", 4: "a float", 5: "a u64", 6: "an f32"}


def _tagged(kind: int, body: bytes) -> bytes:
    return bytes((kind,)) + body


def _body(raw: Raw, kind: int, wanted: str) -> bytes:
    """The body of a row that kind wrote; a row another kind wrote is corrupt here, never read as this one."""
    if not isinstance(raw, bytes):
        raise CorruptError(f"a stored value is {_describe(raw)} where {wanted} was written")
    if not raw:
        raise CorruptError(f"{wanted} from a row of no bytes")
    if raw[0] != kind:
        raise CorruptError(f"{wanted} from a row of {_KINDS.get(raw[0], 'bytes of no kind')}")
    return raw[1:]


def _default(value: Any) -> Any:
    if dataclasses.is_dataclass(value) and not isinstance(value, type):
        return dataclasses.asdict(value)
    dump = getattr(value, "model_dump", None)
    if callable(dump):
        return dump(mode="json")
    if isinstance(value, datetime | date):
        return value.isoformat()
    if isinstance(value, enum.Enum):
        return value.value
    if isinstance(value, uuid.UUID):
        return str(value)
    if isinstance(value, set | frozenset):
        return list(typing.cast("set[Any]", value))
    raise TypeError(f"a {type(value).__name__} JSON cannot write")


def to_json(value: Any) -> str:
    """A value's JSON text, compact, its non-ASCII kept as it is."""
    try:
        return json.dumps(value, separators=(",", ":"), ensure_ascii=False, default=_default, allow_nan=False)
    except (TypeError, ValueError) as err:
        raise InvalidError(f"a value JSON cannot write: {err}") from None


def from_json(text: str | bytes, of: Any) -> Any:
    """A value of type of read from JSON text; a value that no longer reads is corrupt."""
    try:
        return convert(json.loads(text), of)
    except (ValueError, TypeError, KeyError) as err:
        raise CorruptError(f"a stored value does not read as {_name(of)}: {err}") from None


def _name(of: Any) -> str:
    return str(getattr(of, "__name__", of))


def convert(value: Any, of: Any) -> Any:
    """Converts what JSON read into the type of."""
    if of is Any or of is object or of is None:
        return value
    validate = getattr(of, "model_validate", None)
    if callable(validate):
        return validate(value)
    origin = typing.get_origin(of)
    if origin in (typing.Union, types.UnionType):
        return _union(value, typing.get_args(of))
    if origin is typing.Literal:
        return value
    if origin in (list, tuple, set, frozenset):
        args = typing.get_args(of) or (Any,)
        return typing.cast("Any", origin(convert(v, args[0]) for v in value))
    if origin is dict:
        args = typing.get_args(of) or (Any, Any)
        return {k: convert(v, args[1]) for k, v in value.items()}
    if dataclasses.is_dataclass(of) and isinstance(of, type):
        hints = typing.get_type_hints(of)
        given = {
            f.name: convert(value[f.name], hints.get(f.name, Any)) for f in dataclasses.fields(of) if f.name in value
        }
        return of(**given)
    if isinstance(of, type):
        return _plain(value, of)
    return value


def _union(value: Any, args: tuple[Any, ...]) -> Any:
    if value is None and type(None) in args:
        return None
    for arg in args:
        if arg is type(None):
            continue
        try:
            return convert(value, arg)
        except (ValueError, TypeError, KeyError):
            continue
    raise TypeError(f"{value!r} is none of {args}")


def _plain(value: Any, of: type) -> Any:
    if typing.is_typeddict(of):
        return value
    if issubclass(of, enum.Enum):
        return of(value)
    if of is datetime:
        return datetime.fromisoformat(value)
    if of is date:
        return date.fromisoformat(value)
    if of is uuid.UUID:
        return uuid.UUID(value)
    if of is float and isinstance(value, int) and not isinstance(value, bool):
        return float(value)
    if not isinstance(value, of):
        raise TypeError(f"{value!r} is not a {of.__name__}")
    return value


class Values[T](Protocol):
    """Turns a handle's values into what a row keeps, and back."""

    def encode(self, value: T) -> Raw: ...
    def decode(self, raw: Raw) -> T: ...


def _describe(raw: Raw) -> str:
    if raw is None:
        return "nothing"
    return "an integer" if isinstance(raw, int) else f"{len(raw)} bytes"


class _Json:
    def __init__(self, of: Any) -> None:
        self._of = of

    def encode(self, value: Any) -> Raw:
        return _tagged(_JSON, to_json(value).encode())

    def decode(self, raw: Raw) -> Any:
        if raw is None:
            return None
        return from_json(_body(raw, _JSON, "JSON"), self._of)


class _Text:
    def encode(self, value: str) -> Raw:
        if not isinstance(value, str):  # pyright: ignore[reportUnnecessaryIsInstance]
            raise InvalidError(f"a {type(value).__name__} in a bucket of str")
        return _tagged(_TEXT, value.encode())

    def decode(self, raw: Raw) -> str:
        if raw is None:
            return ""
        text = text_of(_body(raw, _TEXT, "text"))
        if text is None:
            raise CorruptError("a stored value is text that is not UTF-8")
        return text


class _Bytes:
    def encode(self, value: bytes) -> Raw:
        if not isinstance(value, bytes | bytearray | memoryview):  # pyright: ignore[reportUnnecessaryIsInstance]
            raise InvalidError(f"a {type(value).__name__} in a bucket of bytes")
        return _tagged(_BYTES, bytes(value))

    def decode(self, raw: Raw) -> bytes:
        if raw is None:
            return b""
        return _body(raw, _BYTES, "bytes")


class _Int:
    def encode(self, value: int) -> Raw:
        if isinstance(value, bool) or not isinstance(value, int):  # pyright: ignore[reportUnnecessaryIsInstance]
            raise InvalidError(f"a {type(value).__name__} in a bucket of int")
        if value not in _INT64:
            raise InvalidError(f"{value} is past what an int64 holds")
        return value

    def decode(self, raw: Raw) -> int:
        if not isinstance(raw, int):
            raise CorruptError(f"a stored value is {_describe(raw)} where an integer was written")
        return raw


class _Float:
    def encode(self, value: float) -> Raw:
        if isinstance(value, bool) or not isinstance(value, int | float):  # pyright: ignore[reportUnnecessaryIsInstance]
            raise InvalidError(f"a {type(value).__name__} in a bucket of float")
        return _tagged(_FLOAT, struct.pack(">d", value))

    def decode(self, raw: Raw) -> float:
        bits = _body(raw, _FLOAT, "a float")
        if len(bits) != 8:
            raise CorruptError(f"a float from {len(bits)} bytes rather than 8")
        return struct.unpack(">d", bits)[0]


class _Bool:
    def encode(self, value: bool) -> Raw:
        if not isinstance(value, bool):  # pyright: ignore[reportUnnecessaryIsInstance]
            raise InvalidError(f"a {type(value).__name__} in a bucket of bool")
        return 1 if value else 0

    def decode(self, raw: Raw) -> bool:
        if not isinstance(raw, int):
            raise CorruptError(f"a stored value is {_describe(raw)} where a bool was written")
        return raw != 0


def values_of(of: Any) -> Values[Any]:
    """How a handle of type of keeps its values: None keeps any JSON value."""
    if of is str:
        return _Text()
    if of is bytes:
        return _Bytes()
    if of is bool:
        return _Bool()
    if of is int:
        return _Int()
    if of is float:
        return _Float()
    return _Json(of)


def json_values(of: Any) -> Values[Any]:
    """JSON whatever the type: a job's value, a kept answer, a document."""
    return _Json(of)
