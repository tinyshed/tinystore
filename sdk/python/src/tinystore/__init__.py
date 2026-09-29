"""TinyStore for Python: kv, jobs, blobs, SQL, records and metrics in one directory, served by a sidecar.

::

    async with tinystore.open("./data") as store:
        sessions = store.kv.bucket("sessions", Session, sliding=timedelta(days=30))
        await sessions.of(user_id).set(token, session)
"""

from .blobs import BlobBucket, BlobObject, Download, Usage
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
from .metrics import Aggregate, Series
from .records import Cursor, Record
from .sql import Database, Done
from .store import Store, connect, open

__all__ = [
    "Aggregate",
    "Batch",
    "BlobBucket",
    "BlobObject",
    "Bucket",
    "CallCancelledError",
    "ClaimedJob",
    "ClosedError",
    "ConflictError",
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
    "OutcomeUnknownError",
    "PermissionDeniedError",
    "ProtocolError",
    "Queue",
    "Record",
    "Repeat",
    "Series",
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
    "open",
]
