"""SQLite's values as the program's types, and back.

A row's names are its columns' names, and a row reads into its type itself:
a field typed ``bool`` reads ``0`` and ``1``, a ``datetime`` unix
milliseconds, a ``uuid.UUID`` its text, and a list, a dict, a dataclass or a
model JSON text. A value goes as SQLite keeps it, and one it would change is
refused before it leaves: a NaN, an integer past int64, a naive datetime.
"""

from __future__ import annotations

import dataclasses
import enum
import json
import math
import types
import typing
import uuid
from datetime import UTC, datetime, timedelta
from typing import TYPE_CHECKING, Any

from ._values import convert, to_json
from .errors import InvalidError

if TYPE_CHECKING:
    from collections.abc import Callable, Mapping

SqlValue = None | int | float | str | bytes
"""A value as SQLite gives it: NULL, an integer, a float, text or bytes."""

SqlArg = None | bool | int | float | str | bytes
"""A value as it leaves: SQLite's five, and a bool, kept as 1 or 0."""

Row = dict[str, Any]
"""A row as SQLite gave it, by its columns' names."""

_INT64 = range(-(1 << 63), 1 << 63)
_EPOCH = datetime(1970, 1, 1, tzinfo=UTC)


def cell_of(value: Any, where: str) -> SqlArg:
    """A value as it leaves, refusing one that would arrive changed, before anything is sent."""
    if value is None or isinstance(value, bool | str):
        return value
    if isinstance(value, int):
        if value not in _INT64:
            raise InvalidError(f"{where}, {value}, is past what an int64 holds")
        return value
    if isinstance(value, float):
        if math.isnan(value):
            raise InvalidError(f"{where} is NaN, which SQLite would keep as NULL")
        return value
    if isinstance(value, bytes):
        return value
    if isinstance(value, bytearray | memoryview):
        return bytes(typing.cast("bytes", value))
    if isinstance(value, datetime):
        if value.tzinfo is None:
            raise InvalidError(f"{where} is a datetime without a zone: give it one, as datetime.now(UTC) does")
        return (value - _EPOCH) // timedelta(milliseconds=1)
    if isinstance(value, uuid.UUID):
        return str(value)
    if isinstance(value, enum.Enum):
        return cell_of(value.value, where)
    if isinstance(value, list | tuple | set | frozenset):
        raise InvalidError(f"{where} is a list: a list for in (...) is sql.list(...), a JSON value sql.json(...)")
    raise InvalidError(f"{where} is a {type(value).__name__}: a JSON value is sql.json(...)")


def field_of(value: Any, name: str) -> SqlArg:
    """A row's field as it is written: a list, a dict, a dataclass or a model as its JSON."""
    if isinstance(value, list | tuple | set | frozenset | dict) or _is_record(value):
        return to_json(value)
    return cell_of(value, name)


def _is_record(value: Any) -> bool:
    return (dataclasses.is_dataclass(value) and not isinstance(value, type)) or callable(
        getattr(value, "model_dump", None)
    )


def fields_of(row: Any) -> dict[str, Any]:
    """A row the program gives, as its fields by name: a dataclass's, a model's, or a mapping's own."""
    if isinstance(row, dict):
        return dict(typing.cast("dict[str, Any]", row))
    if dataclasses.is_dataclass(row) and not isinstance(row, type):
        return {field.name: getattr(row, field.name) for field in dataclasses.fields(row)}
    dump = getattr(row, "model_dump", None)
    if callable(dump):
        return typing.cast("dict[str, Any]", dump())
    raise InvalidError(f"a row is a dataclass, a TypedDict, a pydantic model or a dict, not a {type(row).__name__}")


def _hints(of: Any) -> dict[str, Any]:
    try:
        return typing.get_type_hints(of)
    except (NameError, TypeError):
        return dict(getattr(of, "__annotations__", {}))


def reader(of: Any) -> Callable[[Row], Any]:
    """How a row reads into the type of: its fields by their columns' names, each as its annotation says."""
    if of is None or of is dict or of is Any or typing.get_origin(of) is dict:
        return lambda row: row
    hints = _hints(of)
    make: Callable[[dict[str, Any]], Any]
    if dataclasses.is_dataclass(of) and isinstance(of, type):
        names = {field.name for field in dataclasses.fields(of)}
        build = typing.cast("Callable[..., Any]", of)
        make = lambda fields: build(**fields)  # noqa: E731 - one of three makers, each one line
    elif callable(getattr(of, "model_validate", None)):
        names = set(hints)
        make = typing.cast("Callable[[dict[str, Any]], Any]", getattr(of, "model_validate"))  # noqa: B009
    elif typing.is_typeddict(of):

        def typed(row: Row) -> Any:
            return {
                column: _read(value, hints[column], column) if column in hints else value
                for column, value in row.items()
            }

        return typed
    else:
        raise InvalidError(f"a row reads into a dataclass, a TypedDict, a pydantic model or a dict, not {of!r}")

    def read(row: Row) -> Any:
        fields = {
            column: _read(value, hints.get(column, Any), column) for column, value in row.items() if column in names
        }
        try:
            return make(fields)
        except TypeError as err:
            raise InvalidError(f"a row does not make a {getattr(of, '__name__', of)}: {err}") from None

    return read


def value_reader(of: Any) -> Callable[[Any], Any]:
    """How one value, a scalar's, reads as the type of; None leaves it as SQLite gave it."""
    if of is None or of is Any:
        return lambda value: value
    return lambda value: _read(value, of, "the value")


def _read(value: Any, of: Any, column: str) -> Any:
    try:
        return _converted(value, of)
    except (ValueError, TypeError, KeyError) as err:
        raise InvalidError(f"column {column}: {value!r} does not read as {_named(of)}: {err}") from None


def _named(of: Any) -> str:
    return str(getattr(of, "__name__", of))


def _converted(value: Any, of: Any) -> Any:
    if of is Any or of is object:
        return value
    origin = typing.get_origin(of)
    if origin in (typing.Union, types.UnionType):
        args = typing.get_args(of)
        if value is None and type(None) in args:
            return None
        others = [arg for arg in args if arg is not type(None)]
        if len(others) == 1:
            return _converted(value, others[0])
        return value
    if value is None:
        return None
    if of is bool:
        if value in (0, 1) and not isinstance(value, float):
            return bool(value)
        raise ValueError("a bool is 0 or 1")
    if of is datetime:
        if isinstance(value, int):
            return _EPOCH + timedelta(milliseconds=value)
        if isinstance(value, str):
            return datetime.fromisoformat(value)
        raise ValueError("a time is unix milliseconds")
    if of is uuid.UUID:
        return uuid.UUID(bytes=value) if isinstance(value, bytes) else uuid.UUID(str(value))
    if of is float and isinstance(value, int):
        return float(value)
    if isinstance(of, type) and issubclass(of, enum.Enum):
        return of(value)
    if of in (int, float, str, bytes):
        if not isinstance(value, of):
            raise ValueError(f"a {type(value).__name__}")
        return value
    if isinstance(value, str | bytes):
        return convert(json.loads(value), of)
    return value


def row_of(columns: list[str], values: list[Any]) -> Row:
    """A row of a statement's answer, by its columns' names."""
    return dict(zip(columns, values, strict=False))


def check_columns(columns: list[str]) -> None:
    """Refuses two columns of one name, which one row cannot hold."""
    seen: set[str] = set()
    for column in columns:
        if column in seen:
            raise InvalidError(f"two columns named {column}: name them apart with as")
        seen.add(column)


def named_values(values: Mapping[str, Any], names: list[str]) -> list[SqlArg]:
    """The values of a text's :names, in the order the names first appear."""
    missing = [name for name in names if name not in values]
    if missing:
        raise InvalidError(f"no value for :{missing[0]}")
    return [cell_of(values[name], f":{name}") for name in names]
