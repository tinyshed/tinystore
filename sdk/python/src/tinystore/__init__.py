"""TinyStore for Python: kv, jobs, blobs, SQL, records and metrics in one directory, served by a sidecar.

::

    async with tinystore.open("./data") as store:
        sessions = store.kv.bucket("sessions", Session, sliding=timedelta(days=30))
        await sessions.of(user_id).set(token, session)
"""

from ._console import ConsoleHandler, handler
from ._page import Page
from .blobs import BlobBucket, BlobObject, Download, Usage
from .config import Config, Source
from .errors import (
    CallCancelledError,
    ClosedError,
    ConflictError,
    CorruptError,
    InternalError,
    InUseError,
    InvalidError,
    LimitError,
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
from .jobs import ClaimedJob, Enqueue, Job, JobEntry, Queue, Repeat, cron, daily, every
from .kv import Batch, Bucket, Counters, Entry
from .limiter import Allowance, Limiter
from .metrics import Aggregate, Condition, Plan, Series, none_of, one_of, prefix
from .records import Cursor, Record, trace
from .sql import Database, Done
from .store import Status, Store, connect, open

__all__ = [
    "Aggregate",
    "Allowance",
    "Batch",
    "BlobBucket",
    "BlobObject",
    "Bucket",
    "CallCancelledError",
    "ClaimedJob",
    "ClosedError",
    "Condition",
    "Config",
    "ConflictError",
    "ConsoleHandler",
    "CorruptError",
    "Counters",
    "Cursor",
    "Database",
    "Done",
    "Download",
    "Enqueue",
    "Entry",
    "InUseError",
    "InternalError",
    "InvalidError",
    "Job",
    "JobEntry",
    "LimitError",
    "Limiter",
    "OutcomeUnknownError",
    "Page",
    "PermissionDeniedError",
    "Plan",
    "ProtocolError",
    "Queue",
    "Record",
    "Repeat",
    "Series",
    "Source",
    "Status",
    "Store",
    "SuspendedError",
    "TinystoreError",
    "TooNewError",
    "TooOldError",
    "UnauthenticatedError",
    "UnavailableError",
    "UnimplementedError",
    "Usage",
    "connect",
    "cron",
    "daily",
    "every",
    "handler",
    "none_of",
    "one_of",
    "open",
    "prefix",
    "trace",
]
