"""How each field of a message is written and read.

A message is a map from small integer keys to values. Its fields are declared
by the names docs/wire.md gives them, in snake_case, so that the tests read
every vector of messages.json through them without a table.
"""

from __future__ import annotations

import sys
from array import array
from datetime import datetime
from typing import TYPE_CHECKING, Any

from ..errors import InvalidError
from .msgpack import Reader, Writer

if TYPE_CHECKING:
    from collections.abc import Callable, Mapping, Sequence

SqlValue = None | int | float | str | bytes
"""A value SQLite returns: NULL, an integer, a float, text or bytes."""

SqlArg = SqlValue | bool | datetime
"""A value a statement takes: SQLite's five, a bool kept as 1 or 0, and a
datetime, which sqldb keeps as its unix milliseconds."""


class Codec:
    """How a field's value is written and read; kind says what it is, for the vector tests."""

    __slots__ = ("item", "kind", "read", "write")

    def __init__(
        self,
        kind: str,
        write: Callable[[Writer, Any], None],
        read: Callable[[Reader], Any],
        item: Codec | Message | None = None,
    ) -> None:
        self.kind = kind
        self.write = write
        self.read = read
        self.item = item


uint = Codec("uint", lambda w, v: w.uint(v), lambda r: r.uint())
int_ = Codec("int", lambda w, v: w.int(v), lambda r: r.int())
float_ = Codec("float", lambda w, v: w.float(v), lambda r: r.float())
bool_ = Codec("bool", lambda w, v: w.bool(v), lambda r: r.bool())
str_ = Codec("str", lambda w, v: w.str(v), lambda r: r.str())
bin_ = Codec("bin", lambda w, v: w.bin(v), lambda r: r.bin())


def text_of(b: bytes) -> str | None:
    """The text bytes spell, or None when they are not UTF-8."""
    try:
        return b.decode("utf-8")
    except UnicodeDecodeError:
        return None


def _write_text(w: Writer, v: str | bytes) -> None:
    if isinstance(v, str):
        w.str(v)
        return
    spelled = text_of(v)
    if spelled is None:
        w.bin(v)
    else:
        w.str(spelled)


text = Codec(
    "text",
    _write_text,
    lambda r: r.bin() if r.type() == "bin" else r.str(),
)
"""Text: a str, or a bin whose bytes are text that is not UTF-8.

Another program's output may be such bytes.
"""


Key = str | bytes | int


def _write_key(w: Writer, v: Key) -> None:
    if isinstance(v, bool):
        raise InvalidError("a bool as a key; a key is text or an integer")
    if isinstance(v, int):
        w.str(str(v))
    else:
        _write_text(w, v)


def _read_key(r: Reader) -> str | bytes:
    t = r.type()
    if t == "bin":
        return r.bin()
    if t == "int":
        return str(r.int())
    return r.str()


key = Codec("key", _write_key, _read_key)
"""A kv key's text: a str, a bin, or an integer, which is its decimal spelling."""

Raw = None | int | bytes
"""A kv value as its row keeps it: nothing, an integer or bytes."""


def _write_raw(w: Writer, v: Raw) -> None:
    if v is None:
        w.nil()
    elif isinstance(v, bytes):
        w.bin(v)
    else:
        w.int(v)


def _read_raw(r: Reader) -> Raw:
    if r.nil():
        return None
    return r.bin() if r.type() == "bin" else r.int()


kv_value = Codec("kv value", _write_raw, _read_raw)


def _write_sql(w: Writer, v: SqlArg) -> None:
    if v is None:
        w.nil()
    elif isinstance(v, bool):
        w.bool(v)
    elif isinstance(v, int):
        w.int(v)
    elif isinstance(v, float):
        if v != v:
            raise InvalidError("NaN as an SQL argument, which SQLite would keep as NULL")
        w.float(v)
    elif isinstance(v, str):
        w.str(v)
    elif isinstance(v, datetime):
        if v.tzinfo is None:
            raise InvalidError("a naive datetime as an SQL argument: give it its zone")
        w.int(round(v.timestamp() * 1000))
    else:
        w.bin(v)


def _read_sql(r: Reader) -> SqlValue | bool:
    match r.type():
        case "nil":
            r.nil()
            return None
        case "bool":
            return r.bool()
        case "int":
            return r.int()
        case "float":
            return r.float()
        case "bin":
            return r.bin()
        case _:
            return r.str()


sql_value = Codec("sql value", _write_sql, _read_sql)


def names(value: Codec) -> Codec:
    """Names mapped to values, written in the byte order of their UTF-8.

    That order is their code points'.
    """

    def write(w: Writer, v: Mapping[str, Any]) -> None:
        w.map(len(v))
        for name in sorted(v):
            w.str(name)
            value.write(w, v[name])

    def read(r: Reader) -> dict[str, Any]:
        named: dict[str, Any] = {}

        def each(name: str) -> None:
            named[name] = value.read(r)

        r.names(each)
        return named

    return Codec("sql names" if value.kind == "sql value" else "names", write, read)


# a column's values are little-endian, as the machines this SDK runs on hold
# them; one that holds them otherwise swaps them
_little = sys.byteorder == "little"


def _column(r: Reader, code: str) -> array[Any]:
    raw = r.bin()
    if len(raw) % 8 != 0:
        raise InvalidError(f"a column of {len(raw)} bytes, which holds no whole number of values")
    values = array(code, raw)
    if not _little:
        values.byteswap()
    return values


def _write_column(w: Writer, code: str, v: Sequence[Any]) -> None:
    values = v if isinstance(v, array) else array(code, v)
    if not _little:
        values = array(code, values)
        values.byteswap()
    w.bin(values.tobytes())


ints = Codec("ints", lambda w, v: _write_column(w, "q", v), lambda r: _column(r, "q"))
"""A column of int64s as a bin, eight bytes little-endian each."""

floats = Codec("floats", lambda w, v: _write_column(w, "d", v), lambda r: _column(r, "d"))
"""A column of float64s as a bin.

The bytes are kept rather than the numbers, so a NaN's payload survives.
"""


def list_(item: Codec | Message) -> Codec:
    def write(w: Writer, v: Sequence[Any]) -> None:
        w.array(len(v))
        for each in v:
            item.write(w, each)

    def read(r: Reader) -> list[Any]:
        items: list[Any] = []
        r.items(lambda _: items.append(item.read(r)))
        return items

    return Codec(f"[]{item.kind}", write, read, item)


def nullable(value: Codec | Message) -> Codec:
    """A value that may be nil in its place, as a settlement's error."""
    return Codec(
        f"{value.kind}?",
        lambda w, v: w.nil() if v is None else value.write(w, v),
        lambda r: None if r.nil() else value.read(r),
        value,
    )


class Message:
    """A map of the fields its declaration names, keys ascending.

    A field is written when its value is not None, so an absent field and a
    zero one stay what the caller said; a key the declaration lacks is skipped
    as it reads, well formed and within bounds.
    """

    kind = "message"
    item = None

    def __init__(self, name: str, fields: Mapping[str, tuple[int, Codec | Message]]) -> None:
        self.name = name
        self.fields = dict(fields)
        self._order = sorted(((key, field, codec) for field, (key, codec) in fields.items()))
        self._by_key = {key: (field, codec) for key, field, codec in self._order}

    def write(self, w: Writer, value: Mapping[str, Any]) -> None:
        present = [(key, codec, value[field]) for key, field, codec in self._order if value.get(field) is not None]
        w.map(len(present))
        for key, codec, v in present:
            w.uint(key)
            codec.write(w, v)

    def read(self, r: Reader) -> dict[str, Any]:
        read: dict[str, Any] = {}

        def each(key: int) -> bool:
            found = self._by_key.get(key)
            if found is None:
                return False
            read[found[0]] = found[1].read(r)
            return True

        r.fields(each)
        return read

    def encode(self, value: Mapping[str, Any] | None = None, /, **fields: Any) -> bytes:
        """The message's bytes, from a mapping, keyword fields, or both."""
        w = Writer()
        self.write(w, {**(value or {}), **fields})
        return w.bytes()

    def decode(self, body: bytes) -> dict[str, Any]:
        """Reads a body that is this message and nothing after it."""
        r = Reader(body)
        value = self.read(r)
        r.end()
        return value


def message(name: str, /, **fields: tuple[int, Codec | Message]) -> Message:
    return Message(name, fields)
