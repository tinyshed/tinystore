"""TinyStore for Python: kv, jobs, SQL, files, events, metrics and more in one directory.

::

    async with await tinystore.open("data") as store:
        sessions = store.bucket("sessions", Session, idle="30d")
        await sessions.under(user.id).set(digest(token), session)
"""

from .clock import Clock
from .errors import (
    CallCancelledError,
    ClosedError,
    CommittedLocallyError,
    ConflictError,
    Constraint,
    CorruptError,
    InternalError,
    InUseError,
    InvalidError,
    LimitError,
    LostError,
    NotFoundError,
    OutcomeUnknownError,
    PermissionDeniedError,
    ProtocolError,
    SuspendedError,
    TinystoreError,
    TooNewError,
    TooOldError,
    UnauthenticatedError,
    UnavailableError,
    UnimplementedError,
)
from .jobs import Backoff, Concurrency, Job, JobPage, LastRun, Outcome, Queue, Run, Schedule, StoreTxQueue, Worker
from .kv import Bucket, Counters, Entry, Page
from .limits import Allowance, Quota, RateLimit, WindowUse
from .once import Once
from .store import Status, Store, connect, open
from .tx import Tx, TxBucket, TxCounters

__all__ = [
    "Allowance",
    "Backoff",
    "Bucket",
    "CallCancelledError",
    "Clock",
    "ClosedError",
    "CommittedLocallyError",
    "Concurrency",
    "ConflictError",
    "Constraint",
    "CorruptError",
    "Counters",
    "Entry",
    "InUseError",
    "InternalError",
    "InvalidError",
    "Job",
    "JobPage",
    "LastRun",
    "LimitError",
    "LostError",
    "NotFoundError",
    "Once",
    "Outcome",
    "OutcomeUnknownError",
    "Page",
    "PermissionDeniedError",
    "ProtocolError",
    "Queue",
    "Quota",
    "RateLimit",
    "Run",
    "Schedule",
    "Status",
    "Store",
    "StoreTxQueue",
    "SuspendedError",
    "TinystoreError",
    "TooNewError",
    "TooOldError",
    "Tx",
    "TxBucket",
    "TxCounters",
    "UnauthenticatedError",
    "UnavailableError",
    "UnimplementedError",
    "WindowUse",
    "Worker",
    "connect",
    "open",
]
