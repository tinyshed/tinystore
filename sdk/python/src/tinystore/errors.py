"""What failed, as the server's error or the SDK's own says it.

Each code is a class of its own, so a caller catches ``ConflictError`` as a Go
program asks ``errors.Is(err, tinystore.ErrConflict)``.
"""

from __future__ import annotations

from typing import ClassVar

What = dict[str, str | bytes]
"""The item a failure is about, by the names the server gives: a bucket and a
key, a queue and a key, a series' labels, a table and a constraint. A value
whose bytes are not UTF-8, as a key of bytes may be, comes as its bytes."""


class TinystoreError(Exception):
    code: ClassVar[str] = "internal"

    def __init__(self, message: str, what: What | None = None) -> None:
        super().__init__(message)
        self.message = message
        self.what: What = what if what is not None else {}


class InvalidError(TinystoreError):
    """The request cannot be done as asked."""

    code = "invalid"


class LimitError(TinystoreError):
    """A bound: memory, size or count. Later, or smaller, it may go through.

    A limit the store names says which, what the call would have taken of it,
    and the bound: ``decoded samples``, 120000, 100000.
    """

    code = "limit"

    def __init__(self, message: str, what: What | None = None) -> None:
        super().__init__(message, what)
        limit, wanted, bound = self.what.get("limit"), self.what.get("wanted"), self.what.get("bound")
        self.limit: str | None = limit if isinstance(limit, str) else None
        self.wanted: int | None = int(wanted) if isinstance(wanted, str) else None
        self.bound: int | None = int(bound) if isinstance(bound, str) else None


class ClosedError(TinystoreError):
    """The store, the handle or the connection closed."""

    code = "closed"


class InUseError(TinystoreError):
    """A name is taken."""

    code = "in_use"


class ConflictError(TinystoreError):
    """A condition or a version no longer holds: read again, then decide."""

    code = "conflict"


class CorruptError(TinystoreError):
    """Stored bytes no longer read."""

    code = "corrupt"


class TooOldError(TinystoreError):
    """A time before its engine's window."""

    code = "too_old"


class TooNewError(TinystoreError):
    """A time past its engine's window, ahead of the store's clock."""

    code = "too_new"


class SuspendedError(TinystoreError):
    """A metrics series in quarantine until its repair."""

    code = "suspended"


class OutcomeUnknownError(TinystoreError):
    """A write whose commit may or may not have happened.

    The connection was lost while it was in flight, or the server's commit
    failed. Read what it wrote before writing again.
    """

    code = "outcome_unknown"


class PermissionDeniedError(TinystoreError):
    """The connection's capability does not allow it: a data token changing a schema."""

    code = "permission"


class UnimplementedError(TinystoreError):
    """A method the server does not have."""

    code = "unimplemented"


class CallCancelledError(TinystoreError):
    """A call the client cancelled; named apart from asyncio's, which a cancelled task raises."""

    code = "cancelled"


class UnavailableError(TinystoreError):
    """The server is closing; the call was not run and may be sent on another connection."""

    code = "unavailable"


class InternalError(TinystoreError):
    """A fault of the server's own."""

    code = "internal"


class ProtocolError(TinystoreError):
    """A frame or a message that breaks the protocol; the connection ends with it."""

    code = "protocol"


class UnauthenticatedError(TinystoreError):
    """A remote connection whose token the server does not know."""

    code = "unauthenticated"


_classes: dict[str, type[TinystoreError]] = {
    cls.code: cls
    for cls in (
        InvalidError,
        LimitError,
        ClosedError,
        InUseError,
        ConflictError,
        CorruptError,
        TooOldError,
        TooNewError,
        SuspendedError,
        OutcomeUnknownError,
        PermissionDeniedError,
        UnimplementedError,
        CallCancelledError,
        UnavailableError,
        InternalError,
        ProtocolError,
        UnauthenticatedError,
    )
}

codes: tuple[str, ...] = tuple(_classes)
"""The codes this SDK knows, as ``messages.json`` lists the server's."""


def error_of(code: str, message: str, what: What | None = None) -> TinystoreError:
    """The error a code names; one this SDK does not know yet is internal, keeping the code."""
    known = _classes.get(code)
    if known is None:
        return InternalError(f"{code}: {message}", what)
    return known(message, what)
