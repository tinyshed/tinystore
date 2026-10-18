"""TinyStore for Python: kv, jobs, SQL, files, events, metrics and more in one directory.

::

    async with await tinystore.open("data") as store:
        sessions = store.bucket("sessions", Session, idle="30d")
        await sessions.under(user.id).set(digest(token), session)
"""

from .blobs import FileBytes, FileInfo, FilePage, Files, StoredFile, Upload, Usage
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
from .query import Query, RowPage, Table
from .sql import Database, Done, Sql, SqlTx, TxQueue, sql
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
    "Database",
    "Done",
    "Entry",
    "FileBytes",
    "FileInfo",
    "FilePage",
    "Files",
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
    "Query",
    "Queue",
    "Quota",
    "RateLimit",
    "RowPage",
    "Run",
    "Schedule",
    "Sql",
    "SqlTx",
    "Status",
    "Store",
    "StoreTxQueue",
    "StoredFile",
    "SuspendedError",
    "Table",
    "TinystoreError",
    "TooNewError",
    "TooOldError",
    "Tx",
    "TxBucket",
    "TxCounters",
    "TxQueue",
    "UnauthenticatedError",
    "UnavailableError",
    "UnimplementedError",
    "Upload",
    "Usage",
    "WindowUse",
    "Worker",
    "connect",
    "open",
    "sql",
]
