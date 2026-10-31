"""The bounds one call meets, each by the name a ``LimitError`` gives it.

The same table as Rust's ``tinystore::limits`` and the other SDKs'. A program
cuts its batches to a bound before it calls, and tells which bound a
``LimitError`` met without reading its text::

    from tinystore import bounds

    most = bounds.SET_MANY_DOCUMENTS.bound
    for at in range(0, len(courses), most):
        await index.set_many(courses[at : at + most])

    except LimitError as error:
        if error.limit == bounds.REQUEST_BYTES.name: ...

A bound the store's options set, its memory (``store memory``) or its
``max_series`` (``series in a store``), is named the same way and is not here.
``tinystore.limits`` is another thing: rate limits and quotas.
"""

from __future__ import annotations

from dataclasses import dataclass


@dataclass(frozen=True, slots=True)
class Limit:
    """A bound by its name, as ``LimitError.limit`` says it, and the most it takes."""

    name: str
    bound: int


VALUE_BYTES = Limit("value bytes", 1 << 20)
"""A bucket's value, as it is stored."""
SETTING_BYTES = Limit("setting bytes", 16 << 10)
"""A live setting's change, as JSON."""
CONFIG_BYTES = Limit("config bytes", 256 << 10)
"""Every change a config keeps, together."""
LOCK_WAITERS = Limit("lock waiters", 10_000)
"""The callers that wait for one lock or semaphore at once."""
SUBSCRIPTIONS = Limit("subscriptions", 10_000)
"""The subscriptions to the store's channels at once."""
JOB_BYTES = Limit("job bytes", 1 << 20)
"""A job's payload, its result or a step's answer, as JSON."""
PROGRESS_BYTES = Limit("progress bytes", 4 << 10)
"""A run's progress, as JSON."""
ROWS_BYTES = Limit("bytes of a call's rows", 64 << 20)
"""The values one call reads from a database: past them, read a page at a time."""
EVENT_BYTES = Limit("event bytes", 256 << 10)
"""One event, its name, fields and text together."""
APPEND_BYTES = Limit("append bytes", 4 << 20)
"""The events of one append together."""
AGGREGATE_ROWS = Limit("aggregate rows", 10_000)
"""The rows one ``aggregate`` answers."""
COUNT_BYTES = Limit("count bytes", 64 << 20)
"""The different values one ``top`` or ``distinct`` holds, as JSON."""
INGEST_SAMPLES = Limit("ingest samples", 10_000)
"""The samples of one ``ingest``."""
INGEST_BYTES = Limit("ingest bytes", 4 << 20)
"""The bytes of one ``ingest``'s samples, each with its labels."""
TEXT_SAMPLES = Limit("text samples", 100_000)
"""The samples of one exporter's text."""
TEXT_BYTES = Limit("text bytes", 32 << 20)
"""One exporter's text, and the bytes of its samples."""
QUERY_SERIES = Limit("series", 1000)
"""The series one query finds."""
QUERY_BYTES = Limit("bytes read", 16 << 20)
"""What one query reads from the file."""
QUERY_SAMPLES = Limit("samples decoded", 1 << 20)
"""The samples one query decodes."""
QUERY_ANSWER = Limit("answer", 100_000)
"""The samples or buckets one query answers."""
SET_MANY_DOCUMENTS = Limit("set_many documents", 1000)
"""The documents of one ``set_many``."""
SET_MANY_BYTES = Limit("set_many bytes", 16 << 20)
"""The documents of one ``set_many`` as JSON; one request body, ``REQUEST_BYTES``, bounds it first."""
DOCUMENT_BYTES = Limit("document bytes", 1 << 20)
"""One document as JSON, or a table's row as its index reads it."""
SEARCH_HITS = Limit("search hits", 1000)
"""The hits a search reaches, its offset and its limit together."""
SEARCH_TEXT = Limit("search text", 1 << 10)
"""A search's text, in bytes."""
FILTER_CONDITIONS = Limit("filter conditions", 8)
"""The conditions of one search's filter."""
FILTER_VALUES = Limit("filter values", 100)
"""The values the lists of one search's conditions hold together."""
FILTER_TEXT = Limit("filter text", 256)
"""A text a filter keeps or asks for, in bytes."""
VECTOR_DIMENSIONS = Limit("vector dimensions", 4096)
"""A vector's dimensions."""
REQUEST_BYTES = Limit("request bytes", 2 << 20)
"""One request, its message as the SDK sends it: a larger one fails before it is sent."""
ANSWER_BYTES = Limit("answer bytes", 2 << 20)
"""One answer, its message as the store sends it."""
