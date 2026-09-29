"""Python values as JSON and back, by the type a bucket, a queue or a row is declared with.

A dataclass reads back from its fields' names, a TypedDict as the dict it is,
a model with model_validate as pydantic reads it; lists, dicts, unions,
enums and datetimes follow their annotations. Unknown keys are ignored, as
Go's encoding/json ignores them.
"""

from __future__ import annotations

import dataclasses
import enum
import json
import types
import typing
import uuid
from datetime import date, datetime
from typing import Any

from .errors import CorruptError, InvalidError


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
        return json.dumps(value, separators=(",", ":"), ensure_ascii=False, default=_default)
    except (TypeError, ValueError) as err:
        raise InvalidError(f"a value JSON cannot write: {err}") from None


def from_json(text: str | bytes, of: Any) -> Any:
    """A value of type of read from JSON text; a value that no longer reads is corrupt."""
    try:
        return convert(json.loads(text), of)
    except (ValueError, TypeError, KeyError) as err:
        raise CorruptError(f"a stored value does not read as {getattr(of, '__name__', of)}: {err}") from None


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
