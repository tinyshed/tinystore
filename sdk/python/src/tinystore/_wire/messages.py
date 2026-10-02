"""Every message of docs/wire.md, by the names its tables give each field.

messages.json holds a vector of each, which tests/test_wire.py reads through
these declarations.
"""

from __future__ import annotations

from .codec import (
    Message,
    bin_,
    bool_,
    floats,
    int_,
    ints,
    key,
    kv_value,
    list_,
    message,
    names,
    nullable,
    sql_value,
    str_,
    text,
    uint,
)

METHODS: dict[str, int] = {
    "kv.open": 0x0101,
    "kv.get": 0x0102,
    "kv.has": 0x0103,
    "kv.set": 0x0104,
    "kv.delete": 0x0105,
    "kv.take": 0x0106,
    "kv.touch": 0x0107,
    "kv.add": 0x0108,
    "kv.max": 0x0109,
    "kv.clear": 0x010A,
    "kv.batch": 0x010B,
    "kv.view": 0x010C,
    "kv.scan": 0x010D,
    "kv.allow": 0x010E,
    "kv.configure": 0x010F,
    "kv.watch": 0x0110,
    "kv.run": 0x0111,
    "jobs.open": 0x0201,
    "jobs.enqueue": 0x0202,
    "jobs.update": 0x0203,
    "jobs.cancel": 0x0204,
    "jobs.get": 0x0205,
    "jobs.claim": 0x0206,
    "jobs.settle": 0x0207,
    "jobs.scan": 0x0208,
    "jobs.work": 0x0209,
    "jobs.watch": 0x020A,
    "blobs.open": 0x0301,
    "blobs.stat": 0x0302,
    "blobs.delete": 0x0303,
    "blobs.copy": 0x0304,
    "blobs.move": 0x0305,
    "blobs.usage": 0x0306,
    "blobs.clear": 0x0307,
    "blobs.scan": 0x0308,
    "blobs.put": 0x0309,
    "blobs.get": 0x030A,
    "sql.open": 0x0401,
    "sql.exec": 0x0402,
    "sql.query": 0x0403,
    "sql.batch": 0x0404,
    "records.append": 0x0501,
    "records.read": 0x0502,
    "records.follow": 0x0503,
    "records.lines": 0x0504,
    "records.damaged": 0x0505,
    "records.drop": 0x0506,
    "metrics.ingest": 0x0601,
    "metrics.read": 0x0602,
    "metrics.aggregate": 0x0603,
    "metrics.drop": 0x0604,
    "metrics.explain": 0x0605,
}

Hello = message(
    "hello",
    protocol=(1, uint),
    client=(2, str_),
    token=(3, str_),
    max_body=(4, uint),
    stream_credit=(5, uint),
    challenge=(6, bin_),
)

Welcome = message(
    "welcome",
    protocol=(1, uint),
    server=(2, str_),
    instance=(3, bin_),
    capability=(4, str_),
    max_body=(5, uint),
    in_flight=(6, uint),
    connection_credit=(7, uint),
    stream_credit=(8, uint),
    engines=(9, list_(str_)),
    now=(10, int_),
    proof=(11, bin_),
)

GoAway = message("goaway", code=(1, str_), message=(2, str_))

Failure = message("error", code=(1, str_), message=(2, str_), what=(3, names(text)))

Handle = message("handle", handle=(1, uint))

Empty = message("empty")

KvBucket = message(
    "kv.bucket",
    name=(1, str_),
    counters=(2, bool_),
    default_ttl=(3, uint),
    sliding=(4, uint),
    lose_at_most=(5, uint),
    config=(6, bool_),
    rate=(7, uint),
    per=(8, uint),
    burst=(9, uint),
    once=(10, bool_),
)

_kv_call = {
    "handle": (1, uint),
    "owners": (2, list_(key)),
    "key": (3, key),
    "value": (4, kv_value),
    "ttl": (5, uint),
    "expire_at": (6, int_),
    "if_version": (7, bin_),
    "if_absent": (8, bool_),
    "n": (9, int_),
    "after": (10, key),
    "limit": (11, uint),
}

KvCall = Message("kv.call", _kv_call)
KvOperation = Message("kv.operation", {"method": (0, uint), **_kv_call})
KvCalls = message("kv.calls", calls=(1, list_(KvOperation)))

KvEntry = message(
    "kv.entry",
    found=(1, bool_),
    value=(2, kv_value),
    version=(3, bin_),
    expires=(4, int_),
    key=(5, key),
)

KvResults = message("kv.results", entries=(1, list_(KvEntry)))
KvPage = message("kv.page", more=(1, bool_), after=(2, key))
KvAllowance = message("kv.allowance", ok=(1, bool_), left=(2, uint), retry_after=(3, uint))
KvConfigure = message("kv.configure", handle=(1, uint), set=(2, list_(str_)), reset=(3, list_(str_)))
KvKept = message("kv.kept", changes=(1, uint), fields=(2, list_(str_)))

JobsRepeat = message("jobs.repeat", cron=(1, str_), zone=(2, str_), every=(3, uint))

JobsQueue = message(
    "jobs.queue",
    name=(1, str_),
    lease=(2, uint),
    max_attempts=(3, uint),
    backoff_first=(4, uint),
    backoff_most=(5, uint),
    max_waiting=(6, uint),
    keep_failed=(7, uint),
    keep_done=(8, uint),
    schedule=(9, JobsRepeat),
    max_running=(10, uint),
)

_jobs_job = {
    "value": (1, str_),
    "key": (2, str_),
    "at": (3, int_),
    "after": (4, uint),
    "repeat": (5, JobsRepeat),
}

JobsJob = Message("jobs.job", _jobs_job)
JobsBatch = message("jobs.batch", handle=(1, uint), jobs=(2, list_(JobsJob)))
JobsChange = Message("jobs.change", {**_jobs_job, "handle": (6, uint)})
JobsKey = message("jobs.key", handle=(1, uint), key=(2, str_))

JobsEntry = message(
    "jobs.entry",
    found=(1, bool_),
    key=(2, str_),
    value=(3, str_),
    at=(4, int_),
    attempt=(5, uint),
    state=(6, uint),
    err=(7, str_),
    repeat=(8, str_),
    ahead=(9, uint),
    progress=(10, str_),
)

JobsLease = message("jobs.lease", handle=(1, uint), lease=(2, uint))

JobsHeld = message(
    "jobs.held",
    found=(1, bool_),
    job=(2, uint),
    key=(3, str_),
    value=(4, str_),
    at=(5, int_),
    attempt=(6, uint),
    cancelled=(7, bool_),
)

JobsOutcome = message(
    "jobs.outcome",
    job=(1, uint),
    how=(2, uint),
    err=(3, str_),
    at=(4, int_),
    after=(5, uint),
    progress=(6, str_),
)

JobsOutcomes = message("jobs.outcomes", outcomes=(1, list_(JobsOutcome)))
JobsSettled = message("jobs.settled", settled=(1, list_(nullable(Failure))))

JobsQuery = message(
    "jobs.query",
    handle=(1, uint),
    prefix=(2, str_),
    state=(3, uint),
    after=(4, str_),
    limit=(5, uint),
)

JobsPage = message("jobs.page", more=(1, bool_), after=(2, str_))

JobsWorkers = message(
    "jobs.workers",
    handle=(1, uint),
    workers=(2, uint),
    timeout=(3, uint),
    until_idle=(4, bool_),
    cancels=(5, bool_),
)

BlobsBucket = message("blobs.bucket", name=(1, str_), default_ttl=(2, uint), max_size=(3, uint))

BlobsCall = message(
    "blobs.call",
    handle=(1, uint),
    owners=(2, list_(key)),
    key=(3, str_),
    to=(4, str_),
    content_type=(5, str_),
    meta=(6, names(text)),
    ttl=(7, uint),
    expire_at=(8, int_),
    size=(9, uint),
    if_match=(10, str_),
    if_none_match=(11, bool_),
    prefix=(12, str_),
    after=(13, str_),
    limit=(14, uint),
    offset=(15, uint),
    length=(16, uint),
)

BlobsObject = message(
    "blobs.object",
    found=(1, bool_),
    key=(2, str_),
    size=(3, uint),
    etag=(4, str_),
    content_type=(5, str_),
    modified=(6, int_),
    expires=(7, int_),
    meta=(8, names(text)),
)

BlobsTotal = message("blobs.total", objects=(1, int_), bytes=(2, int_))
BlobsPage = message("blobs.page", more=(1, bool_), after=(2, str_))

SqlMigration = message("sql.migration", name=(1, str_), text=(2, str_))
SqlDatabase = message("sql.database", name=(1, str_), migrations=(2, list_(SqlMigration)))

SqlStatement = message(
    "sql.statement",
    handle=(1, uint),
    sql=(2, str_),
    args=(3, list_(sql_value)),
    named=(4, names(sql_value)),
    write=(5, bool_),
    rows=(6, bool_),
)

SqlStatements = message(
    "sql.statements",
    handle=(1, uint),
    statements=(2, list_(SqlStatement)),
    read=(3, bool_),
)

SqlDone = message("sql.done", changes=(1, int_), last_id=(2, int_))
SqlColumns = message("sql.columns", columns=(1, list_(str_)))
SqlRow = message("sql.row", values=(1, list_(sql_value)))

SqlResult = message(
    "sql.result",
    changes=(1, int_),
    last_id=(2, int_),
    columns=(3, list_(str_)),
    rows=(4, list_(list_(sql_value))),
)

SqlResults = message("sql.results", results=(1, list_(SqlResult)))

RecordsRecord = message(
    "records.record",
    at=(1, int_),
    stream=(2, text),
    name=(3, text),
    level=(4, int_),
    body=(5, text),
    trace_id=(6, bin_),
    span_id=(7, bin_),
    context=(8, list_(text)),
    attrs=(9, list_(text)),
)

RecordsBatch = message("records.batch", records=(1, list_(RecordsRecord)))

RecordsQuery = message(
    "records.query",
    from_=(1, int_),
    to=(2, int_),
    streams=(3, list_(text)),
    names=(4, list_(text)),
    min_level=(5, int_),
    trace_id=(6, bin_),
    attrs=(7, list_(text)),
    context=(8, list_(text)),
    newest=(9, bool_),
    limit=(10, uint),
    budget_blocks=(11, uint),
    budget_bytes=(12, uint),
    budget_records=(13, uint),
    search=(14, str_),
)

RecordsPage = message("records.page", more=(1, bool_), from_=(2, int_), to=(3, int_))

RecordsCursor = message(
    "records.cursor",
    segment=(1, int_),
    row=(2, int_),
    limit=(3, uint),
    expired=(4, uint),
)

RecordsStream = message("records.stream", stream=(1, text))

RecordsDamage = message(
    "records.damage",
    stream=(1, text),
    segment=(2, int_),
    head_row=(3, int_),
    from_=(4, int_),
    to=(5, int_),
    reason=(6, str_),
)

RecordsDamages = message("records.damages", damages=(1, list_(RecordsDamage)))

MetricsSeries = message(
    "metrics.series",
    labels=(1, names(str_)),
    kind=(2, str_),
    times=(3, ints),
    values=(4, floats),
)

MetricsBatch = message("metrics.batch", series=(1, list_(MetricsSeries)))

MetricsPlan = message(
    "metrics.plan",
    series=(1, uint),
    blocks=(2, uint),
    summarized=(3, uint),
    bytes=(4, uint),
    decoded=(5, uint),
    limit_series=(6, uint),
    limit_blocks=(7, uint),
    limit_bytes=(8, uint),
    limit_decoded=(9, uint),
    limit_answered=(10, uint),
    stops=(11, nullable(Failure)),
)

MetricsCondition = message("metrics.condition", label=(1, str_), kind=(2, str_), values=(3, list_(str_)))

MetricsRange = message(
    "metrics.range",
    matchers=(1, names(str_)),
    from_=(2, int_),
    to=(3, int_),
    limit_series=(4, uint),
    limit_blocks=(5, uint),
    limit_bytes=(6, uint),
    limit_decoded=(7, uint),
    limit_answered=(8, uint),
    width=(9, uint),
    op=(10, str_),
    where=(11, list_(MetricsCondition)),
    by=(12, list_(str_)),
    without=(13, list_(str_)),
)

MetricsBuckets = message(
    "metrics.buckets",
    labels=(1, names(str_)),
    kind=(2, str_),
    from_=(3, ints),
    to=(4, ints),
    count=(5, ints),
    resets=(6, ints),
    values=(7, floats),
    flags=(8, bin_),
)

MetricsLabels = message("metrics.labels", labels=(1, names(str_)))
MetricsDropped = message("metrics.dropped", found=(1, bool_), unreadable_groups=(2, uint))

MESSAGES: dict[str, Message] = {
    m.name: m
    for m in (
        Hello,
        Welcome,
        GoAway,
        Failure,
        Handle,
        Empty,
        KvBucket,
        KvCall,
        KvOperation,
        KvCalls,
        KvEntry,
        KvResults,
        KvPage,
        KvAllowance,
        KvConfigure,
        KvKept,
        JobsQueue,
        JobsRepeat,
        JobsJob,
        JobsBatch,
        JobsChange,
        JobsKey,
        JobsEntry,
        JobsLease,
        JobsHeld,
        JobsOutcome,
        JobsOutcomes,
        JobsSettled,
        JobsQuery,
        JobsPage,
        JobsWorkers,
        BlobsBucket,
        BlobsCall,
        BlobsObject,
        BlobsTotal,
        BlobsPage,
        SqlDatabase,
        SqlMigration,
        SqlStatement,
        SqlStatements,
        SqlDone,
        SqlColumns,
        SqlRow,
        SqlResult,
        SqlResults,
        RecordsRecord,
        RecordsBatch,
        RecordsQuery,
        RecordsPage,
        RecordsCursor,
        RecordsStream,
        RecordsDamage,
        RecordsDamages,
        MetricsSeries,
        MetricsBatch,
        MetricsRange,
        MetricsCondition,
        MetricsPlan,
        MetricsBuckets,
        MetricsLabels,
        MetricsDropped,
    )
}
"""Every message by the name messages.json gives it."""
