"""A frame: a twelve-byte header, little-endian, and a body.

::

    0             4      5       6          8             12
    │ body length │ kind │ flags │ method   │ stream      │ body …
      u32           u8     u8      u16        u32
"""

from __future__ import annotations

import struct
from dataclasses import dataclass
from typing import Literal

from ..errors import ProtocolError

HEADER_SIZE = 12
_HEADER = struct.Struct("<IBBHI")

HELLO, WELCOME, REQUEST, RESPONSE, DATA, CANCEL, CREDIT, PING, PONG, GOAWAY = range(1, 11)

END = 1
"""The sender's last frame on its stream."""
ERROR = 2
"""Beside END alone: the body is an error."""


@dataclass(frozen=True, slots=True)
class Header:
    length: int
    kind: int
    flags: int
    method: int
    stream: int


@dataclass(frozen=True, slots=True)
class _Rule:
    name: str
    stream: Literal["none", "own", "either"]
    flags: int
    length: int | None = None


_rules = {
    HELLO: _Rule("HELLO", "none", 0),
    WELCOME: _Rule("WELCOME", "none", 0),
    REQUEST: _Rule("REQUEST", "own", END),
    RESPONSE: _Rule("RESPONSE", "own", END | ERROR),
    DATA: _Rule("DATA", "own", END | ERROR),
    CANCEL: _Rule("CANCEL", "own", 0, 0),
    CREDIT: _Rule("CREDIT", "either", 0, 4),
    PING: _Rule("PING", "none", 0, 8),
    PONG: _Rule("PONG", "none", 0, 8),
    GOAWAY: _Rule("GOAWAY", "none", 0),
}


def kind_name(kind: int) -> str:
    rule = _rules.get(kind)
    return rule.name if rule is not None else f"kind {kind}"


def parse_header(b: bytes | bytearray | memoryview, at: int = 0) -> Header:
    """Reads a header from its twelve bytes without checking it."""
    return Header(*_HEADER.unpack_from(b, at))


def check_header(h: Header, max_body: int) -> None:
    """Refuses a header that breaks the protocol.

    An unknown kind, a flag its kind does not define, ERROR without END, a
    method outside a REQUEST or none in one, a stream where the kind names
    none or none where it names one, and a body of a length the kind cannot
    have or past the agreed maximum.
    """
    rule = _rules.get(h.kind)
    if rule is None:
        raise ProtocolError(f"a frame of kind {h.kind}")
    if h.flags & ~rule.flags:
        raise ProtocolError(f"flags {h.flags:#x} on a {rule.name}")
    if h.flags & ERROR and not h.flags & END:
        raise ProtocolError(f"ERROR without END on stream {h.stream}")
    if h.kind == REQUEST and h.method == 0:
        raise ProtocolError(f"a REQUEST without a method on stream {h.stream}")
    if h.kind != REQUEST and h.method != 0:
        raise ProtocolError(f"method {h.method:#x} on a {rule.name}")
    if (rule.stream == "none" and h.stream != 0) or (rule.stream == "own" and h.stream == 0):
        raise ProtocolError(f"stream {h.stream} on a {rule.name}")
    if rule.length is not None and h.length != rule.length:
        raise ProtocolError(f"a {rule.name} of {h.length} bytes, not {rule.length}")
    if h.length > max_body:
        raise ProtocolError(f"a {rule.name} of {h.length} bytes, {max_body} agreed")


def frame(kind: int, flags: int, method: int, stream: int, body: bytes) -> bytes:
    return _HEADER.pack(len(body), kind, flags, method, stream) + body


def credit(stream: int, n: int) -> bytes:
    """A CREDIT granting n bytes on a stream."""
    return frame(CREDIT, 0, 0, stream, struct.pack("<I", n))


def granted(body: bytes) -> int:
    return struct.unpack_from("<I", body)[0]
