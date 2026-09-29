"""The MessagePack profile of docs/wire.md, written for this SDK.

A general library accepts a float 32, an extension, a key twice or a value
nested nine deep, which the profile refuses. An encoder writes the shortest
form of every integer, length and count and every float in eight bytes, so
that a vector compares byte for byte; a decoder takes any valid form.
"""

from __future__ import annotations

import struct
from typing import TYPE_CHECKING, Literal

from ..errors import InvalidError, ProtocolError

if TYPE_CHECKING:
    import builtins
    from collections.abc import Callable

NIL = 0xC0
FALSE = 0xC2
TRUE = 0xC3
BIN8, BIN16, BIN32 = 0xC4, 0xC5, 0xC6
FLOAT64 = 0xCB
UINT8, UINT16, UINT32, UINT64 = 0xCC, 0xCD, 0xCE, 0xCF
INT8, INT16, INT32, INT64 = 0xD0, 0xD1, 0xD2, 0xD3
STR8, STR16, STR32 = 0xD9, 0xDA, 0xDB
ARRAY16, ARRAY32 = 0xDC, 0xDD
MAP16, MAP32 = 0xDE, 0xDF
FIXMAP, FIXARRAY, FIXSTR, NEGFIX = 0x80, 0x90, 0xA0, 0xE0

MAX_DEPTH = 8
"""How deeply maps and arrays may nest in a message."""

_U16 = struct.Struct(">H")
_U32 = struct.Struct(">I")
_U64 = struct.Struct(">Q")
_I8 = struct.Struct(">b")
_I16 = struct.Struct(">h")
_I32 = struct.Struct(">i")
_I64 = struct.Struct(">q")
_F64 = struct.Struct(">d")


class Writer:
    """Writes one message into a buffer that grows as it must."""

    __slots__ = ("_buf",)

    def __init__(self) -> None:
        self._buf = bytearray()

    def bytes(self) -> builtins.bytes:
        return bytes(self._buf)

    def nil(self) -> None:
        self._buf.append(NIL)

    def bool(self, v: builtins.bool) -> None:
        self._buf.append(TRUE if v else FALSE)

    def uint(self, v: builtins.int) -> None:
        """Writes a value from 0 in the unsigned family's shortest form.

        ::

            7 → 07    300 → cd 01 2c    70000 → ce 00 01 11 70
        """
        buf = self._buf
        if v < 0 or v > 0xFFFF_FFFF_FFFF_FFFF:
            raise InvalidError(f"{v} is not an unsigned 64-bit integer")
        if v <= 0x7F:
            buf.append(v)
        elif v <= 0xFF:
            buf += bytes((UINT8, v))
        elif v <= 0xFFFF:
            buf.append(UINT16)
            buf += _U16.pack(v)
        elif v <= 0xFFFF_FFFF:
            buf.append(UINT32)
            buf += _U32.pack(v)
        else:
            buf.append(UINT64)
            buf += _U64.pack(v)

    def int(self, v: builtins.int) -> None:
        """Writes a value from 0 as uint does, a negative one in the signed family's shortest form.

        ::

            -1 → ff    -33 → d0 df    -300 → d1 fe d4
        """
        if v >= 0:
            self.uint(v)
            return
        buf = self._buf
        if v < -0x8000_0000_0000_0000:
            raise InvalidError(f"{v} is not a 64-bit integer")
        if v >= -32:
            buf.append(v & 0xFF)
        elif v >= -0x80:
            buf.append(INT8)
            buf += _I8.pack(v)
        elif v >= -0x8000:
            buf.append(INT16)
            buf += _I16.pack(v)
        elif v >= -0x8000_0000:
            buf.append(INT32)
            buf += _I32.pack(v)
        else:
            buf.append(INT64)
            buf += _I64.pack(v)

    def float(self, v: builtins.float) -> None:
        """Writes a float 64 with its bits: -0 → cb 80 00 00 00 00 00 00 00."""
        self._buf.append(FLOAT64)
        self._buf += _F64.pack(v)

    def str(self, s: builtins.str) -> None:
        """Writes text as UTF-8; a lone surrogate has no UTF-8 and is refused, not replaced."""
        try:
            encoded = s.encode("utf-8")
        except UnicodeEncodeError:
            raise InvalidError("a string with a lone surrogate, which UTF-8 cannot spell") from None
        n = len(encoded)
        buf = self._buf
        if n <= 31:
            buf.append(FIXSTR | n)
        elif n <= 0xFF:
            buf += bytes((STR8, n))
        elif n <= 0xFFFF:
            buf.append(STR16)
            buf += _U16.pack(n)
        else:
            buf.append(STR32)
            buf += _U32.pack(n)
        buf += encoded

    def bin(self, b: builtins.bytes | bytearray | memoryview) -> None:
        n = len(b)
        buf = self._buf
        if n <= 0xFF:
            buf += bytes((BIN8, n))
        elif n <= 0xFFFF:
            buf.append(BIN16)
            buf += _U16.pack(n)
        else:
            buf.append(BIN32)
            buf += _U32.pack(n)
        buf += b

    def array(self, n: builtins.int) -> None:
        self._count(n, FIXARRAY, ARRAY16, ARRAY32)

    def map(self, n: builtins.int) -> None:
        self._count(n, FIXMAP, MAP16, MAP32)

    def _count(self, n: builtins.int, fix: builtins.int, two: builtins.int, four: builtins.int) -> None:
        buf = self._buf
        if n <= 15:
            buf.append(fix | n)
        elif n <= 0xFFFF:
            buf.append(two)
            buf += _U16.pack(n)
        else:
            buf.append(four)
            buf += _U32.pack(n)


Type = Literal["nil", "bool", "int", "float", "str", "bin", "array", "map", "invalid"]


def type_of(b: int) -> Type:
    """The type a value's first byte says."""
    if b <= 0x7F or b >= NEGFIX or UINT8 <= b <= INT64:
        return "int"
    if b & 0xF0 == FIXMAP or b in (MAP16, MAP32):
        return "map"
    if b & 0xF0 == FIXARRAY or b in (ARRAY16, ARRAY32):
        return "array"
    if b & 0xE0 == FIXSTR or STR8 <= b <= STR32:
        return "str"
    if BIN8 <= b <= BIN32:
        return "bin"
    if b == NIL:
        return "nil"
    if b in (FALSE, TRUE):
        return "bool"
    # a float 32, an extension, or the byte never used
    return "float" if b == FLOAT64 else "invalid"


_names: dict[Type, str] = {
    "nil": "nil",
    "bool": "a bool",
    "int": "an integer",
    "float": "a float",
    "str": "a str",
    "bin": "a bin",
    "array": "an array",
    "map": "a map",
    "invalid": "an invalid type",
}


def _fail(reason: builtins.str) -> ProtocolError:
    return ProtocolError(f"invalid message: {reason}")


class Reader:
    """Reads one message and checks the profile as it goes.

    A value the profile refuses, one past the bytes left, or bytes after the
    message raise a ProtocolError.
    """

    __slots__ = ("_at", "_body", "_depth")

    def __init__(self, body: bytes | bytearray | memoryview) -> None:
        self._body = bytes(body) if not isinstance(body, bytes) else body
        self._at = 0
        self._depth = 0

    def end(self) -> None:
        """Refuses bytes left after the one value a message is."""
        left = len(self._body) - self._at
        if left != 0:
            raise _fail(f"{left} bytes after the message")

    def type(self) -> Type:
        """The next value's type, without reading it."""
        if self._at >= len(self._body):
            return "invalid"
        return type_of(self._body[self._at])

    def _next(self) -> builtins.int:
        if self._at >= len(self._body):
            raise _fail("the body ends inside a value")
        b = self._body[self._at]
        self._at += 1
        return b

    def _take(self, n: builtins.int) -> builtins.int:
        left = len(self._body) - self._at
        if n > left:
            raise _fail(f"{n} bytes asked for where {left} are left")
        at = self._at
        self._at += n
        return at

    def _integer(self, want: builtins.str) -> builtins.int:
        b = self._next()
        if b <= 0x7F:
            return b
        if b >= NEGFIX:
            return b - 0x100
        body = self._body
        match b:
            case 0xCC:
                return body[self._take(1)]
            case 0xCD:
                return _U16.unpack_from(body, self._take(2))[0]
            case 0xCE:
                return _U32.unpack_from(body, self._take(4))[0]
            case 0xCF:
                return _U64.unpack_from(body, self._take(8))[0]
            case 0xD0:
                return _I8.unpack_from(body, self._take(1))[0]
            case 0xD1:
                return _I16.unpack_from(body, self._take(2))[0]
            case 0xD2:
                return _I32.unpack_from(body, self._take(4))[0]
            case 0xD3:
                return _I64.unpack_from(body, self._take(8))[0]
            case _:
                raise _fail(f"{_names[type_of(b)]} where {want} belongs")

    def uint(self) -> builtins.int:
        """Reads an unsigned integer in any encoding of its value."""
        v = self._integer("an unsigned integer")
        if v < 0:
            raise _fail(f"{v} where an unsigned integer belongs")
        return v

    def int(self) -> builtins.int:
        """Reads a signed 64-bit integer in any encoding of its value."""
        v = self._integer("an integer")
        if v > 0x7FFF_FFFF_FFFF_FFFF:
            raise _fail(f"{v} where an int64 belongs")
        return v

    def float(self) -> builtins.float:
        """Reads a float 64, or an integer a float 64 holds exactly.

        A JavaScript encoder writes 3.0 as 3.
        """
        t = self.type()
        if t == "float":
            self._at += 1
            return _F64.unpack_from(self._body, self._take(8))[0]
        if t != "int":
            raise _fail(f"{_names[t]} where a float belongs")
        v = self._integer("a float")
        f = float(v)
        if f >= 2.0**64 or int(f) != v:
            raise _fail(f"{v}, which a float 64 does not hold exactly")
        return f

    def bool(self) -> builtins.bool:
        b = self._next()
        if b == TRUE:
            return True
        if b != FALSE:
            raise _fail(f"{_names[type_of(b)]} where a bool belongs")
        return False

    def nil(self) -> builtins.bool:
        """Reads a nil if one is next and says so, leaving any other value."""
        if self.type() == "nil":
            self._at += 1
            return True
        return False

    def str(self) -> builtins.str:
        """Reads a str, which must be valid UTF-8."""
        b = self._next()
        body = self._body
        if b & 0xE0 == FIXSTR:
            n = b & 0x1F
        elif b == STR8:
            n = body[self._take(1)]
        elif b == STR16:
            n = _U16.unpack_from(body, self._take(2))[0]
        elif b == STR32:
            n = _U32.unpack_from(body, self._take(4))[0]
        else:
            raise _fail(f"{_names[type_of(b)]} where a str belongs")
        at = self._take(n)
        try:
            return body[at : at + n].decode("utf-8")
        except UnicodeDecodeError:
            raise _fail("a str that is not UTF-8") from None

    def bin(self) -> builtins.bytes:
        b = self._next()
        body = self._body
        if b == BIN8:
            n = body[self._take(1)]
        elif b == BIN16:
            n = _U16.unpack_from(body, self._take(2))[0]
        elif b == BIN32:
            n = _U32.unpack_from(body, self._take(4))[0]
        else:
            raise _fail(f"{_names[type_of(b)]} where a bin belongs")
        at = self._take(n)
        return body[at : at + n]

    def array(self) -> builtins.int:
        """Reads an array's count and enters it; leave once its elements are read.

        A count past the bytes left is refused before anything is made of it,
        since every element takes at least a byte.
        """
        return self._enter("array", FIXARRAY, ARRAY16, ARRAY32, 1)

    def map(self) -> builtins.int:
        """Reads a map's count and enters it, each entry taking two bytes at least."""
        return self._enter("map", FIXMAP, MAP16, MAP32, 2)

    def leave(self) -> None:
        self._depth -= 1

    def _enter(
        self,
        kind: Type,
        fix: builtins.int,
        two: builtins.int,
        four: builtins.int,
        each: builtins.int,
    ) -> builtins.int:
        b = self._next()
        body = self._body
        if b & 0xF0 == fix:
            n = b & 0x0F
        elif b == two:
            n = _U16.unpack_from(body, self._take(2))[0]
        elif b == four:
            n = _U32.unpack_from(body, self._take(4))[0]
        else:
            raise _fail(f"{_names[type_of(b)]} where {_names[kind]} belongs")
        left = len(body) - self._at
        if n * each > left:
            raise _fail(f"{_names[kind]} of {n} elements in {left} bytes")
        if self._depth == MAX_DEPTH:
            raise _fail(f"{_names[kind]} nested deeper than {MAX_DEPTH}")
        self._depth += 1
        return n

    def fields(self, each: Callable[[builtins.int], builtins.bool]) -> None:
        """Reads a map with unsigned keys, a message or a field of one.

        each is called with a key and reads its value, or returns False to
        have it skipped, well formed and within bounds. A key twice is refused.
        """
        n = self.map()
        seen: set[int] = set()
        for _ in range(n):
            key = self.uint()
            if key in seen:
                raise _fail(f"key {key} twice")
            seen.add(key)
            at = self._at
            if not each(key) or self._at == at:
                self._at = at
                self.skip()
        self.leave()

    def names(self, each: Callable[[builtins.str], None]) -> None:
        """Reads a map of names, an error's what or a series' labels, as fields does."""
        n = self.map()
        seen: set[str] = set()
        for _ in range(n):
            name = self.str()
            if name in seen:
                raise _fail(f"name {name!r} twice")
            seen.add(name)
            at = self._at
            each(name)
            if self._at == at:
                self.skip()
        self.leave()

    def items(self, each: Callable[[builtins.int], None]) -> None:
        """Reads an array, calling each with an element's index and the element next."""
        n = self.array()
        for i in range(n):
            at = self._at
            each(i)
            if self._at == at:
                self.skip()
        self.leave()

    def skip(self) -> None:
        """Reads past one value, checking it is well formed as if it were read."""
        match self.type():
            case "nil" | "bool":
                self._at += 1
            case "int":
                self._integer("an integer")
            case "float":
                self.float()
            case "str":
                self.str()
            case "bin":
                self.bin()
            case "array":
                self.items(lambda _: self.skip())
            case "map":
                self._skip_map()
            case "invalid":
                raise _fail(self._describe())

    def _skip_map(self) -> None:
        # a map's keys are unsigned integers or names, as a message's maps are
        at = self._at
        n = self.map()
        named = n > 0 and self.type() == "str"
        self._at = at
        self._depth -= 1
        if named:
            self.names(lambda _: self.skip())
        else:
            self.fields(lambda _: False)

    def _describe(self) -> builtins.str:
        if self._at >= len(self._body):
            return "the body ends where a value belongs"
        b = self._body[self._at]
        if b == 0xCA:
            return "a float 32, which the profile does not allow"
        if b == 0xC1:
            return "the type byte 0xc1, which MessagePack never uses"
        return f"the extension type {b:#x}, which the profile does not allow"
