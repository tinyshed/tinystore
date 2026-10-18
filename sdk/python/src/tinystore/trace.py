"""The trace a call runs in, carried in a contextvars variable as Go carries one in a context.Context.

::

    with tinystore.trace(trace_id=trace_id, span_id=span_id):
        await handle(request)

A line logged and an event appended inside, however deep and after any
await, take its trace and span; an event given its own keeps them.
"""

from __future__ import annotations

import re
from contextlib import contextmanager
from contextvars import ContextVar
from typing import TYPE_CHECKING

from .errors import InvalidError

if TYPE_CHECKING:
    from collections.abc import Generator

_TRACE: ContextVar[tuple[str, str | None] | None] = ContextVar("tinystore trace", default=None)
_HEX = re.compile(r"[0-9a-fA-F]+")


def _checked(hex_id: str, digits: int, what: str) -> str:
    if not isinstance(hex_id, str) or len(hex_id) != digits or not _HEX.fullmatch(hex_id):  # pyright: ignore[reportUnnecessaryIsInstance]
        raise InvalidError(f"{what} {hex_id!r}: {digits} hex digits, as a traceparent header writes it")
    return hex_id.lower()


@contextmanager
def trace(*, trace_id: str, span_id: str | None = None) -> Generator[None]:
    """Runs the block in a trace: 32 hex digits, and a span's 16."""
    given = (_checked(trace_id, 32, "trace_id"), None if span_id is None else _checked(span_id, 16, "span_id"))
    token = _TRACE.set(given)
    try:
        yield
    finally:
        _TRACE.reset(token)


def current_trace() -> tuple[str, str | None] | None:
    """The trace and span the caller runs in, as hex; None outside a trace."""
    return _TRACE.get()
