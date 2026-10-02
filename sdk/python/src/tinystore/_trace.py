"""The trace a call runs in, as Go's records.WithTrace carries one in a context."""

from __future__ import annotations

from contextvars import ContextVar

carried: ContextVar[tuple[bytes, bytes | None] | None] = ContextVar("tinystore_trace", default=None)
"""The trace and span of tinystore.trace, which a handler's lines and records appended without a trace take."""
