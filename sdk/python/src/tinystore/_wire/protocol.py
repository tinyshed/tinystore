"""Every message and method of the protocol.

Written by crates/protocol from protocol/*.wire; `just protocol` writes it again.
"""

# ruff: noqa: PERF401, RUF100
from __future__ import annotations

from typing import TYPE_CHECKING, Any

from .codec import Message, bin_, bool_, float_, int_, key, kv_value, list_, names, sql_value, str_, uint

if TYPE_CHECKING:
    from collections.abc import Mapping

    from .msgpack import Reader, Writer

methods: dict[str, int] = {
    "backup.write": 0x0801,
    "blobs.open": 0x0401,
    "blobs.put": 0x0402,
    "blobs.upload": 0x0403,
    "blobs.get": 0x0404,
    "blobs.head": 0x0405,
    "blobs.delete": 0x0406,
    "blobs.copy": 0x0407,
    "blobs.rename": 0x0408,
    "blobs.expire": 0x0409,
    "blobs.list": 0x040A,
    "blobs.usage": 0x040B,
    "blobs.clear": 0x040C,
    "blobs.read": 0x040D,
    # Answers once the store has the message, not once anyone handled it; in a
    # database's transaction, it is handed over once the transaction commits.
    "channels.publish": 0x0A01,
    # Answers once the subscription is registered, then sends batches within the
    # client's credit until it cancels: a client that grants none falls behind.
    "channels.subscribe": 0x0A02,
    "config.open": 0x0701,
    # The config's value now, then each time it changes, until the client
    # cancels: one stream a config a process, whose value every handle and
    # watcher of it there takes.
    "config.follow": 0x0702,
    "config.update": 0x0703,
    "config.reset": 0x0704,
    "jobs.queue.open": 0x0201,
    "jobs.schedule.open": 0x0202,
    "jobs.add": 0x0203,
    "jobs.set": 0x0204,
    "jobs.update": 0x0205,
    "jobs.cancel": 0x0206,
    "jobs.get": 0x0207,
    "jobs.list": 0x0208,
    "jobs.work": 0x0209,
    "jobs.step": 0x020A,
    "jobs.keep": 0x020B,
    "jobs.watch": 0x020C,
    "jobs.tx": 0x020D,
    "kv.bucket.open": 0x0101,
    "kv.get": 0x0102,
    "kv.has": 0x0103,
    "kv.set": 0x0104,
    "kv.create": 0x0105,
    "kv.take": 0x0106,
    "kv.delete": 0x0107,
    "kv.expire": 0x0108,
    "kv.clear": 0x0109,
    "kv.list": 0x010A,
    "kv.reseal": 0x010B,
    "kv.counters.open": 0x0110,
    "kv.counters.add": 0x0111,
    "kv.counters.get": 0x0112,
    "kv.counters.delete": 0x0113,
    "kv.counters.clear": 0x0114,
    "kv.rateLimit.open": 0x0120,
    "kv.quota.open": 0x0121,
    "kv.allow": 0x0122,
    "kv.peek": 0x0123,
    "kv.reset": 0x0124,
    "kv.refund": 0x0125,
    "kv.once.open": 0x0130,
    "kv.once.run": 0x0131,
    "kv.once.get": 0x0132,
    "kv.once.delete": 0x0133,
    "kv.tx": 0x0140,
    # Answers once the key is held, or held past the wait, `in_use`; a CANCEL
    # takes the ask out of its line.
    "locks.hold": 0x0901,
    # A lease that ran out or was revoked is `lost`.
    "locks.renew": 0x0902,
    "locks.release": 0x0903,
    "metrics.read": 0x0601,
    "metrics.latest": 0x0602,
    "metrics.aggregate": 0x0603,
    "metrics.explain": 0x0604,
    "metrics.names": 0x0605,
    "metrics.labelNames": 0x0606,
    "metrics.labelValues": 0x0607,
    "metrics.description": 0x0608,
    "metrics.damaged": 0x0609,
    "metrics.ingest": 0x060A,
    "metrics.describe": 0x060B,
    "metrics.delete": 0x060C,
    "metrics.flush": 0x060D,
    "metrics.instruments": 0x060E,
    "metrics.ingestPrometheus": 0x060F,
    # Answers its RESPONSE at once; the store's memory for the whole text is
    # taken before a piece is kept, and the last piece is answered once the text
    # is written, every series or none. A cancel or a lost connection leaves
    # nothing written and lets go of what came.
    "metrics.uploadPrometheus": 0x0610,
    "records.open": 0x0501,
    "records.append": 0x0502,
    "records.list": 0x0503,
    "records.count": 0x0504,
    "records.top": 0x0505,
    "records.aggregate": 0x0506,
    "records.streams": 0x0507,
    "records.fieldNames": 0x0508,
    "records.poll": 0x0509,
    "records.tail": 0x050A,
    "records.lines": 0x050B,
    "records.damaged": 0x050C,
    "records.drop": 0x050D,
    "search.open": 0x0B01,
    "search.set": 0x0B02,
    "search.setMany": 0x0B03,
    # Answers whether the index held the document.
    "search.delete": 0x0B04,
    "search.get": 0x0B05,
    "search.search": 0x0B06,
    "search.status": 0x0B07,
    # Drops an index and every document it holds, in the background.
    "search.dropIndex": 0x0B08,
    "search.list": 0x0B09,
    # Stops the server as its closing does: the streams running finish, every
    # connection is told GOAWAY, and a sidecar gives back SERVE and its directory.
    "server.stop": 0x0001,
    "server.clock": 0x0002,
    "sql.open": 0x0301,
    "sql.query": 0x0302,
    "sql.exec": 0x0303,
    "sql.batch": 0x0304,
    "sql.tx": 0x0305,
    "sql.columns": 0x0306,
}


def _write_backup_write(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("include")) is not None:
        w.uint(1)
        w.array(len(x))
        for item0 in x:
            w.str(item0)
        n += 1
    if (x := v.get("include_encryption_key")) is not None:
        w.uint(2)
        w.bool(x)
        n += 1
    w.close_map(head, n)


def _read_backup_write(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                items0 = [r.str() for _ in range(r.array())]
                r.leave()
                read["include"] = items0
            case 2:
                read["include_encryption_key"] = r.bool()
            case _:
                r.skip()
    r.leave()
    return read


# What a backup takes beside every engine's files: the files of the
# program's it includes, by their paths inside the store's directory, and
# whether it includes the store's encryption key.
BackupWrite = Message(
    "backup.Write",
    {
        "include": (1, list_(str_)),
        "include_encryption_key": (2, bool_),
    },
    _write_backup_write,
    _read_backup_write,
)


def _write_backup_piece(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("bytes")) is not None:
        w.uint(1)
        w.bin(x)
        n += 1
    w.close_map(head, n)


def _read_backup_piece(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["bytes"] = r.bin()
            case _:
                r.skip()
    r.leave()
    return read


# A piece of the zip, in order.
BackupPiece = Message(
    "backup.Piece",
    {
        "bytes": (1, bin_),
    },
    _write_backup_piece,
    _read_backup_piece,
)


def _write_backup_written(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("files")) is not None:
        w.uint(1)
        w.uint(x)
        n += 1
    if (x := v.get("bytes")) is not None:
        w.uint(2)
        w.uint(x)
        n += 1
    if (x := v.get("key_left_out")) is not None:
        w.uint(3)
        w.bool(x)
        n += 1
    w.close_map(head, n)


def _read_backup_written(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["files"] = r.uint()
            case 2:
                read["bytes"] = r.uint()
            case 3:
                read["key_left_out"] = r.bool()
            case _:
                r.skip()
    r.leave()
    return read


# What the backup holds, once its zip has gone whole.
BackupWritten = Message(
    "backup.Written",
    {
        "files": (1, uint),
        "bytes": (2, uint),
        # the store seals values with a key the backup does not hold
        "key_left_out": (3, bool_),
    },
    _write_backup_written,
    _read_backup_written,
)


def _write_blobs_open(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("name")) is not None:
        w.uint(1)
        w.str(x)
        n += 1
    if (x := v.get("ttl")) is not None:
        w.uint(2)
        w.uint(x)
        n += 1
    if (x := v.get("max_file_size")) is not None:
        w.uint(3)
        w.uint(x)
        n += 1
    if (x := v.get("database")) is not None:
        w.uint(4)
        w.uint(x)
        n += 1
    w.close_map(head, n)


def _read_blobs_open(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["name"] = r.str()
            case 2:
                read["ttl"] = r.uint()
            case 3:
                read["max_file_size"] = r.uint()
            case 4:
                read["database"] = r.uint()
            case _:
                r.skip()
    r.leave()
    return read


# Opens a set of files by name, made the first time.
BlobsOpen = Message(
    "blobs.Open",
    {
        # [a-z0-9][a-z0-9_-]{0,63}
        "name": (1, str_),
        # the term of every file written without its own
        "ttl": (2, uint),
        # the largest file a write may leave
        "max_file_size": (3, uint),
        # a database's handle: the files live in its file, and put, head, delete, copy and rename join its transactions;
        # blobs.db when absent
        "database": (4, uint),
    },
    _write_blobs_open,
    _read_blobs_open,
)


def _write_blobs_at(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("handle")) is not None:
        w.uint(1)
        w.uint(x)
        n += 1
    if (x := v.get("folder")) is not None:
        w.uint(2)
        w.array(len(x))
        for item0 in x:
            w.str(item0)
        n += 1
    if (x := v.get("path")) is not None:
        w.uint(3)
        w.str(x)
        n += 1
    w.close_map(head, n)


def _read_blobs_at(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["handle"] = r.uint()
            case 2:
                items0 = [r.str() for _ in range(r.array())]
                r.leave()
                read["folder"] = items0
            case 3:
                read["path"] = r.str()
            case _:
                r.skip()
    r.leave()
    return read


# Where a call stands: the files, a folder's segments, a path within it.
BlobsAt = Message(
    "blobs.At",
    {
        "handle": (1, uint),
        "folder": (2, list_(str_)),
        "path": (3, str_),
    },
    _write_blobs_at,
    _read_blobs_at,
)


def _write_blobs_info(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("path")) is not None:
        w.uint(1)
        w.str(x)
        n += 1
    if (x := v.get("size")) is not None:
        w.uint(2)
        w.uint(x)
        n += 1
    if (x := v.get("etag")) is not None:
        w.uint(3)
        w.str(x)
        n += 1
    if (x := v.get("content_type")) is not None:
        w.uint(4)
        w.str(x)
        n += 1
    if (x := v.get("last_modified")) is not None:
        w.uint(5)
        w.int(x)
        n += 1
    if (x := v.get("expires")) is not None:
        w.uint(6)
        w.int(x)
        n += 1
    if (x := v.get("meta")) is not None:
        w.uint(7)
        w.map(len(x))
        for name0 in sorted(x):
            w.str(name0)
            w.str(x[name0])
        n += 1
    w.close_map(head, n)


def _read_blobs_info(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["path"] = r.str()
            case 2:
                read["size"] = r.uint()
            case 3:
                read["etag"] = r.str()
            case 4:
                read["content_type"] = r.str()
            case 5:
                read["last_modified"] = r.int()
            case 6:
                read["expires"] = r.int()
            case 7:
                named0: dict[str, Any] = {}
                names0: set[str] = set()
                for _ in range(r.map()):
                    name0 = r.name(names0)
                    named0[name0] = r.str()
                r.leave()
                read["meta"] = named0
            case _:
                r.skip()
    r.leave()
    return read


# What a file carries: what serving it needs.
BlobsInfo = Message(
    "blobs.Info",
    {
        # within the folder the call stood in
        "path": (1, str_),
        "size": (2, uint),
        # the SHA-256 of its bytes in hex, quoted
        "etag": (3, str_),
        "content_type": (4, str_),
        "last_modified": (5, int_),
        "expires": (6, int_),
        "meta": (7, names(str_)),
    },
    _write_blobs_info,
    _read_blobs_info,
)


def _write_blobs_write(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("handle")) is not None:
        w.uint(1)
        w.uint(x)
        n += 1
    if (x := v.get("folder")) is not None:
        w.uint(2)
        w.array(len(x))
        for item0 in x:
            w.str(item0)
        n += 1
    if (x := v.get("path")) is not None:
        w.uint(3)
        w.str(x)
        n += 1
    if (x := v.get("content_type")) is not None:
        w.uint(4)
        w.str(x)
        n += 1
    if (x := v.get("meta")) is not None:
        w.uint(5)
        w.map(len(x))
        for name0 in sorted(x):
            w.str(name0)
            w.str(x[name0])
        n += 1
    if (x := v.get("ttl")) is not None:
        w.uint(6)
        w.uint(x)
        n += 1
    if (x := v.get("if_match")) is not None:
        w.uint(7)
        w.str(x)
        n += 1
    if (x := v.get("size")) is not None:
        w.uint(8)
        w.uint(x)
        n += 1
    if (x := v.get("create")) is not None:
        w.uint(9)
        w.bool(x)
        n += 1
    if (x := v.get("bytes")) is not None:
        w.uint(10)
        w.bin(x)
        n += 1
    w.close_map(head, n)


def _read_blobs_write(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["handle"] = r.uint()
            case 2:
                items0 = [r.str() for _ in range(r.array())]
                r.leave()
                read["folder"] = items0
            case 3:
                read["path"] = r.str()
            case 4:
                read["content_type"] = r.str()
            case 5:
                named0: dict[str, Any] = {}
                names0: set[str] = set()
                for _ in range(r.map()):
                    name0 = r.name(names0)
                    named0[name0] = r.str()
                r.leave()
                read["meta"] = named0
            case 6:
                read["ttl"] = r.uint()
            case 7:
                read["if_match"] = r.str()
            case 8:
                read["size"] = r.uint()
            case 9:
                read["create"] = r.bool()
            case 10:
                read["bytes"] = r.bin()
            case _:
                r.skip()
    r.leave()
    return read


# A write of a whole file: a put, or with create a write only where no file
# is. Its bytes are in the message, or, for an upload, the DATA that follow.
BlobsWrite = Message(
    "blobs.Write",
    {
        "handle": (1, uint),
        "folder": (2, list_(str_)),
        "path": (3, str_),
        "content_type": (4, str_),
        "meta": (5, names(str_)),
        # the file's own term, from its write
        "ttl": (6, uint),
        # the ETag of the one file it may replace
        "if_match": (7, str_),
        # the body's length: one longer or shorter is refused
        "size": (8, uint),
        # writes only where no file is
        "create": (9, bool_),
        # a put's whole body; nothing for an upload
        "bytes": (10, bin_),
    },
    _write_blobs_write,
    _read_blobs_write,
)


def _write_blobs_written(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("info")) is not None:
        w.uint(1)
        BlobsInfo.write(w, x)
        n += 1
    w.close_map(head, n)


def _read_blobs_written(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["info"] = BlobsInfo.read(r)
            case _:
                r.skip()
    r.leave()
    return read


# What a write wrote: none when a create found a file there.
BlobsWritten = Message(
    "blobs.Written",
    {
        "info": (1, BlobsInfo),
    },
    _write_blobs_written,
    _read_blobs_written,
)


def _write_blobs_piece(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("bytes")) is not None:
        w.uint(1)
        w.bin(x)
        n += 1
    w.close_map(head, n)


def _read_blobs_piece(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["bytes"] = r.bin()
            case _:
                r.skip()
    r.leave()
    return read


# A piece of a file's bytes: an upload's DATA from the client, a read's from
# the server.
BlobsPiece = Message(
    "blobs.Piece",
    {
        "bytes": (1, bin_),
    },
    _write_blobs_piece,
    _read_blobs_piece,
)


def _write_blobs_got(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("info")) is not None:
        w.uint(1)
        BlobsInfo.write(w, x)
        n += 1
    if (x := v.get("bytes")) is not None:
        w.uint(2)
        w.bin(x)
        n += 1
    w.close_map(head, n)


def _read_blobs_got(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["info"] = BlobsInfo.read(r)
            case 2:
                read["bytes"] = r.bin()
            case _:
                r.skip()
    r.leave()
    return read


# A get's answer: what the file carries, none when there is no file, and all
# its bytes when they fit one message; a larger file's are left to blobs.read,
# so that a read of a range sends no byte before it.
BlobsGot = Message(
    "blobs.Got",
    {
        "info": (1, BlobsInfo),
        # the whole file, or nothing when it is larger
        "bytes": (2, bin_),
    },
    _write_blobs_got,
    _read_blobs_got,
)


def _write_blobs_read(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("handle")) is not None:
        w.uint(1)
        w.uint(x)
        n += 1
    if (x := v.get("folder")) is not None:
        w.uint(2)
        w.array(len(x))
        for item0 in x:
            w.str(item0)
        n += 1
    if (x := v.get("path")) is not None:
        w.uint(3)
        w.str(x)
        n += 1
    if (x := v.get("if_match")) is not None:
        w.uint(4)
        w.str(x)
        n += 1
    if (x := v.get("offset")) is not None:
        w.uint(5)
        w.uint(x)
        n += 1
    if (x := v.get("length")) is not None:
        w.uint(6)
        w.uint(x)
        n += 1
    w.close_map(head, n)


def _read_blobs_read(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["handle"] = r.uint()
            case 2:
                items0 = [r.str() for _ in range(r.array())]
                r.leave()
                read["folder"] = items0
            case 3:
                read["path"] = r.str()
            case 4:
                read["if_match"] = r.str()
            case 5:
                read["offset"] = r.uint()
            case 6:
                read["length"] = r.uint()
            case _:
                r.skip()
    r.leave()
    return read


# A read of a file's bytes, as S3's GET with a range and If-Match: the whole
# file, checked, its last piece sent once its bytes matched their SHA-256 and a
# changed byte ending the stream corrupt instead; or a range, not checked.
BlobsRead = Message(
    "blobs.Read",
    {
        "handle": (1, uint),
        "folder": (2, list_(str_)),
        "path": (3, str_),
        # the ETag a get gave: another file there, or none, is a conflict
        "if_match": (4, str_),
        # from the first byte when absent
        "offset": (5, uint),
        # to the last byte when absent
        "length": (6, uint),
    },
    _write_blobs_read,
    _read_blobs_read,
)


def _write_blobs_head(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("info")) is not None:
        w.uint(1)
        BlobsInfo.write(w, x)
        n += 1
    w.close_map(head, n)


def _read_blobs_head(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["info"] = BlobsInfo.read(r)
            case _:
                r.skip()
    r.leave()
    return read


BlobsHead = Message(
    "blobs.Head",
    {
        "info": (1, BlobsInfo),
    },
    _write_blobs_head,
    _read_blobs_head,
)


def _write_blobs_delete(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("handle")) is not None:
        w.uint(1)
        w.uint(x)
        n += 1
    if (x := v.get("folder")) is not None:
        w.uint(2)
        w.array(len(x))
        for item0 in x:
            w.str(item0)
        n += 1
    if (x := v.get("path")) is not None:
        w.uint(3)
        w.str(x)
        n += 1
    if (x := v.get("if_match")) is not None:
        w.uint(4)
        w.str(x)
        n += 1
    w.close_map(head, n)


def _read_blobs_delete(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["handle"] = r.uint()
            case 2:
                items0 = [r.str() for _ in range(r.array())]
                r.leave()
                read["folder"] = items0
            case 3:
                read["path"] = r.str()
            case 4:
                read["if_match"] = r.str()
            case _:
                r.skip()
    r.leave()
    return read


BlobsDelete = Message(
    "blobs.Delete",
    {
        "handle": (1, uint),
        "folder": (2, list_(str_)),
        "path": (3, str_),
        "if_match": (4, str_),
    },
    _write_blobs_delete,
    _read_blobs_delete,
)


def _write_blobs_move(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("handle")) is not None:
        w.uint(1)
        w.uint(x)
        n += 1
    if (x := v.get("folder")) is not None:
        w.uint(2)
        w.array(len(x))
        for item0 in x:
            w.str(item0)
        n += 1
    if (x := v.get("from")) is not None:
        w.uint(3)
        w.str(x)
        n += 1
    if (x := v.get("to")) is not None:
        w.uint(4)
        w.str(x)
        n += 1
    if (x := v.get("if_match")) is not None:
        w.uint(5)
        w.str(x)
        n += 1
    w.close_map(head, n)


def _read_blobs_move(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["handle"] = r.uint()
            case 2:
                items0 = [r.str() for _ in range(r.array())]
                r.leave()
                read["folder"] = items0
            case 3:
                read["from"] = r.str()
            case 4:
                read["to"] = r.str()
            case 5:
                read["if_match"] = r.str()
            case _:
                r.skip()
    r.leave()
    return read


# A copy or a rename: from a path to another of the same folder. A file at
# to is a conflict, unless ifMatch names the version to replace.
BlobsMove = Message(
    "blobs.Move",
    {
        "handle": (1, uint),
        "folder": (2, list_(str_)),
        "from": (3, str_),
        "to": (4, str_),
        "if_match": (5, str_),
    },
    _write_blobs_move,
    _read_blobs_move,
)


def _write_blobs_expire(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("handle")) is not None:
        w.uint(1)
        w.uint(x)
        n += 1
    if (x := v.get("folder")) is not None:
        w.uint(2)
        w.array(len(x))
        for item0 in x:
            w.str(item0)
        n += 1
    if (x := v.get("path")) is not None:
        w.uint(3)
        w.str(x)
        n += 1
    if (x := v.get("after")) is not None:
        w.uint(4)
        w.uint(x)
        n += 1
    w.close_map(head, n)


def _read_blobs_expire(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["handle"] = r.uint()
            case 2:
                items0 = [r.str() for _ in range(r.array())]
                r.leave()
                read["folder"] = items0
            case 3:
                read["path"] = r.str()
            case 4:
                read["after"] = r.uint()
            case _:
                r.skip()
    r.leave()
    return read


BlobsExpire = Message(
    "blobs.Expire",
    {
        "handle": (1, uint),
        "folder": (2, list_(str_)),
        "path": (3, str_),
        "after": (4, uint),
    },
    _write_blobs_expire,
    _read_blobs_expire,
)


def _write_blobs_found(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("found")) is not None:
        w.uint(1)
        w.bool(x)
        n += 1
    w.close_map(head, n)


def _read_blobs_found(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["found"] = r.bool()
            case _:
                r.skip()
    r.leave()
    return read


BlobsFound = Message(
    "blobs.Found",
    {
        "found": (1, bool_),
    },
    _write_blobs_found,
    _read_blobs_found,
)


def _write_blobs_list(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("handle")) is not None:
        w.uint(1)
        w.uint(x)
        n += 1
    if (x := v.get("folder")) is not None:
        w.uint(2)
        w.array(len(x))
        for item0 in x:
            w.str(item0)
        n += 1
    if (x := v.get("prefix")) is not None:
        w.uint(3)
        w.str(x)
        n += 1
    if (x := v.get("after")) is not None:
        w.uint(4)
        w.str(x)
        n += 1
    if (x := v.get("limit")) is not None:
        w.uint(5)
        w.uint(x)
        n += 1
    w.close_map(head, n)


def _read_blobs_list(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["handle"] = r.uint()
            case 2:
                items0 = [r.str() for _ in range(r.array())]
                r.leave()
                read["folder"] = items0
            case 3:
                read["prefix"] = r.str()
            case 4:
                read["after"] = r.str()
            case 5:
                read["limit"] = r.uint()
            case _:
                r.skip()
    r.leave()
    return read


# A page of a folder's files, in the byte order of their paths.
BlobsList = Message(
    "blobs.List",
    {
        "handle": (1, uint),
        "folder": (2, list_(str_)),
        # text within the folder
        "prefix": (3, str_),
        # a page's next
        "after": (4, str_),
        # 1000 at most, and when absent
        "limit": (5, uint),
    },
    _write_blobs_list,
    _read_blobs_list,
)


def _write_blobs_page(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("files")) is not None:
        w.uint(1)
        w.array(len(x))
        for item0 in x:
            BlobsInfo.write(w, item0)
        n += 1
    if (x := v.get("next")) is not None:
        w.uint(2)
        w.str(x)
        n += 1
    w.close_map(head, n)


def _read_blobs_page(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                items0 = [BlobsInfo.read(r) for _ in range(r.array())]
                r.leave()
                read["files"] = items0
            case 2:
                read["next"] = r.str()
            case _:
                r.skip()
    r.leave()
    return read


BlobsPage = Message(
    "blobs.Page",
    {
        "files": (1, list_(BlobsInfo)),
        "next": (2, str_),
    },
    _write_blobs_page,
    _read_blobs_page,
)


def _write_blobs_folder(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("handle")) is not None:
        w.uint(1)
        w.uint(x)
        n += 1
    if (x := v.get("folder")) is not None:
        w.uint(2)
        w.array(len(x))
        for item0 in x:
            w.str(item0)
        n += 1
    w.close_map(head, n)


def _read_blobs_folder(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["handle"] = r.uint()
            case 2:
                items0 = [r.str() for _ in range(r.array())]
                r.leave()
                read["folder"] = items0
            case _:
                r.skip()
    r.leave()
    return read


BlobsFolder = Message(
    "blobs.Folder",
    {
        "handle": (1, uint),
        "folder": (2, list_(str_)),
    },
    _write_blobs_folder,
    _read_blobs_folder,
)


def _write_blobs_usage(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("count")) is not None:
        w.uint(1)
        w.uint(x)
        n += 1
    if (x := v.get("size")) is not None:
        w.uint(2)
        w.uint(x)
        n += 1
    w.close_map(head, n)


def _read_blobs_usage(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["count"] = r.uint()
            case 2:
                read["size"] = r.uint()
            case _:
                r.skip()
    r.leave()
    return read


BlobsUsage = Message(
    "blobs.Usage",
    {
        "count": (1, uint),
        "size": (2, uint),
    },
    _write_blobs_usage,
    _read_blobs_usage,
)


def _write_channels_publish(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("name")) is not None:
        w.uint(1)
        w.str(x)
        n += 1
    if (x := v.get("under")) is not None:
        w.uint(2)
        w.array(len(x))
        for item0 in x:
            w.str(item0)
        n += 1
    if (x := v.get("value")) is not None:
        w.uint(3)
        w.str(x)
        n += 1
    w.close_map(head, n)


def _read_channels_publish(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["name"] = r.str()
            case 2:
                items0 = [r.str() for _ in range(r.array())]
                r.leave()
                read["under"] = items0
            case 3:
                read["value"] = r.str()
            case _:
                r.skip()
    r.leave()
    return read


# A message published on a channel's branch, as docs/guides/channels/README.md has
# it: every subscription listening there now receives it.
ChannelsPublish = Message(
    "channels.Publish",
    {
        # [a-z0-9][a-z0-9_-]{0,63}
        "name": (1, str_),
        # the branch's owners, 1 KiB together at most
        "under": (2, list_(str_)),
        # the message, 1 MiB at most
        "value": (3, str_),
    },
    _write_channels_publish,
    _read_channels_publish,
)


def _write_channels_subscribe(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("name")) is not None:
        w.uint(1)
        w.str(x)
        n += 1
    if (x := v.get("under")) is not None:
        w.uint(2)
        w.array(len(x))
        for item0 in x:
            w.str(item0)
        n += 1
    if (x := v.get("recursive")) is not None:
        w.uint(3)
        w.bool(x)
        n += 1
    w.close_map(head, n)


def _read_channels_subscribe(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["name"] = r.str()
            case 2:
                items0 = [r.str() for _ in range(r.array())]
                r.leave()
                read["under"] = items0
            case 3:
                read["recursive"] = r.bool()
            case _:
                r.skip()
    r.leave()
    return read


# A subscription of a branch, or of it and every branch below it.
ChannelsSubscribe = Message(
    "channels.Subscribe",
    {
        "name": (1, str_),
        "under": (2, list_(str_)),
        "recursive": (3, bool_),
    },
    _write_channels_subscribe,
    _read_channels_subscribe,
)


def _write_channels_message(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("under")) is not None:
        w.uint(1)
        w.array(len(x))
        for item0 in x:
            w.str(item0)
        n += 1
    if (x := v.get("value")) is not None:
        w.uint(2)
        w.str(x)
        n += 1
    w.close_map(head, n)


def _read_channels_message(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                items0 = [r.str() for _ in range(r.array())]
                r.leave()
                read["under"] = items0
            case 2:
                read["value"] = r.str()
            case _:
                r.skip()
    r.leave()
    return read


ChannelsMessage = Message(
    "channels.Message",
    {
        # the branch it was published under
        "under": (1, list_(str_)),
        "value": (2, str_),
    },
    _write_channels_message,
    _read_channels_message,
)


def _write_config_open(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("name")) is not None:
        w.uint(1)
        w.str(x)
        n += 1
    if (x := v.get("declaration")) is not None:
        w.uint(2)
        w.str(x)
        n += 1
    if (x := v.get("layers")) is not None:
        w.uint(3)
        w.str(x)
        n += 1
    if (x := v.get("strict")) is not None:
        w.uint(4)
        w.bool(x)
        n += 1
    if (x := v.get("kept")) is not None:
        w.uint(5)
        w.bool(x)
        n += 1
    w.close_map(head, n)


def _read_config_open(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["name"] = r.str()
            case 2:
                read["declaration"] = r.str()
            case 3:
                read["layers"] = r.str()
            case 4:
                read["strict"] = r.bool()
            case 5:
                read["kept"] = r.bool()
            case _:
                r.skip()
    r.leave()
    return read


# A config a program opens: its declaration and its layers as data, as
# docs/guides/config/README.md has them, with what the program read itself, a file's
# text and its variables, since those belong to it and not to the server.
ConfigOpen = Message(
    "config.Open",
    {
        # [a-z0-9][a-z0-9_-]{0,63}
        "name": (1, str_),
        # each setting's kind, rules, default and marks
        "declaration": (2, str_),
        # objects, files and the environment, in the order written
        "layers": (3, str_),
        # a kept change that no longer fits refuses the open
        "strict": (4, bool_),
        # as kv.db keeps it, for a command: the last declaration a program opened it with,
        "kept": (5, bool_),
    },
    _write_config_open,
    _read_config_open,
)


def _write_config_origin(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("kind")) is not None:
        w.uint(1)
        w.str(x)
        n += 1
    if (x := v.get("place")) is not None:
        w.uint(2)
        w.uint(x)
        n += 1
    if (x := v.get("path")) is not None:
        w.uint(3)
        w.str(x)
        n += 1
    if (x := v.get("variable")) is not None:
        w.uint(4)
        w.str(x)
        n += 1
    if (x := v.get("dotenv")) is not None:
        w.uint(5)
        w.str(x)
        n += 1
    if (x := v.get("file")) is not None:
        w.uint(6)
        w.str(x)
        n += 1
    if (x := v.get("version")) is not None:
        w.uint(7)
        w.int(x)
        n += 1
    if (x := v.get("at")) is not None:
        w.uint(8)
        w.int(x)
        n += 1
    w.close_map(head, n)


def _read_config_origin(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["kind"] = r.str()
            case 2:
                read["place"] = r.uint()
            case 3:
                read["path"] = r.str()
            case 4:
                read["variable"] = r.str()
            case 5:
                read["dotenv"] = r.str()
            case 6:
                read["file"] = r.str()
            case 7:
                read["version"] = r.int()
            case 8:
                read["at"] = r.int()
            case _:
                r.skip()
    r.leave()
    return read


# Where a value came from: its kind, and only the fields that kind names.
ConfigOrigin = Message(
    "config.Origin",
    {
        # declaration, layer, file, env or change
        "kind": (1, str_),
        # a layer's place in the list, the declaration 1
        "place": (2, uint),
        # a file's path
        "path": (3, str_),
        # an env's variable
        "variable": (4, str_),
        # the .env file that set the variable
        "dotenv": (5, str_),
        # the file a NAME_FILE variable named
        "file": (6, str_),
        # a change's version
        "version": (7, int_),
        # when a change was made
        "at": (8, int_),
    },
    _write_config_origin,
    _read_config_origin,
)


def _write_config_update(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("handle")) is not None:
        w.uint(1)
        w.uint(x)
        n += 1
    if (x := v.get("version")) is not None:
        w.uint(2)
        w.int(x)
        n += 1
    if (x := v.get("changes")) is not None:
        w.uint(3)
        w.str(x)
        n += 1
    w.close_map(head, n)


def _read_config_update(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["handle"] = r.uint()
            case 2:
                read["version"] = r.int()
            case 3:
                read["changes"] = r.str()
            case _:
                r.skip()
    r.leave()
    return read


# A change of live settings, made on the version the program's value had.
ConfigUpdate = Message(
    "config.Update",
    {
        "handle": (1, uint),
        # a change made since is a conflict
        "version": (2, int_),
        # the settings it changes, in the declaration's shape
        "changes": (3, str_),
    },
    _write_config_update,
    _read_config_update,
)


def _write_config_reset(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("handle")) is not None:
        w.uint(1)
        w.uint(x)
        n += 1
    if (x := v.get("path")) is not None:
        w.uint(2)
        w.str(x)
        n += 1
    w.close_map(head, n)


def _read_config_reset(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["handle"] = r.uint()
            case 2:
                read["path"] = r.str()
            case _:
                r.skip()
    r.leave()
    return read


# Drops the changes kept of a setting or of a group, or of every setting.
ConfigReset = Message(
    "config.Reset",
    {
        "handle": (1, uint),
        # empty for every change
        "path": (2, str_),
    },
    _write_config_reset,
    _read_config_reset,
)


def _write_hello(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("protocol")) is not None:
        w.uint(1)
        w.uint(x)
        n += 1
    if (x := v.get("client")) is not None:
        w.uint(2)
        w.str(x)
        n += 1
    if (x := v.get("token")) is not None:
        w.uint(3)
        w.str(x)
        n += 1
    if (x := v.get("max_body")) is not None:
        w.uint(4)
        w.uint(x)
        n += 1
    if (x := v.get("stream_credit")) is not None:
        w.uint(5)
        w.uint(x)
        n += 1
    if (x := v.get("challenge")) is not None:
        w.uint(6)
        w.bin(x)
        n += 1
    w.close_map(head, n)


def _read_hello(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["protocol"] = r.uint()
            case 2:
                read["client"] = r.str()
            case 3:
                read["token"] = r.str()
            case 4:
                read["max_body"] = r.uint()
            case 5:
                read["stream_credit"] = r.uint()
            case 6:
                read["challenge"] = r.bin()
            case _:
                r.skip()
    r.leave()
    return read


# What a client says first.
Hello = Message(
    "Hello",
    {
        # the newest the client speaks
        "protocol": (1, uint),
        # a name and version, for logs
        "client": (2, str_),
        # required on TCP
        "token": (3, str_),
        # the largest body the client takes; the server's when absent
        "max_body": (4, uint),
        # the DATA the server may send a stream before the client grants more; 2 MiB when absent
        "stream_credit": (5, uint),
        # 16 random bytes, when the client found the server through SERVE
        "challenge": (6, bin_),
    },
    _write_hello,
    _read_hello,
)


def _write_welcome(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("protocol")) is not None:
        w.uint(1)
        w.uint(x)
        n += 1
    if (x := v.get("server")) is not None:
        w.uint(2)
        w.str(x)
        n += 1
    if (x := v.get("instance")) is not None:
        w.uint(3)
        w.bin(x)
        n += 1
    if (x := v.get("capability")) is not None:
        w.uint(4)
        w.str(x)
        n += 1
    if (x := v.get("max_body")) is not None:
        w.uint(5)
        w.uint(x)
        n += 1
    if (x := v.get("in_flight")) is not None:
        w.uint(6)
        w.uint(x)
        n += 1
    if (x := v.get("connection_credit")) is not None:
        w.uint(7)
        w.uint(x)
        n += 1
    if (x := v.get("stream_credit")) is not None:
        w.uint(8)
        w.uint(x)
        n += 1
    if (x := v.get("engines")) is not None:
        w.uint(9)
        w.array(len(x))
        for item0 in x:
            w.str(item0)
        n += 1
    if (x := v.get("now")) is not None:
        w.uint(10)
        w.int(x)
        n += 1
    if (x := v.get("proof")) is not None:
        w.uint(11)
        w.bin(x)
        n += 1
    if (x := v.get("durability")) is not None:
        w.uint(12)
        w.str(x)
        n += 1
    w.close_map(head, n)


def _read_welcome(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["protocol"] = r.uint()
            case 2:
                read["server"] = r.str()
            case 3:
                read["instance"] = r.bin()
            case 4:
                read["capability"] = r.str()
            case 5:
                read["max_body"] = r.uint()
            case 6:
                read["in_flight"] = r.uint()
            case 7:
                read["connection_credit"] = r.uint()
            case 8:
                read["stream_credit"] = r.uint()
            case 9:
                items0 = [r.str() for _ in range(r.array())]
                r.leave()
                read["engines"] = items0
            case 10:
                read["now"] = r.int()
            case 11:
                read["proof"] = r.bin()
            case 12:
                read["durability"] = r.str()
            case _:
                r.skip()
    r.leave()
    return read


# What a server answers HELLO with: what the connection agrees.
Welcome = Message(
    "Welcome",
    {
        # the one this connection speaks
        "protocol": (1, uint),
        # its version
        "server": (2, str_),
        # 16 random bytes a start
        "instance": (3, bin_),
        # admin, data or read
        "capability": (4, str_),
        # the largest body either side sends
        "max_body": (5, uint),
        # the streams a client may have open at once
        "in_flight": (6, uint),
        # the REQUEST and DATA bytes a client may send before credit comes back
        "connection_credit": (7, uint),
        # the DATA bytes a client may send a stream before credit comes back
        "stream_credit": (8, uint),
        # what this server serves
        "engines": (9, list_(str_)),
        # the store's clock
        "now": (10, int_),
        # the HMAC-SHA256 of HELLO's challenge, keyed with SERVE's secret
        "proof": (11, bin_),
        # full or os, when the store was opened with one: how far its files' commits go
        "durability": (12, str_),
    },
    _write_welcome,
    _read_welcome,
)


def _write_store_options(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("durability")) is not None:
        w.uint(1)
        w.str(x)
        n += 1
    if (x := v.get("encryption_key_file")) is not None:
        w.uint(2)
        w.str(x)
        n += 1
    if (x := v.get("node")) is not None:
        w.uint(3)
        w.bool(x)
        n += 1
    if (x := v.get("from")) is not None:
        w.uint(4)
        w.array(len(x))
        for item0 in x:
            w.str(item0)
        n += 1
    if (x := v.get("serve")) is not None:
        w.uint(5)
        w.str(x)
        n += 1
    if (x := v.get("files")) is not None:
        w.uint(6)
        w.array(len(x))
        for item0 in x:
            w.str(item0)
        n += 1
    if (x := v.get("token")) is not None:
        w.uint(7)
        w.str(x)
        n += 1
    if (x := v.get("synchronous")) is not None:
        w.uint(8)
        w.bool(x)
        n += 1
    if (x := v.get("catalog")) is not None:
        w.uint(9)
        w.bool(x)
        n += 1
    if (x := v.get("advertise")) is not None:
        w.uint(10)
        w.str(x)
        n += 1
    if (x := v.get("synchronous_with")) is not None:
        w.uint(11)
        w.array(len(x))
        for item0 in x:
            w.str(item0)
        n += 1
    if (x := v.get("insecure")) is not None:
        w.uint(12)
        w.bool(x)
        n += 1
    if (x := v.get("self_metrics")) is not None:
        w.uint(13)
        w.bool(x)
        n += 1
    if (x := v.get("without_system")) is not None:
        w.uint(14)
        w.bool(x)
        n += 1
    if (x := v.get("max_series")) is not None:
        w.uint(15)
        w.uint(x)
        n += 1
    if (x := v.get("older_encryption_keys")) is not None:
        w.uint(16)
        w.array(len(x))
        for item0 in x:
            w.str(item0)
        n += 1
    w.close_map(head, n)


def _read_store_options(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["durability"] = r.str()
            case 2:
                read["encryption_key_file"] = r.str()
            case 3:
                read["node"] = r.bool()
            case 4:
                items0 = [r.str() for _ in range(r.array())]
                r.leave()
                read["from"] = items0
            case 5:
                read["serve"] = r.str()
            case 6:
                items0 = [r.str() for _ in range(r.array())]
                r.leave()
                read["files"] = items0
            case 7:
                read["token"] = r.str()
            case 8:
                read["synchronous"] = r.bool()
            case 9:
                read["catalog"] = r.bool()
            case 10:
                read["advertise"] = r.str()
            case 11:
                items0 = [r.str() for _ in range(r.array())]
                r.leave()
                read["synchronous_with"] = items0
            case 12:
                read["insecure"] = r.bool()
            case 13:
                read["self_metrics"] = r.bool()
            case 14:
                read["without_system"] = r.bool()
            case 15:
                read["max_series"] = r.uint()
            case 16:
                items0 = [r.str() for _ in range(r.array())]
                r.leave()
                read["older_encryption_keys"] = items0
            case _:
                r.skip()
    r.leave()
    return read


# How a host opens the store in its own process: the pipe's open takes it,
# and a server reads the same from its command line.
StoreOptions = Message(
    "store.Options",
    {
        # full or os: how far a commit of the store's files goes before it returns; each engine's own when absent
        "durability": (1, str_),
        # the file of the store's encryption key, which the store only reads; encryption.key of its directory, made when
        # needed, when absent
        "encryption_key_file": (2, str_),
        # makes the store a node, which keeps a log of each file it writes; from or serve make it one too
        "node": (3, bool_),
        # where the node takes the files it copies, host:port, tried in order
        "from": (4, list_(str_)),
        # where other nodes take files from this one, host:port
        "serve": (5, str_),
        # the files it copies, as kv.db or sql/app.db; every file but its own records and metrics when empty
        "files": (6, list_(str_)),
        # what it says to the node it takes from, and asks of the nodes that take from it
        "token": (7, str_),
        # a write of a file it writes answers once a node copying it holds the commit
        "synchronous": (8, bool_),
        # it is the home of the catalog, which names each file's home
        "catalog": (9, bool_),
        # where other nodes reach it, host:port; the address serve listens on when absent
        "advertise": (10, str_),
        # the nodes, by address or id, a synchronous commit waits for one of; any when empty
        "synchronous_with": (11, list_(str_)),
        # plain TCP on links beyond this machine, which anyone on the way reads
        "insecure": (12, bool_),
        # measures the store into its own metrics.db, under names that start with tinystore_
        "self_metrics": (13, bool_),
        # with selfMetrics, leaves out the machine it runs on: its process's memory and processor, the system's
        "without_system": (14, bool_),
        # the metrics series the store holds at most; 100,000 when absent
        "max_series": (15, uint),
        # the files of the store's older keys, which open the values they sealed and seal none
        "older_encryption_keys": (16, list_(str_)),
    },
    _write_store_options,
    _read_store_options,
)


def _write_go_away(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("code")) is not None:
        w.uint(1)
        w.str(x)
        n += 1
    if (x := v.get("message")) is not None:
        w.uint(2)
        w.str(x)
        n += 1
    w.close_map(head, n)


def _read_go_away(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["code"] = r.str()
            case 2:
                read["message"] = r.str()
            case _:
                r.skip()
    r.leave()
    return read


# The last frame of a connection.
GoAway = Message(
    "GoAway",
    {
        "code": (1, str_),
        "message": (2, str_),
    },
    _write_go_away,
    _read_go_away,
)


def _write_failure(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("code")) is not None:
        w.uint(1)
        w.str(x)
        n += 1
    if (x := v.get("message")) is not None:
        w.uint(2)
        w.str(x)
        n += 1
    if (x := v.get("what")) is not None:
        w.uint(3)
        w.map(len(x))
        for name0 in sorted(x):
            w.str(name0)
            w.str(x[name0])
        n += 1
    w.close_map(head, n)


def _read_failure(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["code"] = r.str()
            case 2:
                read["message"] = r.str()
            case 3:
                named0: dict[str, Any] = {}
                names0: set[str] = set()
                for _ in range(r.map()):
                    name0 = r.name(names0)
                    named0[name0] = r.str()
                r.leave()
                read["what"] = named0
            case _:
                r.skip()
    r.leave()
    return read


# A stream's last frame when it failed, with ERROR set: its kind's code, what
# failed, and the item it names. A message refused is one too.
Failure = Message(
    "Failure",
    {
        "code": (1, str_),
        "message": (2, str_),
        "what": (3, names(str_)),
    },
    _write_failure,
    _read_failure,
)


def _write_handle(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("handle")) is not None:
        w.uint(1)
        w.uint(x)
        n += 1
    w.close_map(head, n)


def _read_handle(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["handle"] = r.uint()
            case _:
                r.skip()
    r.leave()
    return read


# What a call that opens something answers: its handle, which later calls name.
Handle = Message(
    "Handle",
    {
        "handle": (1, uint),
    },
    _write_handle,
    _read_handle,
)


def _write_empty(w: Writer, v: Mapping[str, Any]) -> None:
    w.map(0)


def _read_empty(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        r.field(seen)
        r.skip()
    r.leave()
    return read


# A message with nothing to say.
Empty = Message(
    "Empty",
    {},
    _write_empty,
    _read_empty,
)


def _write_jobs_concurrency(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("total")) is not None:
        w.uint(1)
        w.uint(x)
        n += 1
    if (x := v.get("group")) is not None:
        w.uint(2)
        w.uint(x)
        n += 1
    w.close_map(head, n)


def _read_jobs_concurrency(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["total"] = r.uint()
            case 2:
                read["group"] = r.uint()
            case _:
                r.skip()
    r.leave()
    return read


# How many jobs of a queue run at once: in all, across every worker of the
# store, and in each group.
JobsConcurrency = Message(
    "jobs.Concurrency",
    {
        "total": (1, uint),
        "group": (2, uint),
    },
    _write_jobs_concurrency,
    _read_jobs_concurrency,
)


def _write_jobs_backoff(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("initial")) is not None:
        w.uint(1)
        w.uint(x)
        n += 1
    if (x := v.get("max")) is not None:
        w.uint(2)
        w.uint(x)
        n += 1
    w.close_map(head, n)


def _read_jobs_backoff(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["initial"] = r.uint()
            case 2:
                read["max"] = r.uint()
            case _:
                r.skip()
    r.leave()
    return read


# The wait before a retry: initial, doubling each time up to max.
JobsBackoff = Message(
    "jobs.Backoff",
    {
        "initial": (1, uint),
        "max": (2, uint),
    },
    _write_jobs_backoff,
    _read_jobs_backoff,
)


def _write_jobs_rate(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("count")) is not None:
        w.uint(1)
        w.uint(x)
        n += 1
    if (x := v.get("per")) is not None:
        w.uint(2)
        w.uint(x)
        n += 1
    w.close_map(head, n)


def _read_jobs_rate(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["count"] = r.uint()
            case 2:
                read["per"] = r.uint()
            case _:
                r.skip()
    r.leave()
    return read


# Jobs started in any span of per.
JobsRate = Message(
    "jobs.Rate",
    {
        "count": (1, uint),
        "per": (2, uint),
    },
    _write_jobs_rate,
    _read_jobs_rate,
)


def _write_jobs_queue_open(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("name")) is not None:
        w.uint(1)
        w.str(x)
        n += 1
    if (x := v.get("attempts")) is not None:
        w.uint(2)
        w.uint(x)
        n += 1
    if (x := v.get("backoff")) is not None:
        w.uint(3)
        JobsBackoff.write(w, x)
        n += 1
    if (x := v.get("timeout")) is not None:
        w.uint(4)
        w.uint(x)
        n += 1
    if (x := v.get("concurrency")) is not None:
        w.uint(5)
        JobsConcurrency.write(w, x)
        n += 1
    if (x := v.get("rate")) is not None:
        w.uint(6)
        JobsRate.write(w, x)
        n += 1
    if (x := v.get("dedupe")) is not None:
        w.uint(7)
        w.uint(x)
        n += 1
    if (x := v.get("keep")) is not None:
        w.uint(8)
        w.uint(x)
        n += 1
    if (x := v.get("max_waiting")) is not None:
        w.uint(9)
        w.uint(x)
        n += 1
    if (x := v.get("database")) is not None:
        w.uint(10)
        w.uint(x)
        n += 1
    if (x := v.get("group_rate")) is not None:
        w.uint(11)
        JobsRate.write(w, x)
        n += 1
    w.close_map(head, n)


def _read_jobs_queue_open(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["name"] = r.str()
            case 2:
                read["attempts"] = r.uint()
            case 3:
                read["backoff"] = JobsBackoff.read(r)
            case 4:
                read["timeout"] = r.uint()
            case 5:
                read["concurrency"] = JobsConcurrency.read(r)
            case 6:
                read["rate"] = JobsRate.read(r)
            case 7:
                read["dedupe"] = r.uint()
            case 8:
                read["keep"] = r.uint()
            case 9:
                read["max_waiting"] = r.uint()
            case 10:
                read["database"] = r.uint()
            case 11:
                read["group_rate"] = JobsRate.read(r)
            case _:
                r.skip()
    r.leave()
    return read


# Opens a queue; an option left out takes its default.
JobsQueueOpen = Message(
    "jobs.QueueOpen",
    {
        # [a-z0-9][a-z0-9_-]{0,63}
        "name": (1, str_),
        # 10 when absent, the first run counted
        "attempts": (2, uint),
        # 1 s doubling to 1 h when absent
        "backoff": (3, JobsBackoff),
        # how long one run may take: a minute when absent
        "timeout": (4, uint),
        # one at a time in each worker when absent
        "concurrency": (5, JobsConcurrency),
        "rate": (6, JobsRate),
        # how long a done job's id stays taken
        "dedupe": (7, uint),
        # how long a failed job stays: a week when absent
        "keep": (8, uint),
        # ten million when absent
        "max_waiting": (9, uint),
        # a database's handle: the queue lives in its file; jobs.db when absent
        "database": (10, uint),
        # each group's starts, beside the queue's rate
        "group_rate": (11, JobsRate),
    },
    _write_jobs_queue_open,
    _read_jobs_queue_open,
)


def _write_jobs_schedule_open(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("name")) is not None:
        w.uint(1)
        w.str(x)
        n += 1
    if (x := v.get("every")) is not None:
        w.uint(2)
        w.uint(x)
        n += 1
    if (x := v.get("cron")) is not None:
        w.uint(3)
        w.str(x)
        n += 1
    if (x := v.get("time_zone")) is not None:
        w.uint(4)
        w.str(x)
        n += 1
    if (x := v.get("attempts")) is not None:
        w.uint(5)
        w.uint(x)
        n += 1
    if (x := v.get("backoff")) is not None:
        w.uint(6)
        JobsBackoff.write(w, x)
        n += 1
    if (x := v.get("timeout")) is not None:
        w.uint(7)
        w.uint(x)
        n += 1
    w.close_map(head, n)


def _read_jobs_schedule_open(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["name"] = r.str()
            case 2:
                read["every"] = r.uint()
            case 3:
                read["cron"] = r.str()
            case 4:
                read["time_zone"] = r.str()
            case 5:
                read["attempts"] = r.uint()
            case 6:
                read["backoff"] = JobsBackoff.read(r)
            case 7:
                read["timeout"] = r.uint()
            case _:
                r.skip()
    r.leave()
    return read


# Opens a schedule: one repeating job the code owns, under its name, whose
# repeat replaces the one kept.
JobsScheduleOpen = Message(
    "jobs.ScheduleOpen",
    {
        "name": (1, str_),
        "every": (2, uint),
        # five fields, with timeZone
        "cron": (3, str_),
        # an IANA name, UTC among them
        "time_zone": (4, str_),
        "attempts": (5, uint),
        "backoff": (6, JobsBackoff),
        "timeout": (7, uint),
    },
    _write_jobs_schedule_open,
    _read_jobs_schedule_open,
)


def _write_jobs_call(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("handle")) is not None:
        w.uint(1)
        w.uint(x)
        n += 1
    if (x := v.get("id")) is not None:
        w.uint(2)
        w.str(x)
        n += 1
    if (x := v.get("value")) is not None:
        w.uint(3)
        w.str(x)
        n += 1
    if (x := v.get("at")) is not None:
        w.uint(4)
        w.int(x)
        n += 1
    if (x := v.get("delay")) is not None:
        w.uint(5)
        w.uint(x)
        n += 1
    if (x := v.get("group")) is not None:
        w.uint(6)
        w.str(x)
        n += 1
    if (x := v.get("every")) is not None:
        w.uint(7)
        w.uint(x)
        n += 1
    if (x := v.get("cron")) is not None:
        w.uint(8)
        w.str(x)
        n += 1
    if (x := v.get("time_zone")) is not None:
        w.uint(9)
        w.str(x)
        n += 1
    w.close_map(head, n)


def _read_jobs_call(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["handle"] = r.uint()
            case 2:
                read["id"] = r.str()
            case 3:
                read["value"] = r.str()
            case 4:
                read["at"] = r.int()
            case 5:
                read["delay"] = r.uint()
            case 6:
                read["group"] = r.str()
            case 7:
                read["every"] = r.uint()
            case 8:
                read["cron"] = r.str()
            case 9:
                read["time_zone"] = r.str()
            case _:
                r.skip()
    r.leave()
    return read


# A call on one job: an add, a set or an update.
JobsCall = Message(
    "jobs.Call",
    {
        "handle": (1, uint),
        # 1 to 1024 bytes; a set and an update need one
        "id": (2, str_),
        "value": (3, str_),
        "at": (4, int_),
        # from now; not beside at
        "delay": (5, uint),
        "group": (6, str_),
        # a repeat, which needs an id
        "every": (7, uint),
        "cron": (8, str_),
        "time_zone": (9, str_),
    },
    _write_jobs_call,
    _read_jobs_call,
)


def _write_jobs_id(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("handle")) is not None:
        w.uint(1)
        w.uint(x)
        n += 1
    if (x := v.get("id")) is not None:
        w.uint(2)
        w.str(x)
        n += 1
    w.close_map(head, n)


def _read_jobs_id(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["handle"] = r.uint()
            case 2:
                read["id"] = r.str()
            case _:
                r.skip()
    r.leave()
    return read


# The job under an id.
JobsId = Message(
    "jobs.Id",
    {
        "handle": (1, uint),
        "id": (2, str_),
    },
    _write_jobs_id,
    _read_jobs_id,
)


def _write_jobs_changed(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("changed")) is not None:
        w.uint(1)
        w.bool(x)
        n += 1
    w.close_map(head, n)


def _read_jobs_changed(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["changed"] = r.bool()
            case _:
                r.skip()
    r.leave()
    return read


# Whether a call did what it asked: an add added, an update changed, a cancel
# found a job.
JobsChanged = Message(
    "jobs.Changed",
    {
        "changed": (1, bool_),
    },
    _write_jobs_changed,
    _read_jobs_changed,
)


def _write_jobs_job(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("found")) is not None:
        w.uint(1)
        w.bool(x)
        n += 1
    if (x := v.get("id")) is not None:
        w.uint(2)
        w.str(x)
        n += 1
    if (x := v.get("value")) is not None:
        w.uint(3)
        w.str(x)
        n += 1
    if (x := v.get("state")) is not None:
        w.uint(4)
        w.str(x)
        n += 1
    if (x := v.get("at")) is not None:
        w.uint(5)
        w.int(x)
        n += 1
    if (x := v.get("attempt")) is not None:
        w.uint(6)
        w.uint(x)
        n += 1
    if (x := v.get("ahead")) is not None:
        w.uint(7)
        w.uint(x)
        n += 1
    if (x := v.get("progress")) is not None:
        w.uint(8)
        w.str(x)
        n += 1
    if (x := v.get("error")) is not None:
        w.uint(9)
        w.str(x)
        n += 1
    if (x := v.get("group")) is not None:
        w.uint(10)
        w.str(x)
        n += 1
    if (x := v.get("repeat")) is not None:
        w.uint(11)
        w.str(x)
        n += 1
    if (x := v.get("started_at")) is not None:
        w.uint(12)
        w.int(x)
        n += 1
    if (x := v.get("ended_at")) is not None:
        w.uint(13)
        w.int(x)
        n += 1
    w.close_map(head, n)


def _read_jobs_job(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["found"] = r.bool()
            case 2:
                read["id"] = r.str()
            case 3:
                read["value"] = r.str()
            case 4:
                read["state"] = r.str()
            case 5:
                read["at"] = r.int()
            case 6:
                read["attempt"] = r.uint()
            case 7:
                read["ahead"] = r.uint()
            case 8:
                read["progress"] = r.str()
            case 9:
                read["error"] = r.str()
            case 10:
                read["group"] = r.str()
            case 11:
                read["repeat"] = r.str()
            case 12:
                read["started_at"] = r.int()
            case 13:
                read["ended_at"] = r.int()
            case _:
                r.skip()
    r.leave()
    return read


# A job as its queue holds it.
JobsJob = Message(
    "jobs.Job",
    {
        "found": (1, bool_),
        "id": (2, str_),
        "value": (3, str_),
        # scheduled, waiting, running, done, failed or cancelled
        "state": (4, str_),
        # when it runs next; for one done or failed, when its last run was for
        "at": (5, int_),
        "attempt": (6, uint),
        # the jobs that run before a waiting one, up to 10,000
        "ahead": (7, uint),
        "progress": (8, str_),
        "error": (9, str_),
        "group": (10, str_),
        # as the job keeps it: @every 30s +6178ms, 10 3 * * * Europe/Berlin
        "repeat": (11, str_),
        # the last run a handler finished
        "started_at": (12, int_),
        "ended_at": (13, int_),
    },
    _write_jobs_job,
    _read_jobs_job,
)


def _write_jobs_list(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("handle")) is not None:
        w.uint(1)
        w.uint(x)
        n += 1
    if (x := v.get("prefix")) is not None:
        w.uint(2)
        w.str(x)
        n += 1
    if (x := v.get("state")) is not None:
        w.uint(3)
        w.str(x)
        n += 1
    if (x := v.get("after")) is not None:
        w.uint(4)
        w.str(x)
        n += 1
    if (x := v.get("limit")) is not None:
        w.uint(5)
        w.uint(x)
        n += 1
    w.close_map(head, n)


def _read_jobs_list(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["handle"] = r.uint()
            case 2:
                read["prefix"] = r.str()
            case 3:
                read["state"] = r.str()
            case 4:
                read["after"] = r.str()
            case 5:
                read["limit"] = r.uint()
            case _:
                r.skip()
    r.leave()
    return read


# A page of a queue's jobs: those whose ids start with a prefix, in the byte
# order of their ids, or with no prefix and the failed state, the last failed
# first.
JobsList = Message(
    "jobs.List",
    {
        "handle": (1, uint),
        "prefix": (2, str_),
        # scheduled, waiting, running or failed
        "state": (3, str_),
        # the page before's next
        "after": (4, str_),
        # 100 when absent, 1000 at most
        "limit": (5, uint),
    },
    _write_jobs_list,
    _read_jobs_list,
)


def _write_jobs_page(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("jobs")) is not None:
        w.uint(1)
        w.array(len(x))
        for item0 in x:
            JobsJob.write(w, item0)
        n += 1
    if (x := v.get("next")) is not None:
        w.uint(2)
        w.str(x)
        n += 1
    w.close_map(head, n)


def _read_jobs_page(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                items0 = [JobsJob.read(r) for _ in range(r.array())]
                r.leave()
                read["jobs"] = items0
            case 2:
                read["next"] = r.str()
            case _:
                r.skip()
    r.leave()
    return read


JobsPage = Message(
    "jobs.Page",
    {
        "jobs": (1, list_(JobsJob)),
        "next": (2, str_),
    },
    _write_jobs_page,
    _read_jobs_page,
)


def _write_jobs_work(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("handle")) is not None:
        w.uint(1)
        w.uint(x)
        n += 1
    if (x := v.get("concurrency")) is not None:
        w.uint(2)
        w.uint(x)
        n += 1
    if (x := v.get("until_idle")) is not None:
        w.uint(3)
        w.bool(x)
        n += 1
    w.close_map(head, n)


def _read_jobs_work(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["handle"] = r.uint()
            case 2:
                read["concurrency"] = r.uint()
            case 3:
                read["until_idle"] = r.bool()
            case _:
                r.skip()
    r.leave()
    return read


# Starts the queue's worker for the client: the server claims its jobs as they
# fall due and hands each over as a held job, no more at once than the
# client's handlers, and the client answers each. A stop, or the server's
# GOAWAY, hands no job more, and the server ends the stream with DATA·END once
# the jobs the client holds are answered and written. The client's DATA·END
# ends the worker at once, the attempt of a job it still holds failing.
JobsWork = Message(
    "jobs.Work",
    {
        "handle": (1, uint),
        # the handlers the client runs at once, 1024 at most: the queue's total, or one
        "concurrency": (2, uint),
        # runDue: the server ends the stream once no job is due and none is held
        "until_idle": (3, bool_),
    },
    _write_jobs_work,
    _read_jobs_work,
)


def _write_jobs_held(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("run")) is not None:
        w.uint(1)
        w.uint(x)
        n += 1
    if (x := v.get("id")) is not None:
        w.uint(2)
        w.str(x)
        n += 1
    if (x := v.get("value")) is not None:
        w.uint(3)
        w.str(x)
        n += 1
    if (x := v.get("at")) is not None:
        w.uint(4)
        w.int(x)
        n += 1
    if (x := v.get("attempt")) is not None:
        w.uint(5)
        w.uint(x)
        n += 1
    if (x := v.get("group")) is not None:
        w.uint(6)
        w.str(x)
        n += 1
    if (x := v.get("cancelled")) is not None:
        w.uint(7)
        w.bool(x)
        n += 1
    w.close_map(head, n)


def _read_jobs_held(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["run"] = r.uint()
            case 2:
                read["id"] = r.str()
            case 3:
                read["value"] = r.str()
            case 4:
                read["at"] = r.int()
            case 5:
                read["attempt"] = r.uint()
            case 6:
                read["group"] = r.str()
            case 7:
                read["cancelled"] = r.bool()
            case _:
                r.skip()
    r.leave()
    return read


# A job the server hands the client's worker. Its run's number names it to the
# answer, a step and a keep; a cancel sends it again, cancelled, so that its
# handler stops.
JobsHeld = Message(
    "jobs.Held",
    {
        "run": (1, uint),
        "id": (2, str_),
        "value": (3, str_),
        # when the run was due
        "at": (4, int_),
        # the first being 1
        "attempt": (5, uint),
        "group": (6, str_),
        "cancelled": (7, bool_),
    },
    _write_jobs_held,
    _read_jobs_held,
)


def _write_jobs_answer(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("run")) is not None:
        w.uint(1)
        w.uint(x)
        n += 1
    if (x := v.get("how")) is not None:
        w.uint(2)
        w.str(x)
        n += 1
    if (x := v.get("at")) is not None:
        w.uint(3)
        w.int(x)
        n += 1
    if (x := v.get("error")) is not None:
        w.uint(4)
        w.str(x)
        n += 1
    if (x := v.get("progress")) is not None:
        w.uint(5)
        w.str(x)
        n += 1
    if (x := v.get("delay")) is not None:
        w.uint(6)
        w.uint(x)
        n += 1
    w.close_map(head, n)


def _read_jobs_answer(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["run"] = r.uint()
            case 2:
                read["how"] = r.str()
            case 3:
                read["at"] = r.int()
            case 4:
                read["error"] = r.str()
            case 5:
                read["progress"] = r.str()
            case 6:
                read["delay"] = r.uint()
            case _:
                r.skip()
    r.leave()
    return read


# The client's answer for a held job: how its run ended, or how far it got,
# which settles nothing; or a stop, which names no run. An answer for a job a
# cancel took settles nothing.
JobsAnswer = Message(
    "jobs.Answer",
    {
        "run": (1, uint),
        # done, retry, snooze, fail, back, progress or stop
        "how": (2, str_),
        # when a retry or a snooze runs again; a retry without one waits its backoff
        "at": (3, int_),
        # why a retry or a failure
        "error": (4, str_),
        # 4 KiB at most
        "progress": (5, str_),
        # from now on the server's clock; not beside at
        "delay": (6, uint),
    },
    _write_jobs_answer,
    _read_jobs_answer,
)


def _write_jobs_step(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("run")) is not None:
        w.uint(1)
        w.uint(x)
        n += 1
    if (x := v.get("name")) is not None:
        w.uint(2)
        w.str(x)
        n += 1
    if (x := v.get("answer")) is not None:
        w.uint(3)
        w.str(x)
        n += 1
    w.close_map(head, n)


def _read_jobs_step(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["run"] = r.uint()
            case 2:
                read["name"] = r.str()
            case 3:
                read["answer"] = r.str()
            case _:
                r.skip()
    r.leave()
    return read


# A step of a held job's run: jobs.step asks for its kept answer, jobs.keep
# keeps one.
JobsStep = Message(
    "jobs.Step",
    {
        "run": (1, uint),
        # 1 to 256 bytes
        "name": (2, str_),
        # jobs.keep's: 1 MiB at most
        "answer": (3, str_),
    },
    _write_jobs_step,
    _read_jobs_step,
)


def _write_jobs_kept(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("found")) is not None:
        w.uint(1)
        w.bool(x)
        n += 1
    if (x := v.get("answer")) is not None:
        w.uint(2)
        w.str(x)
        n += 1
    w.close_map(head, n)


def _read_jobs_kept(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["found"] = r.bool()
            case 2:
                read["answer"] = r.str()
            case _:
                r.skip()
    r.leave()
    return read


JobsKept = Message(
    "jobs.Kept",
    {
        "found": (1, bool_),
        "answer": (2, str_),
    },
    _write_jobs_kept,
    _read_jobs_kept,
)


def _write_jobs_op(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("method")) is not None:
        w.uint(1)
        w.uint(x)
        n += 1
    if (x := v.get("call")) is not None:
        w.uint(2)
        JobsCall.write(w, x)
        n += 1
    if (x := v.get("id")) is not None:
        w.uint(3)
        JobsId.write(w, x)
        n += 1
    w.close_map(head, n)


def _read_jobs_op(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["method"] = r.uint()
            case 2:
                read["call"] = JobsCall.read(r)
            case 3:
                read["id"] = JobsId.read(r)
            case _:
                r.skip()
    r.leave()
    return read


# One write of a transaction: an add, a set or an update with its call, or a
# cancel with its id.
JobsOp = Message(
    "jobs.Op",
    {
        # jobs.add, jobs.set, jobs.update or jobs.cancel
        "method": (1, uint),
        "call": (2, JobsCall),
        "id": (3, JobsId),
    },
    _write_jobs_op,
    _read_jobs_op,
)


def _write_jobs_tx(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("writes")) is not None:
        w.uint(1)
        w.array(len(x))
        for item0 in x:
            JobsOp.write(w, item0)
        n += 1
    w.close_map(head, n)


def _read_jobs_tx(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                items0 = [JobsOp.read(r) for _ in range(r.array())]
                r.leave()
                read["writes"] = items0
            case _:
                r.skip()
    r.leave()
    return read


# A transaction of jobs.db across the wire: its writes, all applied or none.
# One that fails names its place in what, as write.
JobsTx = Message(
    "jobs.Tx",
    {
        "writes": (1, list_(JobsOp)),
    },
    _write_jobs_tx,
    _read_jobs_tx,
)


def _write_jobs_tx_results(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("outcomes")) is not None:
        w.uint(1)
        w.array(len(x))
        for item0 in x:
            JobsChanged.write(w, item0)
        n += 1
    w.close_map(head, n)


def _read_jobs_tx_results(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                items0 = [JobsChanged.read(r) for _ in range(r.array())]
                r.leave()
                read["outcomes"] = items0
            case _:
                r.skip()
    r.leave()
    return read


# What each write of a transaction did, in their order: whether an add added,
# an update changed or a cancel found a job; a set changes always.
JobsTxResults = Message(
    "jobs.TxResults",
    {
        "outcomes": (1, list_(JobsChanged)),
    },
    _write_jobs_tx_results,
    _read_jobs_tx_results,
)


def _write_kv_bucket_open(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("name")) is not None:
        w.uint(1)
        w.str(x)
        n += 1
    if (x := v.get("ttl")) is not None:
        w.uint(2)
        w.uint(x)
        n += 1
    if (x := v.get("idle")) is not None:
        w.uint(3)
        w.uint(x)
        n += 1
    if (x := v.get("database")) is not None:
        w.uint(4)
        w.uint(x)
        n += 1
    if (x := v.get("encrypted")) is not None:
        w.uint(5)
        w.bool(x)
        n += 1
    w.close_map(head, n)


def _read_kv_bucket_open(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["name"] = r.str()
            case 2:
                read["ttl"] = r.uint()
            case 3:
                read["idle"] = r.uint()
            case 4:
                read["database"] = r.uint()
            case 5:
                read["encrypted"] = r.bool()
            case _:
                r.skip()
    r.leave()
    return read


# Opens a bucket of values by key. Its keys expire ttl after they are written,
# or idle after they were last read or written; not both.
KvBucketOpen = Message(
    "kv.BucketOpen",
    {
        # [a-z0-9][a-z0-9_-]{0,63}
        "name": (1, str_),
        "ttl": (2, uint),
        "idle": (3, uint),
        # a database's handle: the bucket lives in its file; kv.db when absent
        "database": (4, uint),
        # its values are sealed with the server's encryption key; a read connection opens none
        "encrypted": (5, bool_),
    },
    _write_kv_bucket_open,
    _read_kv_bucket_open,
)


def _write_kv_call(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("handle")) is not None:
        w.uint(1)
        w.uint(x)
        n += 1
    if (x := v.get("under")) is not None:
        w.uint(2)
        w.array(len(x))
        for item0 in x:
            key.write(w, item0)
        n += 1
    if (x := v.get("key")) is not None:
        w.uint(3)
        key.write(w, x)
        n += 1
    if (x := v.get("value")) is not None:
        w.uint(4)
        kv_value.write(w, x)
        n += 1
    if (x := v.get("ttl")) is not None:
        w.uint(5)
        w.uint(x)
        n += 1
    if (x := v.get("expires_at")) is not None:
        w.uint(6)
        w.int(x)
        n += 1
    if (x := v.get("if_version")) is not None:
        w.uint(7)
        w.bin(x)
        n += 1
    if (x := v.get("n")) is not None:
        w.uint(8)
        w.int(x)
        n += 1
    w.close_map(head, n)


def _read_kv_call(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["handle"] = r.uint()
            case 2:
                items0 = [key.read(r) for _ in range(r.array())]
                r.leave()
                read["under"] = items0
            case 3:
                read["key"] = key.read(r)
            case 4:
                read["value"] = kv_value.read(r)
            case 5:
                read["ttl"] = r.uint()
            case 6:
                read["expires_at"] = r.int()
            case 7:
                read["if_version"] = r.bin()
            case 8:
                read["n"] = r.int()
            case _:
                r.skip()
    r.leave()
    return read


# A call on one key of a handle's branch.
KvCall = Message(
    "kv.Call",
    {
        "handle": (1, uint),
        # the branch's owners, outermost first; the bucket's root when empty
        "under": (2, list_(key)),
        "key": (3, key),
        # what a set or a create writes
        "value": (4, kv_value),
        # the expiry a write gives, from now
        "ttl": (5, uint),
        # the expiry a write gives, as a time
        "expires_at": (6, int_),
        # the version the key must still be at
        "if_version": (7, bin_),
        # what a counter adds; the requests or uses a limit is asked for, 1 when absent
        "n": (8, int_),
    },
    _write_kv_call,
    _read_kv_call,
)


def _write_kv_entry(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("found")) is not None:
        w.uint(1)
        w.bool(x)
        n += 1
    if (x := v.get("value")) is not None:
        w.uint(2)
        kv_value.write(w, x)
        n += 1
    if (x := v.get("version")) is not None:
        w.uint(3)
        w.bin(x)
        n += 1
    if (x := v.get("expires_at")) is not None:
        w.uint(4)
        w.int(x)
        n += 1
    if (x := v.get("key")) is not None:
        w.uint(5)
        key.write(w, x)
        n += 1
    w.close_map(head, n)


def _read_kv_entry(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["found"] = r.bool()
            case 2:
                read["value"] = kv_value.read(r)
            case 3:
                read["version"] = r.bin()
            case 4:
                read["expires_at"] = r.int()
            case 5:
                read["key"] = key.read(r)
            case _:
                r.skip()
    r.leave()
    return read


# A key's value with what a conditional write needs.
KvEntry = Message(
    "kv.Entry",
    {
        "found": (1, bool_),
        "value": (2, kv_value),
        # compared only for equality
        "version": (3, bin_),
        # absent for a key that never expires
        "expires_at": (4, int_),
        # a page's entry alone
        "key": (5, key),
    },
    _write_kv_entry,
    _read_kv_entry,
)


def _write_kv_written(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("written")) is not None:
        w.uint(1)
        w.bool(x)
        n += 1
    if (x := v.get("version")) is not None:
        w.uint(2)
        w.bin(x)
        n += 1
    if (x := v.get("expires_at")) is not None:
        w.uint(3)
        w.int(x)
        n += 1
    w.close_map(head, n)


def _read_kv_written(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["written"] = r.bool()
            case 2:
                read["version"] = r.bin()
            case 3:
                read["expires_at"] = r.int()
            case _:
                r.skip()
    r.leave()
    return read


# What a write left: whether it wrote, and the version and expiry the key has
# now, the live key's own when a create found one.
KvWritten = Message(
    "kv.Written",
    {
        "written": (1, bool_),
        "version": (2, bin_),
        "expires_at": (3, int_),
    },
    _write_kv_written,
    _read_kv_written,
)


def _write_kv_found(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("found")) is not None:
        w.uint(1)
        w.bool(x)
        n += 1
    w.close_map(head, n)


def _read_kv_found(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["found"] = r.bool()
            case _:
                r.skip()
    r.leave()
    return read


KvFound = Message(
    "kv.Found",
    {
        "found": (1, bool_),
    },
    _write_kv_found,
    _read_kv_found,
)


def _write_kv_resealed(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("count")) is not None:
        w.uint(1)
        w.uint(x)
        n += 1
    w.close_map(head, n)


def _read_kv_resealed(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["count"] = r.uint()
            case _:
                r.skip()
    r.leave()
    return read


# How many values a reseal sealed again with the sealing key.
KvResealed = Message(
    "kv.Resealed",
    {
        "count": (1, uint),
    },
    _write_kv_resealed,
    _read_kv_resealed,
)


def _write_kv_branch(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("handle")) is not None:
        w.uint(1)
        w.uint(x)
        n += 1
    if (x := v.get("under")) is not None:
        w.uint(2)
        w.array(len(x))
        for item0 in x:
            key.write(w, item0)
        n += 1
    w.close_map(head, n)


def _read_kv_branch(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["handle"] = r.uint()
            case 2:
                items0 = [key.read(r) for _ in range(r.array())]
                r.leave()
                read["under"] = items0
            case _:
                r.skip()
    r.leave()
    return read


# A branch of a handle, for a clear.
KvBranch = Message(
    "kv.Branch",
    {
        "handle": (1, uint),
        "under": (2, list_(key)),
    },
    _write_kv_branch,
    _read_kv_branch,
)


def _write_kv_list(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("handle")) is not None:
        w.uint(1)
        w.uint(x)
        n += 1
    if (x := v.get("under")) is not None:
        w.uint(2)
        w.array(len(x))
        for item0 in x:
            key.write(w, item0)
        n += 1
    if (x := v.get("after")) is not None:
        w.uint(3)
        key.write(w, x)
        n += 1
    if (x := v.get("limit")) is not None:
        w.uint(4)
        w.uint(x)
        n += 1
    w.close_map(head, n)


def _read_kv_list(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["handle"] = r.uint()
            case 2:
                items0 = [key.read(r) for _ in range(r.array())]
                r.leave()
                read["under"] = items0
            case 3:
                read["after"] = key.read(r)
            case 4:
                read["limit"] = r.uint()
            case _:
                r.skip()
    r.leave()
    return read


# A page of a branch's own keys, in the byte order of their text.
KvList = Message(
    "kv.List",
    {
        "handle": (1, uint),
        "under": (2, list_(key)),
        # the key the page starts after
        "after": (3, key),
        # 100 when absent, 1000 at most
        "limit": (4, uint),
    },
    _write_kv_list,
    _read_kv_list,
)


def _write_kv_page(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("entries")) is not None:
        w.uint(1)
        w.array(len(x))
        for item0 in x:
            KvEntry.write(w, item0)
        n += 1
    if (x := v.get("next")) is not None:
        w.uint(2)
        key.write(w, x)
        n += 1
    w.close_map(head, n)


def _read_kv_page(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                items0 = [KvEntry.read(r) for _ in range(r.array())]
                r.leave()
                read["entries"] = items0
            case 2:
                read["next"] = key.read(r)
            case _:
                r.skip()
    r.leave()
    return read


# A page of entries, as many as the limit asks and the body holds, and where
# the next page starts, when there is one.
KvPage = Message(
    "kv.Page",
    {
        "entries": (1, list_(KvEntry)),
        "next": (2, key),
    },
    _write_kv_page,
    _read_kv_page,
)


def _write_kv_counters_open(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("name")) is not None:
        w.uint(1)
        w.str(x)
        n += 1
    if (x := v.get("ttl")) is not None:
        w.uint(2)
        w.uint(x)
        n += 1
    if (x := v.get("flush_every")) is not None:
        w.uint(3)
        w.uint(x)
        n += 1
    w.close_map(head, n)


def _read_kv_counters_open(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["name"] = r.str()
            case 2:
                read["ttl"] = r.uint()
            case 3:
                read["flush_every"] = r.uint()
            case _:
                r.skip()
    r.leave()
    return read


# Opens counters: numbers by key that only add up.
KvCountersOpen = Message(
    "kv.CountersOpen",
    {
        "name": (1, str_),
        # a counter lasts this long from its first add
        "ttl": (2, uint),
        # kept in memory and written every span; each add written when absent
        "flush_every": (3, uint),
    },
    _write_kv_counters_open,
    _read_kv_counters_open,
)


def _write_kv_count(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("value")) is not None:
        w.uint(1)
        w.int(x)
        n += 1
    w.close_map(head, n)


def _read_kv_count(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["value"] = r.int()
            case _:
                r.skip()
    r.leave()
    return read


KvCount = Message(
    "kv.Count",
    {
        "value": (1, int_),
    },
    _write_kv_count,
    _read_kv_count,
)


def _write_kv_rate_limit_open(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("name")) is not None:
        w.uint(1)
        w.str(x)
        n += 1
    if (x := v.get("rate")) is not None:
        w.uint(2)
        w.uint(x)
        n += 1
    if (x := v.get("per")) is not None:
        w.uint(3)
        w.uint(x)
        n += 1
    if (x := v.get("burst")) is not None:
        w.uint(4)
        w.uint(x)
        n += 1
    w.close_map(head, n)


def _read_kv_rate_limit_open(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["name"] = r.str()
            case 2:
                read["rate"] = r.uint()
            case 3:
                read["per"] = r.uint()
            case 4:
                read["burst"] = r.uint()
            case _:
                r.skip()
    r.leave()
    return read


# Opens a rate limit: rate requests a key every per, burst at once.
KvRateLimitOpen = Message(
    "kv.RateLimitOpen",
    {
        "name": (1, str_),
        "rate": (2, uint),
        "per": (3, uint),
        # the rate when absent
        "burst": (4, uint),
    },
    _write_kv_rate_limit_open,
    _read_kv_rate_limit_open,
)


def _write_kv_window(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("name")) is not None:
        w.uint(1)
        w.str(x)
        n += 1
    if (x := v.get("limit")) is not None:
        w.uint(2)
        w.uint(x)
        n += 1
    if (x := v.get("per")) is not None:
        w.uint(3)
        w.uint(x)
        n += 1
    w.close_map(head, n)


def _read_kv_window(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["name"] = r.str()
            case 2:
                read["limit"] = r.uint()
            case 3:
                read["per"] = r.uint()
            case _:
                r.skip()
    r.leave()
    return read


# One window of a quota: a key may use up to limit every per from its first use.
KvWindow = Message(
    "kv.Window",
    {
        # [a-z][a-z0-9_]{0,31}
        "name": (1, str_),
        "limit": (2, uint),
        "per": (3, uint),
    },
    _write_kv_window,
    _read_kv_window,
)


def _write_kv_quota_open(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("name")) is not None:
        w.uint(1)
        w.str(x)
        n += 1
    if (x := v.get("windows")) is not None:
        w.uint(2)
        w.array(len(x))
        for item0 in x:
            KvWindow.write(w, item0)
        n += 1
    w.close_map(head, n)


def _read_kv_quota_open(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["name"] = r.str()
            case 2:
                items0 = [KvWindow.read(r) for _ in range(r.array())]
                r.leave()
                read["windows"] = items0
            case _:
                r.skip()
    r.leave()
    return read


KvQuotaOpen = Message(
    "kv.QuotaOpen",
    {
        "name": (1, str_),
        # one to eight
        "windows": (2, list_(KvWindow)),
    },
    _write_kv_quota_open,
    _read_kv_quota_open,
)


def _write_kv_window_use(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("name")) is not None:
        w.uint(1)
        w.str(x)
        n += 1
    if (x := v.get("used")) is not None:
        w.uint(2)
        w.uint(x)
        n += 1
    if (x := v.get("limit")) is not None:
        w.uint(3)
        w.uint(x)
        n += 1
    if (x := v.get("left")) is not None:
        w.uint(4)
        w.uint(x)
        n += 1
    if (x := v.get("resets_at")) is not None:
        w.uint(5)
        w.int(x)
        n += 1
    w.close_map(head, n)


def _read_kv_window_use(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["name"] = r.str()
            case 2:
                read["used"] = r.uint()
            case 3:
                read["limit"] = r.uint()
            case 4:
                read["left"] = r.uint()
            case 5:
                read["resets_at"] = r.int()
            case _:
                r.skip()
    r.leave()
    return read


# One window of a quota as a key stands in it.
KvWindowUse = Message(
    "kv.WindowUse",
    {
        "name": (1, str_),
        "used": (2, uint),
        "limit": (3, uint),
        "left": (4, uint),
        # absent for a window not started
        "resets_at": (5, int_),
    },
    _write_kv_window_use,
    _read_kv_window_use,
)


def _write_kv_allowance(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("ok")) is not None:
        w.uint(1)
        w.bool(x)
        n += 1
    if (x := v.get("left")) is not None:
        w.uint(2)
        w.uint(x)
        n += 1
    if (x := v.get("retry_at")) is not None:
        w.uint(3)
        w.int(x)
        n += 1
    if (x := v.get("windows")) is not None:
        w.uint(4)
        w.array(len(x))
        for item0 in x:
            KvWindowUse.write(w, item0)
        n += 1
    w.close_map(head, n)


def _read_kv_allowance(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["ok"] = r.bool()
            case 2:
                read["left"] = r.uint()
            case 3:
                read["retry_at"] = r.int()
            case 4:
                items0 = [KvWindowUse.read(r) for _ in range(r.array())]
                r.leave()
                read["windows"] = items0
            case _:
                r.skip()
    r.leave()
    return read


# What a rate limit or a quota answers a request.
KvAllowance = Message(
    "kv.Allowance",
    {
        "ok": (1, bool_),
        # how many more would pass now
        "left": (2, uint),
        # when the request would pass; absent when it did
        "retry_at": (3, int_),
        # a quota's, in the order it names them
        "windows": (4, list_(KvWindowUse)),
    },
    _write_kv_allowance,
    _read_kv_allowance,
)


def _write_kv_once_open(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("name")) is not None:
        w.uint(1)
        w.str(x)
        n += 1
    if (x := v.get("keep")) is not None:
        w.uint(2)
        w.uint(x)
        n += 1
    w.close_map(head, n)


def _read_kv_once_open(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["name"] = r.str()
            case 2:
                read["keep"] = r.uint()
            case _:
                r.skip()
    r.leave()
    return read


# Opens once's answers: a function run once a key, its answer kept.
KvOnceOpen = Message(
    "kv.OnceOpen",
    {
        "name": (1, str_),
        # a day when absent
        "keep": (2, uint),
    },
    _write_kv_once_open,
    _read_kv_once_open,
)


def _write_kv_answer(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("found")) is not None:
        w.uint(1)
        w.bool(x)
        n += 1
    if (x := v.get("value")) is not None:
        w.uint(2)
        kv_value.write(w, x)
        n += 1
    w.close_map(head, n)


def _read_kv_answer(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["found"] = r.bool()
            case 2:
                read["value"] = kv_value.read(r)
            case _:
                r.skip()
    r.leave()
    return read


# An answer of once: in the server's RESPONSE, found when one was kept, and
# otherwise the run is the client's; in the client's last DATA, found with the
# answer to keep, or not found when the function failed.
KvAnswer = Message(
    "kv.Answer",
    {
        "found": (1, bool_),
        "value": (2, kv_value),
    },
    _write_kv_answer,
    _read_kv_answer,
)


def _write_kv_check(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("handle")) is not None:
        w.uint(1)
        w.uint(x)
        n += 1
    if (x := v.get("under")) is not None:
        w.uint(2)
        w.array(len(x))
        for item0 in x:
            key.write(w, item0)
        n += 1
    if (x := v.get("key")) is not None:
        w.uint(3)
        key.write(w, x)
        n += 1
    if (x := v.get("version")) is not None:
        w.uint(4)
        w.bin(x)
        n += 1
    w.close_map(head, n)


def _read_kv_check(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["handle"] = r.uint()
            case 2:
                items0 = [key.read(r) for _ in range(r.array())]
                r.leave()
                read["under"] = items0
            case 3:
                read["key"] = key.read(r)
            case 4:
                read["version"] = r.bin()
            case _:
                r.skip()
    r.leave()
    return read


# A read a transaction made, which its commit checks: the key still at the
# version it was found at, or still absent.
KvCheck = Message(
    "kv.Check",
    {
        "handle": (1, uint),
        "under": (2, list_(key)),
        "key": (3, key),
        # absent: the read found nothing
        "version": (4, bin_),
    },
    _write_kv_check,
    _read_kv_check,
)


def _write_kv_op(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("method")) is not None:
        w.uint(1)
        w.uint(x)
        n += 1
    if (x := v.get("call")) is not None:
        w.uint(2)
        KvCall.write(w, x)
        n += 1
    w.close_map(head, n)


def _read_kv_op(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["method"] = r.uint()
            case 2:
                read["call"] = KvCall.read(r)
            case _:
                r.skip()
    r.leave()
    return read


# One write of a transaction: the method it is, and its call.
KvOp = Message(
    "kv.Op",
    {
        # kv.set, kv.create, kv.take, kv.delete, kv.expire, kv.clear or kv.counters.add
        "method": (1, uint),
        "call": (2, KvCall),
    },
    _write_kv_op,
    _read_kv_op,
)


def _write_kv_tx(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("checks")) is not None:
        w.uint(1)
        w.array(len(x))
        for item0 in x:
            KvCheck.write(w, item0)
        n += 1
    if (x := v.get("writes")) is not None:
        w.uint(2)
        w.array(len(x))
        for item0 in x:
            KvOp.write(w, item0)
        n += 1
    w.close_map(head, n)


def _read_kv_tx(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                items0 = [KvCheck.read(r) for _ in range(r.array())]
                r.leave()
                read["checks"] = items0
            case 2:
                items0 = [KvOp.read(r) for _ in range(r.array())]
                r.leave()
                read["writes"] = items0
            case _:
                r.skip()
    r.leave()
    return read


# A transaction across the wire: its reads' checks, then its writes, all
# applied or none. A failed check or write names its place in what.
KvTx = Message(
    "kv.Tx",
    {
        "checks": (1, list_(KvCheck)),
        "writes": (2, list_(KvOp)),
    },
    _write_kv_tx,
    _read_kv_tx,
)


def _write_kv_outcome(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("found")) is not None:
        w.uint(1)
        w.bool(x)
        n += 1
    if (x := v.get("written")) is not None:
        w.uint(2)
        w.bool(x)
        n += 1
    if (x := v.get("value")) is not None:
        w.uint(3)
        kv_value.write(w, x)
        n += 1
    if (x := v.get("version")) is not None:
        w.uint(4)
        w.bin(x)
        n += 1
    if (x := v.get("expires_at")) is not None:
        w.uint(5)
        w.int(x)
        n += 1
    if (x := v.get("count")) is not None:
        w.uint(6)
        w.int(x)
        n += 1
    w.close_map(head, n)


def _read_kv_outcome(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["found"] = r.bool()
            case 2:
                read["written"] = r.bool()
            case 3:
                read["value"] = kv_value.read(r)
            case 4:
                read["version"] = r.bin()
            case 5:
                read["expires_at"] = r.int()
            case 6:
                read["count"] = r.int()
            case _:
                r.skip()
    r.leave()
    return read


# What one write of a transaction did.
KvOutcome = Message(
    "kv.Outcome",
    {
        # a take, a delete or an expire found a live key
        "found": (1, bool_),
        # a set or a create wrote
        "written": (2, bool_),
        # what a take took
        "value": (3, kv_value),
        "version": (4, bin_),
        "expires_at": (5, int_),
        # a counter's value after its add
        "count": (6, int_),
    },
    _write_kv_outcome,
    _read_kv_outcome,
)


def _write_kv_tx_results(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("outcomes")) is not None:
        w.uint(1)
        w.array(len(x))
        for item0 in x:
            KvOutcome.write(w, item0)
        n += 1
    w.close_map(head, n)


def _read_kv_tx_results(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                items0 = [KvOutcome.read(r) for _ in range(r.array())]
                r.leave()
                read["outcomes"] = items0
            case _:
                r.skip()
    r.leave()
    return read


KvTxResults = Message(
    "kv.TxResults",
    {
        "outcomes": (1, list_(KvOutcome)),
    },
    _write_kv_tx_results,
    _read_kv_tx_results,
)


def _write_locks_hold(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("kind")) is not None:
        w.uint(1)
        w.str(x)
        n += 1
    if (x := v.get("name")) is not None:
        w.uint(2)
        w.str(x)
        n += 1
    if (x := v.get("key")) is not None:
        w.uint(3)
        w.str(x)
        n += 1
    if (x := v.get("limit")) is not None:
        w.uint(4)
        w.uint(x)
        n += 1
    if (x := v.get("ttl")) is not None:
        w.uint(5)
        w.uint(x)
        n += 1
    if (x := v.get("max_hold")) is not None:
        w.uint(6)
        w.uint(x)
        n += 1
    if (x := v.get("wait")) is not None:
        w.uint(7)
        w.uint(x)
        n += 1
    if (x := v.get("request")) is not None:
        w.uint(8)
        w.bin(x)
        n += 1
    w.close_map(head, n)


def _read_locks_hold(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["kind"] = r.str()
            case 2:
                read["name"] = r.str()
            case 3:
                read["key"] = r.str()
            case 4:
                read["limit"] = r.uint()
            case 5:
                read["ttl"] = r.uint()
            case 6:
                read["max_hold"] = r.uint()
            case 7:
                read["wait"] = r.uint()
            case 8:
                read["request"] = r.bin()
            case _:
                r.skip()
    r.leave()
    return read


# A key of a lock or a semaphore that a holder through a connection asks
# for, as docs/guides/locks/README.md has it: the lease lasts `ttl` from its grant and
# from each renewal, so that a holder whose program froze loses it.
LocksHold = Message(
    "locks.Hold",
    {
        # lock or semaphore
        "kind": (1, str_),
        # [a-z0-9][a-z0-9_-]{0,63}
        "name": (2, str_),
        # 1 to 1024 bytes
        "key": (3, str_),
        # how many may hold the key at once: 1 for a lock, 1 to 1000
        "limit": (4, uint),
        # 1 s to 1 hour
        "ttl": (5, uint),
        # from the grant, however the lease is renewed
        "max_hold": (6, uint),
        # 0 takes the key only if it is free
        "wait": (7, uint),
        # 16 random bytes: asked again, the lease given answers
        "request": (8, bin_),
    },
    _write_locks_hold,
    _read_locks_hold,
)


def _write_locks_held(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("acquired")) is not None:
        w.uint(1)
        w.bool(x)
        n += 1
    if (x := v.get("lease")) is not None:
        w.uint(2)
        w.bin(x)
        n += 1
    if (x := v.get("fence")) is not None:
        w.uint(3)
        w.str(x)
        n += 1
    if (x := v.get("revoke_in")) is not None:
        w.uint(4)
        w.uint(x)
        n += 1
    w.close_map(head, n)


def _read_locks_held(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["acquired"] = r.bool()
            case 2:
                read["lease"] = r.bin()
            case 3:
                read["fence"] = r.str()
            case 4:
                read["revoke_in"] = r.uint()
            case _:
                r.skip()
    r.leave()
    return read


# A lease, or the key held by others when the hold would not wait.
LocksHeld = Message(
    "locks.Held",
    {
        "acquired": (1, bool_),
        # 16 random bytes only its holder learns
        "lease": (2, bin_),
        # a decimal integer of up to 128 bits; empty for a semaphore
        "fence": (3, str_),
        # how long until maxHold revokes the lease, from now
        "revoke_in": (4, uint),
    },
    _write_locks_held,
    _read_locks_held,
)


def _write_locks_renew(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("lease")) is not None:
        w.uint(1)
        w.bin(x)
        n += 1
    if (x := v.get("ttl")) is not None:
        w.uint(2)
        w.uint(x)
        n += 1
    w.close_map(head, n)


def _read_locks_renew(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["lease"] = r.bin()
            case 2:
                read["ttl"] = r.uint()
            case _:
                r.skip()
    r.leave()
    return read


LocksRenew = Message(
    "locks.Renew",
    {
        "lease": (1, bin_),
        # how long the lease lasts from now
        "ttl": (2, uint),
    },
    _write_locks_renew,
    _read_locks_renew,
)


def _write_locks_lease(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("lease")) is not None:
        w.uint(1)
        w.bin(x)
        n += 1
    w.close_map(head, n)


def _read_locks_lease(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["lease"] = r.bin()
            case _:
                r.skip()
    r.leave()
    return read


LocksLease = Message(
    "locks.Lease",
    {
        "lease": (1, bin_),
    },
    _write_locks_lease,
    _read_locks_lease,
)


def _write_metrics_condition(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("values")) is not None:
        w.uint(1)
        w.array(len(x))
        for item0 in x:
            w.str(item0)
        n += 1
    if (x := v.get("not")) is not None:
        w.uint(2)
        w.bool(x)
        n += 1
    if (x := v.get("prefix")) is not None:
        w.uint(3)
        w.str(x)
        n += 1
    w.close_map(head, n)


def _read_metrics_condition(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                items0 = [r.str() for _ in range(r.array())]
                r.leave()
                read["values"] = items0
            case 2:
                read["not"] = r.bool()
            case 3:
                read["prefix"] = r.str()
            case _:
                r.skip()
    r.leave()
    return read


# What a label's value must be: one of values, none of them with not, the
# label absent too, or a value that starts with prefix.
MetricsCondition = Message(
    "metrics.Condition",
    {
        "values": (1, list_(str_)),
        "not": (2, bool_),
        "prefix": (3, str_),
    },
    _write_metrics_condition,
    _read_metrics_condition,
)


def _write_metrics_query(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("name")) is not None:
        w.uint(1)
        w.str(x)
        n += 1
    if (x := v.get("where")) is not None:
        w.uint(2)
        w.map(len(x))
        for name0 in sorted(x):
            w.str(name0)
            MetricsCondition.write(w, x[name0])
        n += 1
    if (x := v.get("since")) is not None:
        w.uint(3)
        w.uint(x)
        n += 1
    if (x := v.get("from")) is not None:
        w.uint(4)
        w.int(x)
        n += 1
    if (x := v.get("to")) is not None:
        w.uint(5)
        w.int(x)
        n += 1
    if (x := v.get("limit")) is not None:
        w.uint(6)
        w.uint(x)
        n += 1
    if (x := v.get("after")) is not None:
        w.uint(7)
        w.str(x)
        n += 1
    w.close_map(head, n)


def _read_metrics_query(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["name"] = r.str()
            case 2:
                named0: dict[str, Any] = {}
                names0: set[str] = set()
                for _ in range(r.map()):
                    name0 = r.name(names0)
                    named0[name0] = MetricsCondition.read(r)
                r.leave()
                read["where"] = named0
            case 3:
                read["since"] = r.uint()
            case 4:
                read["from"] = r.int()
            case 5:
                read["to"] = r.int()
            case 6:
                read["limit"] = r.uint()
            case 7:
                read["after"] = r.str()
            case _:
                r.skip()
    r.leave()
    return read


# The series a call finds, and its range: since before now, or from and to.
MetricsQuery = Message(
    "metrics.Query",
    {
        "name": (1, str_),
        "where": (2, names(MetricsCondition)),
        "since": (3, uint),
        "from": (4, int_),
        # excluded; now, a sample taken now in the range, unless given
        "to": (5, int_),
        # a page's samples or buckets at most, 100,000 at most: a page, which read and aggregate take
        "limit": (6, uint),
        # a page's next, with limit
        "after": (7, str_),
    },
    _write_metrics_query,
    _read_metrics_query,
)


def _write_metrics_series(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("name")) is not None:
        w.uint(1)
        w.str(x)
        n += 1
    if (x := v.get("kind")) is not None:
        w.uint(2)
        w.str(x)
        n += 1
    if (x := v.get("labels")) is not None:
        w.uint(3)
        w.map(len(x))
        for name0 in sorted(x):
            w.str(name0)
            w.str(x[name0])
        n += 1
    if (x := v.get("times")) is not None:
        w.uint(4)
        w.bin(x)
        n += 1
    if (x := v.get("values")) is not None:
        w.uint(5)
        w.bin(x)
        n += 1
    if (x := v.get("more")) is not None:
        w.uint(6)
        w.bool(x)
        n += 1
    w.close_map(head, n)


def _read_metrics_series(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["name"] = r.str()
            case 2:
                read["kind"] = r.str()
            case 3:
                named0: dict[str, Any] = {}
                names0: set[str] = set()
                for _ in range(r.map()):
                    name0 = r.name(names0)
                    named0[name0] = r.str()
                r.leave()
                read["labels"] = named0
            case 4:
                read["times"] = r.bin()
            case 5:
                read["values"] = r.bin()
            case 6:
                read["more"] = r.bool()
            case _:
                r.skip()
    r.leave()
    return read


# A series and its samples, or a part of them: a series longer than a body
# goes on in the next part.
MetricsSeries = Message(
    "metrics.Series",
    {
        "name": (1, str_),
        # gauge, counter or histogram
        "kind": (2, str_),
        "labels": (3, names(str_)),
        # an int64 a sample
        "times": (4, bin_),
        # a float64 a sample
        "values": (5, bin_),
        # the next part goes on with this series; on a page's last, the next page's first
        "more": (6, bool_),
    },
    _write_metrics_series,
    _read_metrics_series,
)


def _write_metrics_read(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("series")) is not None:
        w.uint(1)
        w.array(len(x))
        for item0 in x:
            MetricsSeries.write(w, item0)
        n += 1
    if (x := v.get("next")) is not None:
        w.uint(2)
        w.str(x)
        n += 1
    w.close_map(head, n)


def _read_metrics_read(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                items0 = [MetricsSeries.read(r) for _ in range(r.array())]
                r.leave()
                read["series"] = items0
            case 2:
                read["next"] = r.str()
            case _:
                r.skip()
    r.leave()
    return read


MetricsRead = Message(
    "metrics.Read",
    {
        "series": (1, list_(MetricsSeries)),
        # on the last part of a page that has more, to a request that gave limit
        "next": (2, str_),
    },
    _write_metrics_read,
    _read_metrics_read,
)


def _write_metrics_newest(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("name")) is not None:
        w.uint(1)
        w.str(x)
        n += 1
    if (x := v.get("labels")) is not None:
        w.uint(2)
        w.map(len(x))
        for name0 in sorted(x):
            w.str(name0)
            w.str(x[name0])
        n += 1
    if (x := v.get("at")) is not None:
        w.uint(3)
        w.int(x)
        n += 1
    if (x := v.get("value")) is not None:
        w.uint(4)
        w.float(x)
        n += 1
    w.close_map(head, n)


def _read_metrics_newest(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["name"] = r.str()
            case 2:
                named0: dict[str, Any] = {}
                names0: set[str] = set()
                for _ in range(r.map()):
                    name0 = r.name(names0)
                    named0[name0] = r.str()
                r.leave()
                read["labels"] = named0
            case 3:
                read["at"] = r.int()
            case 4:
                read["value"] = r.float()
            case _:
                r.skip()
    r.leave()
    return read


# A series' newest sample in the range.
MetricsNewest = Message(
    "metrics.Newest",
    {
        "name": (1, str_),
        "labels": (2, names(str_)),
        "at": (3, int_),
        "value": (4, float_),
    },
    _write_metrics_newest,
    _read_metrics_newest,
)


def _write_metrics_latest(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("series")) is not None:
        w.uint(1)
        w.array(len(x))
        for item0 in x:
            MetricsNewest.write(w, item0)
        n += 1
    w.close_map(head, n)


def _read_metrics_latest(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                items0 = [MetricsNewest.read(r) for _ in range(r.array())]
                r.leave()
                read["series"] = items0
            case _:
                r.skip()
    r.leave()
    return read


MetricsLatest = Message(
    "metrics.Latest",
    {
        "series": (1, list_(MetricsNewest)),
    },
    _write_metrics_latest,
    _read_metrics_latest,
)


def _write_metrics_aggregate_asked(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("query")) is not None:
        w.uint(1)
        MetricsQuery.write(w, x)
        n += 1
    if (x := v.get("every")) is not None:
        w.uint(2)
        w.uint(x)
        n += 1
    if (x := v.get("op")) is not None:
        w.uint(3)
        w.str(x)
        n += 1
    if (x := v.get("by")) is not None:
        w.uint(4)
        w.array(len(x))
        for item0 in x:
            w.str(item0)
        n += 1
    if (x := v.get("without")) is not None:
        w.uint(5)
        w.array(len(x))
        for item0 in x:
            w.str(item0)
        n += 1
    if (x := v.get("estimate")) is not None:
        w.uint(6)
        w.bool(x)
        n += 1
    if (x := v.get("calendar")) is not None:
        w.uint(7)
        w.str(x)
        n += 1
    if (x := v.get("time_zone")) is not None:
        w.uint(8)
        w.str(x)
        n += 1
    w.close_map(head, n)


def _read_metrics_aggregate_asked(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["query"] = MetricsQuery.read(r)
            case 2:
                read["every"] = r.uint()
            case 3:
                read["op"] = r.str()
            case 4:
                items0 = [r.str() for _ in range(r.array())]
                r.leave()
                read["by"] = items0
            case 5:
                items0 = [r.str() for _ in range(r.array())]
                r.leave()
                read["without"] = items0
            case 6:
                read["estimate"] = r.bool()
            case 7:
                read["calendar"] = r.str()
            case 8:
                read["time_zone"] = r.str()
            case _:
                r.skip()
    r.leave()
    return read


# An aggregate: read's query, its buckets, its op and its groups. Without
# by and without, every series of a name is one group; without: [] keeps
# every series apart.
MetricsAggregateAsked = Message(
    "metrics.AggregateAsked",
    {
        "query": (1, MetricsQuery),
        "every": (2, uint),
        # count, sum, avg, min, max, increase, rate, delta, first, last, or p and a rank; none for a read's plan
        "op": (3, str_),
        "by": (4, list_(str_)),
        "without": (5, list_(str_)),
        # a timer's percentile also gives a point between its bounds
        "estimate": (6, bool_),
        # day, week or month: buckets of a calendar in timeZone, in place of every
        "calendar": (7, str_),
        # an IANA name, UTC among them, which calendar takes
        "time_zone": (8, str_),
    },
    _write_metrics_aggregate_asked,
    _read_metrics_aggregate_asked,
)


def _write_metrics_bucket(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("at")) is not None:
        w.uint(1)
        w.int(x)
        n += 1
    if (x := v.get("value")) is not None:
        w.uint(2)
        w.float(x)
        n += 1
    if (x := v.get("lower")) is not None:
        w.uint(3)
        w.float(x)
        n += 1
    if (x := v.get("upper")) is not None:
        w.uint(4)
        w.float(x)
        n += 1
    if (x := v.get("estimate")) is not None:
        w.uint(5)
        w.float(x)
        n += 1
    if (x := v.get("samples")) is not None:
        w.uint(6)
        w.uint(x)
        n += 1
    if (x := v.get("resets")) is not None:
        w.uint(7)
        w.uint(x)
        n += 1
    if (x := v.get("partial")) is not None:
        w.uint(8)
        w.bool(x)
        n += 1
    if (x := v.get("gap")) is not None:
        w.uint(9)
        w.bool(x)
        n += 1
    w.close_map(head, n)


def _read_metrics_bucket(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["at"] = r.int()
            case 2:
                read["value"] = r.float()
            case 3:
                read["lower"] = r.float()
            case 4:
                read["upper"] = r.float()
            case 5:
                read["estimate"] = r.float()
            case 6:
                read["samples"] = r.uint()
            case 7:
                read["resets"] = r.uint()
            case 8:
                read["partial"] = r.bool()
            case 9:
                read["gap"] = r.bool()
            case _:
                r.skip()
    r.leave()
    return read


# A bucket of an aggregate: its value, or a timer's percentile's bounds.
MetricsBucket = Message(
    "metrics.Bucket",
    {
        # where it begins
        "at": (1, int_),
        # none for a timer's percentile
        "value": (2, float_),
        "lower": (3, float_),
        # infinity past the last bound
        "upper": (4, float_),
        "estimate": (5, float_),
        "samples": (6, uint),
        "resets": (7, uint),
        # its value comes from less than its span
        "partial": (8, bool_),
        # its first step began before the bucket before it
        "gap": (9, bool_),
    },
    _write_metrics_bucket,
    _read_metrics_bucket,
)


def _write_metrics_group(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("name")) is not None:
        w.uint(1)
        w.str(x)
        n += 1
    if (x := v.get("labels")) is not None:
        w.uint(2)
        w.map(len(x))
        for name0 in sorted(x):
            w.str(name0)
            w.str(x[name0])
        n += 1
    if (x := v.get("buckets")) is not None:
        w.uint(3)
        w.array(len(x))
        for item0 in x:
            MetricsBucket.write(w, item0)
        n += 1
    if (x := v.get("more")) is not None:
        w.uint(4)
        w.bool(x)
        n += 1
    w.close_map(head, n)


def _read_metrics_group(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["name"] = r.str()
            case 2:
                named0: dict[str, Any] = {}
                names0: set[str] = set()
                for _ in range(r.map()):
                    name0 = r.name(names0)
                    named0[name0] = r.str()
                r.leave()
                read["labels"] = named0
            case 3:
                items0 = [MetricsBucket.read(r) for _ in range(r.array())]
                r.leave()
                read["buckets"] = items0
            case 4:
                read["more"] = r.bool()
            case _:
                r.skip()
    r.leave()
    return read


# A group's buckets, or a part of them: a group longer than a body goes on
# in the next part.
MetricsGroup = Message(
    "metrics.Group",
    {
        "name": (1, str_),
        "labels": (2, names(str_)),
        "buckets": (3, list_(MetricsBucket)),
        # the next part goes on with this group; on a page's last, the next page's first
        "more": (4, bool_),
    },
    _write_metrics_group,
    _read_metrics_group,
)


def _write_metrics_aggregates(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("groups")) is not None:
        w.uint(1)
        w.array(len(x))
        for item0 in x:
            MetricsGroup.write(w, item0)
        n += 1
    if (x := v.get("next")) is not None:
        w.uint(2)
        w.str(x)
        n += 1
    w.close_map(head, n)


def _read_metrics_aggregates(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                items0 = [MetricsGroup.read(r) for _ in range(r.array())]
                r.leave()
                read["groups"] = items0
            case 2:
                read["next"] = r.str()
            case _:
                r.skip()
    r.leave()
    return read


MetricsAggregates = Message(
    "metrics.Aggregates",
    {
        "groups": (1, list_(MetricsGroup)),
        # on the last part of a page that has more, to a request that gave limit
        "next": (2, str_),
    },
    _write_metrics_aggregates,
    _read_metrics_aggregates,
)


def _write_metrics_exceeds(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("limit")) is not None:
        w.uint(1)
        w.str(x)
        n += 1
    if (x := v.get("wanted")) is not None:
        w.uint(2)
        w.uint(x)
        n += 1
    if (x := v.get("bound")) is not None:
        w.uint(3)
        w.uint(x)
        n += 1
    w.close_map(head, n)


def _read_metrics_exceeds(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["limit"] = r.str()
            case 2:
                read["wanted"] = r.uint()
            case 3:
                read["bound"] = r.uint()
            case _:
                r.skip()
    r.leave()
    return read


MetricsExceeds = Message(
    "metrics.Exceeds",
    {
        # series, bytes read, samples decoded or answer
        "limit": (1, str_),
        "wanted": (2, uint),
        "bound": (3, uint),
    },
    _write_metrics_exceeds,
    _read_metrics_exceeds,
)


def _write_metrics_new_series(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("name")) is not None:
        w.uint(1)
        w.str(x)
        n += 1
    if (x := v.get("kind")) is not None:
        w.uint(2)
        w.str(x)
        n += 1
    if (x := v.get("labels")) is not None:
        w.uint(3)
        w.map(len(x))
        for name0 in sorted(x):
            w.str(name0)
            w.str(x[name0])
        n += 1
    if (x := v.get("times")) is not None:
        w.uint(4)
        w.bin(x)
        n += 1
    if (x := v.get("values")) is not None:
        w.uint(5)
        w.bin(x)
        n += 1
    if (x := v.get("keep")) is not None:
        w.uint(6)
        w.uint(x)
        n += 1
    w.close_map(head, n)


def _read_metrics_new_series(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["name"] = r.str()
            case 2:
                read["kind"] = r.str()
            case 3:
                named0: dict[str, Any] = {}
                names0: set[str] = set()
                for _ in range(r.map()):
                    name0 = r.name(names0)
                    named0[name0] = r.str()
                r.leave()
                read["labels"] = named0
            case 4:
                read["times"] = r.bin()
            case 5:
                read["values"] = r.bin()
            case 6:
                read["keep"] = r.uint()
            case _:
                r.skip()
    r.leave()
    return read


# A series to write, its samples as two columns.
MetricsNewSeries = Message(
    "metrics.NewSeries",
    {
        "name": (1, str_),
        # gauge, counter or histogram
        "kind": (2, str_),
        "labels": (3, names(str_)),
        # an int64 a sample
        "times": (4, bin_),
        # a float64 a sample
        "values": (5, bin_),
        # how long the name's samples stay: its own, kept in the file
        "keep": (6, uint),
    },
    _write_metrics_new_series,
    _read_metrics_new_series,
)


def _write_metrics_ingest(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("series")) is not None:
        w.uint(1)
        w.array(len(x))
        for item0 in x:
            MetricsNewSeries.write(w, item0)
        n += 1
    w.close_map(head, n)


def _read_metrics_ingest(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                items0 = [MetricsNewSeries.read(r) for _ in range(r.array())]
                r.leave()
                read["series"] = items0
            case _:
                r.skip()
    r.leave()
    return read


# Series to write, every one or none.
MetricsIngest = Message(
    "metrics.Ingest",
    {
        "series": (1, list_(MetricsNewSeries)),
    },
    _write_metrics_ingest,
    _read_metrics_ingest,
)


def _write_metrics_exposition(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("text")) is not None:
        w.uint(1)
        w.bin(x)
        n += 1
    if (x := v.get("labels")) is not None:
        w.uint(2)
        w.map(len(x))
        for name0 in sorted(x):
            w.str(name0)
            w.str(x[name0])
        n += 1
    if (x := v.get("at")) is not None:
        w.uint(3)
        w.int(x)
        n += 1
    if (x := v.get("keep")) is not None:
        w.uint(4)
        w.uint(x)
        n += 1
    w.close_map(head, n)


def _read_metrics_exposition(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["text"] = r.bin()
            case 2:
                named0: dict[str, Any] = {}
                names0: set[str] = set()
                for _ in range(r.map()):
                    name0 = r.name(names0)
                    named0[name0] = r.str()
                r.leave()
                read["labels"] = named0
            case 3:
                read["at"] = r.int()
            case 4:
                read["keep"] = r.uint()
            case _:
                r.skip()
    r.leave()
    return read


# A text an exporter serves, in Prometheus' format or OpenMetrics', whose
# series the store writes every one or none, each with labels beside its own.
MetricsExposition = Message(
    "metrics.Exposition",
    {
        # UTF-8, read a line at a time, so that an error names its line
        "text": (1, bin_),
        "labels": (2, names(str_)),
        # of a sample the text gives no time; now unless given
        "at": (3, int_),
        # how long each name of the text keeps its samples
        "keep": (4, uint),
    },
    _write_metrics_exposition,
    _read_metrics_exposition,
)


def _write_metrics_upload(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("size")) is not None:
        w.uint(1)
        w.uint(x)
        n += 1
    if (x := v.get("labels")) is not None:
        w.uint(2)
        w.map(len(x))
        for name0 in sorted(x):
            w.str(name0)
            w.str(x[name0])
        n += 1
    if (x := v.get("at")) is not None:
        w.uint(3)
        w.int(x)
        n += 1
    if (x := v.get("keep")) is not None:
        w.uint(4)
        w.uint(x)
        n += 1
    w.close_map(head, n)


def _read_metrics_upload(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["size"] = r.uint()
            case 2:
                named0: dict[str, Any] = {}
                names0: set[str] = set()
                for _ in range(r.map()):
                    name0 = r.name(names0)
                    named0[name0] = r.str()
                r.leave()
                read["labels"] = named0
            case 3:
                read["at"] = r.int()
            case 4:
                read["keep"] = r.uint()
            case _:
                r.skip()
    r.leave()
    return read


# A text past what one message holds: what metrics.Exposition carries but
# the text, whose bytes follow as the client's DATA, a metrics.Piece each.
MetricsUpload = Message(
    "metrics.Upload",
    {
        # the text's bytes, 32 MiB at most: pieces that come to more or fewer are refused
        "size": (1, uint),
        "labels": (2, names(str_)),
        "at": (3, int_),
        "keep": (4, uint),
    },
    _write_metrics_upload,
    _read_metrics_upload,
)


def _write_metrics_piece(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("bytes")) is not None:
        w.uint(1)
        w.bin(x)
        n += 1
    w.close_map(head, n)


def _read_metrics_piece(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["bytes"] = r.bin()
            case _:
                r.skip()
    r.leave()
    return read


# A piece of an uploaded text, cut anywhere.
MetricsPiece = Message(
    "metrics.Piece",
    {
        "bytes": (1, bin_),
    },
    _write_metrics_piece,
    _read_metrics_piece,
)


def _write_metrics_page_asked(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("name")) is not None:
        w.uint(1)
        w.str(x)
        n += 1
    if (x := v.get("label")) is not None:
        w.uint(2)
        w.str(x)
        n += 1
    if (x := v.get("prefix")) is not None:
        w.uint(3)
        w.str(x)
        n += 1
    if (x := v.get("after")) is not None:
        w.uint(4)
        w.str(x)
        n += 1
    if (x := v.get("limit")) is not None:
        w.uint(5)
        w.uint(x)
        n += 1
    w.close_map(head, n)


def _read_metrics_page_asked(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["name"] = r.str()
            case 2:
                read["label"] = r.str()
            case 3:
                read["prefix"] = r.str()
            case 4:
                read["after"] = r.str()
            case 5:
                read["limit"] = r.uint()
            case _:
                r.skip()
    r.leave()
    return read


# A page of names, of a name's labels or of a label's values.
MetricsPageAsked = Message(
    "metrics.PageAsked",
    {
        # the name whose labels, or whose label's values, it lists
        "name": (1, str_),
        # the label whose values it lists
        "label": (2, str_),
        "prefix": (3, str_),
        # a page's next
        "after": (4, str_),
        # 1000 unless given, 10,000 at most
        "limit": (5, uint),
    },
    _write_metrics_page_asked,
    _read_metrics_page_asked,
)


def _write_metrics_name(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("name")) is not None:
        w.uint(1)
        w.str(x)
        n += 1
    if (x := v.get("kind")) is not None:
        w.uint(2)
        w.str(x)
        n += 1
    if (x := v.get("series_count")) is not None:
        w.uint(3)
        w.uint(x)
        n += 1
    if (x := v.get("unit")) is not None:
        w.uint(4)
        w.str(x)
        n += 1
    if (x := v.get("help")) is not None:
        w.uint(5)
        w.str(x)
        n += 1
    if (x := v.get("keep")) is not None:
        w.uint(6)
        w.uint(x)
        n += 1
    if (x := v.get("retained_from")) is not None:
        w.uint(7)
        w.int(x)
        n += 1
    if (x := v.get("newest")) is not None:
        w.uint(8)
        w.int(x)
        n += 1
    w.close_map(head, n)


def _read_metrics_name(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["name"] = r.str()
            case 2:
                read["kind"] = r.str()
            case 3:
                read["series_count"] = r.uint()
            case 4:
                read["unit"] = r.str()
            case 5:
                read["help"] = r.str()
            case 6:
                read["keep"] = r.uint()
            case 7:
                read["retained_from"] = r.int()
            case 8:
                read["newest"] = r.int()
            case _:
                r.skip()
    r.leave()
    return read


# A name the store holds samples of.
MetricsName = Message(
    "metrics.Name",
    {
        "name": (1, str_),
        "kind": (2, str_),
        "series_count": (3, uint),
        "unit": (4, str_),
        "help": (5, str_),
        "keep": (6, uint),
        # its oldest sample, or where its keep begins while it holds older
        "retained_from": (7, int_),
        "newest": (8, int_),
    },
    _write_metrics_name,
    _read_metrics_name,
)


def _write_metrics_name_page(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("names")) is not None:
        w.uint(1)
        w.array(len(x))
        for item0 in x:
            MetricsName.write(w, item0)
        n += 1
    if (x := v.get("next")) is not None:
        w.uint(2)
        w.str(x)
        n += 1
    w.close_map(head, n)


def _read_metrics_name_page(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                items0 = [MetricsName.read(r) for _ in range(r.array())]
                r.leave()
                read["names"] = items0
            case 2:
                read["next"] = r.str()
            case _:
                r.skip()
    r.leave()
    return read


MetricsNamePage = Message(
    "metrics.NamePage",
    {
        "names": (1, list_(MetricsName)),
        # the page after it; the last page has none
        "next": (2, str_),
    },
    _write_metrics_name_page,
    _read_metrics_name_page,
)


def _write_metrics_text_page(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("texts")) is not None:
        w.uint(1)
        w.array(len(x))
        for item0 in x:
            w.str(item0)
        n += 1
    if (x := v.get("next")) is not None:
        w.uint(2)
        w.str(x)
        n += 1
    w.close_map(head, n)


def _read_metrics_text_page(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                items0 = [r.str() for _ in range(r.array())]
                r.leave()
                read["texts"] = items0
            case 2:
                read["next"] = r.str()
            case _:
                r.skip()
    r.leave()
    return read


# A page of a name's labels, or of a label's values.
MetricsTextPage = Message(
    "metrics.TextPage",
    {
        "texts": (1, list_(str_)),
        "next": (2, str_),
    },
    _write_metrics_text_page,
    _read_metrics_text_page,
)


def _write_metrics_description(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("name")) is not None:
        w.uint(1)
        w.str(x)
        n += 1
    if (x := v.get("unit")) is not None:
        w.uint(2)
        w.str(x)
        n += 1
    if (x := v.get("help")) is not None:
        w.uint(3)
        w.str(x)
        n += 1
    w.close_map(head, n)


def _read_metrics_description(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["name"] = r.str()
            case 2:
                read["unit"] = r.str()
            case 3:
                read["help"] = r.str()
            case _:
                r.skip()
    r.leave()
    return read


# A name's unit and help; empty ones remove them.
MetricsDescription = Message(
    "metrics.Description",
    {
        "name": (1, str_),
        "unit": (2, str_),
        "help": (3, str_),
    },
    _write_metrics_description,
    _read_metrics_description,
)


def _write_metrics_name_asked(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("name")) is not None:
        w.uint(1)
        w.str(x)
        n += 1
    w.close_map(head, n)


def _read_metrics_name_asked(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["name"] = r.str()
            case _:
                r.skip()
    r.leave()
    return read


MetricsNameAsked = Message(
    "metrics.NameAsked",
    {
        "name": (1, str_),
    },
    _write_metrics_name_asked,
    _read_metrics_name_asked,
)


def _write_metrics_series_named(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("name")) is not None:
        w.uint(1)
        w.str(x)
        n += 1
    if (x := v.get("labels")) is not None:
        w.uint(2)
        w.map(len(x))
        for name0 in sorted(x):
            w.str(name0)
            w.str(x[name0])
        n += 1
    w.close_map(head, n)


def _read_metrics_series_named(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["name"] = r.str()
            case 2:
                named0: dict[str, Any] = {}
                names0: set[str] = set()
                for _ in range(r.map()):
                    name0 = r.name(names0)
                    named0[name0] = r.str()
                r.leave()
                read["labels"] = named0
            case _:
                r.skip()
    r.leave()
    return read


# One series, by exactly its name and labels.
MetricsSeriesNamed = Message(
    "metrics.SeriesNamed",
    {
        "name": (1, str_),
        "labels": (2, names(str_)),
    },
    _write_metrics_series_named,
    _read_metrics_series_named,
)


def _write_metrics_deleted(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("found")) is not None:
        w.uint(1)
        w.bool(x)
        n += 1
    w.close_map(head, n)


def _read_metrics_deleted(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["found"] = r.bool()
            case _:
                r.skip()
    r.leave()
    return read


MetricsDeleted = Message(
    "metrics.Deleted",
    {
        "found": (1, bool_),
    },
    _write_metrics_deleted,
    _read_metrics_deleted,
)


def _write_metrics_damage(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("name")) is not None:
        w.uint(1)
        w.str(x)
        n += 1
    if (x := v.get("labels")) is not None:
        w.uint(2)
        w.map(len(x))
        for name0 in sorted(x):
            w.str(name0)
            w.str(x[name0])
        n += 1
    if (x := v.get("error")) is not None:
        w.uint(3)
        w.str(x)
        n += 1
    w.close_map(head, n)


def _read_metrics_damage(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["name"] = r.str()
            case 2:
                named0: dict[str, Any] = {}
                names0: set[str] = set()
                for _ in range(r.map()):
                    name0 = r.name(names0)
                    named0[name0] = r.str()
                r.leave()
                read["labels"] = named0
            case 3:
                read["error"] = r.str()
            case _:
                r.skip()
    r.leave()
    return read


# A series whose bytes no longer read, and what broke.
MetricsDamage = Message(
    "metrics.Damage",
    {
        "name": (1, str_),
        "labels": (2, names(str_)),
        "error": (3, str_),
    },
    _write_metrics_damage,
    _read_metrics_damage,
)


def _write_metrics_damages(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("damages")) is not None:
        w.uint(1)
        w.array(len(x))
        for item0 in x:
            MetricsDamage.write(w, item0)
        n += 1
    w.close_map(head, n)


def _read_metrics_damages(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                items0 = [MetricsDamage.read(r) for _ in range(r.array())]
                r.leave()
                read["damages"] = items0
            case _:
                r.skip()
    r.leave()
    return read


MetricsDamages = Message(
    "metrics.Damages",
    {
        "damages": (1, list_(MetricsDamage)),
    },
    _write_metrics_damages,
    _read_metrics_damages,
)


def _write_metrics_session(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("id")) is not None:
        w.uint(1)
        w.bin(x)
        n += 1
    if (x := v.get("after")) is not None:
        w.uint(2)
        w.uint(x)
        n += 1
    w.close_map(head, n)


def _read_metrics_session(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["id"] = r.bin()
            case 2:
                read["after"] = r.uint()
            case _:
                r.skip()
    r.leave()
    return read


# An SDK's instruments' stream: its session, sixteen random bytes drawn when
# it opened the store, and the last batch the store answered it, 0 for none.
MetricsSession = Message(
    "metrics.Session",
    {
        "id": (1, bin_),
        "after": (2, uint),
    },
    _write_metrics_session,
    _read_metrics_session,
)


def _write_metrics_instrument(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("number")) is not None:
        w.uint(1)
        w.uint(x)
        n += 1
    if (x := v.get("name")) is not None:
        w.uint(2)
        w.str(x)
        n += 1
    if (x := v.get("form")) is not None:
        w.uint(3)
        w.str(x)
        n += 1
    if (x := v.get("unit")) is not None:
        w.uint(4)
        w.str(x)
        n += 1
    if (x := v.get("help")) is not None:
        w.uint(5)
        w.str(x)
        n += 1
    if (x := v.get("keep")) is not None:
        w.uint(6)
        w.uint(x)
        n += 1
    if (x := v.get("bounds")) is not None:
        w.uint(7)
        w.array(len(x))
        for item0 in x:
            w.float(item0)
        n += 1
    if (x := v.get("exponential")) is not None:
        w.uint(8)
        w.bool(x)
        n += 1
    w.close_map(head, n)


def _read_metrics_instrument(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["number"] = r.uint()
            case 2:
                read["name"] = r.str()
            case 3:
                read["form"] = r.str()
            case 4:
                read["unit"] = r.str()
            case 5:
                read["help"] = r.str()
            case 6:
                read["keep"] = r.uint()
            case 7:
                items0 = [r.float() for _ in range(r.array())]
                r.leave()
                read["bounds"] = items0
            case 8:
                read["exponential"] = r.bool()
            case _:
                r.skip()
    r.leave()
    return read


# An instrument a batch counts into, by a number the stream gives it.
MetricsInstrument = Message(
    "metrics.Instrument",
    {
        "number": (1, uint),
        "name": (2, str_),
        # counter, gauge, timer or histogram
        "form": (3, str_),
        "unit": (4, str_),
        "help": (5, str_),
        "keep": (6, uint),
        # a timer's or a histogram's upper bounds, ascending
        "bounds": (7, list_(float_)),
        # a timer's spread on the store's grid
        "exponential": (8, bool_),
    },
    _write_metrics_instrument,
    _read_metrics_instrument,
)


def _write_metrics_set(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("number")) is not None:
        w.uint(1)
        w.uint(x)
        n += 1
    if (x := v.get("instrument")) is not None:
        w.uint(2)
        w.uint(x)
        n += 1
    if (x := v.get("labels")) is not None:
        w.uint(3)
        w.map(len(x))
        for name0 in sorted(x):
            w.str(name0)
            w.str(x[name0])
        n += 1
    if (x := v.get("overflow")) is not None:
        w.uint(4)
        w.bool(x)
        n += 1
    w.close_map(head, n)


def _read_metrics_set(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["number"] = r.uint()
            case 2:
                read["instrument"] = r.uint()
            case 3:
                named0: dict[str, Any] = {}
                names0: set[str] = set()
                for _ in range(r.map()):
                    name0 = r.name(names0)
                    named0[name0] = r.str()
                r.leave()
                read["labels"] = named0
            case 4:
                read["overflow"] = r.bool()
            case _:
                r.skip()
    r.leave()
    return read


# A set of labels of an instrument, by a number the stream gives it, or its
# series of __overflow, where a client counts past the sets it holds.
MetricsSet = Message(
    "metrics.Set",
    {
        "number": (1, uint),
        "instrument": (2, uint),
        "labels": (3, names(str_)),
        "overflow": (4, bool_),
    },
    _write_metrics_set,
    _read_metrics_set,
)


def _write_metrics_change(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("set")) is not None:
        w.uint(1)
        w.uint(x)
        n += 1
    if (x := v.get("add")) is not None:
        w.uint(2)
        w.float(x)
        n += 1
    if (x := v.get("value")) is not None:
        w.uint(3)
        w.float(x)
        n += 1
    if (x := v.get("count")) is not None:
        w.uint(4)
        w.uint(x)
        n += 1
    if (x := v.get("sum")) is not None:
        w.uint(5)
        w.float(x)
        n += 1
    if (x := v.get("longest")) is not None:
        w.uint(6)
        w.float(x)
        n += 1
    if (x := v.get("buckets")) is not None:
        w.uint(7)
        w.array(len(x))
        for item0 in x:
            w.uint(item0)
        n += 1
    if (x := v.get("bins")) is not None:
        w.uint(8)
        w.map(len(x))
        for name0 in sorted(x):
            w.str(name0)
            w.uint(x[name0])
        n += 1
    w.close_map(head, n)


def _read_metrics_change(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["set"] = r.uint()
            case 2:
                read["add"] = r.float()
            case 3:
                read["value"] = r.float()
            case 4:
                read["count"] = r.uint()
            case 5:
                read["sum"] = r.float()
            case 6:
                read["longest"] = r.float()
            case 7:
                items0 = [r.uint() for _ in range(r.array())]
                r.leave()
                read["buckets"] = items0
            case 8:
                named0: dict[str, Any] = {}
                names0: set[str] = set()
                for _ in range(r.map()):
                    name0 = r.name(names0)
                    named0[name0] = r.uint()
                r.leave()
                read["bins"] = named0
            case _:
                r.skip()
    r.leave()
    return read


# What a set of labels changed since the batch before: a counter's sum to
# add, a gauge's value, or what a timer or a histogram measured.
MetricsChange = Message(
    "metrics.Change",
    {
        "set": (1, uint),
        "add": (2, float_),
        "value": (3, float_),
        "count": (4, uint),
        "sum": (5, float_),
        "longest": (6, float_),
        # each bound's own count, +Inf last
        "buckets": (7, list_(uint)),
        # each interval of the grid's count, by its label: 3:13, zero
        "bins": (8, names(uint)),
    },
    _write_metrics_change,
    _read_metrics_change,
)


def _write_metrics_batch(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("number")) is not None:
        w.uint(1)
        w.uint(x)
        n += 1
    if (x := v.get("instruments")) is not None:
        w.uint(2)
        w.array(len(x))
        for item0 in x:
            MetricsInstrument.write(w, item0)
        n += 1
    if (x := v.get("sets")) is not None:
        w.uint(3)
        w.array(len(x))
        for item0 in x:
            MetricsSet.write(w, item0)
        n += 1
    if (x := v.get("changes")) is not None:
        w.uint(4)
        w.array(len(x))
        for item0 in x:
            MetricsChange.write(w, item0)
        n += 1
    if (x := v.get("flush")) is not None:
        w.uint(5)
        w.bool(x)
        n += 1
    w.close_map(head, n)


def _read_metrics_batch(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["number"] = r.uint()
            case 2:
                items0 = [MetricsInstrument.read(r) for _ in range(r.array())]
                r.leave()
                read["instruments"] = items0
            case 3:
                items0 = [MetricsSet.read(r) for _ in range(r.array())]
                r.leave()
                read["sets"] = items0
            case 4:
                items0 = [MetricsChange.read(r) for _ in range(r.array())]
                r.leave()
                read["changes"] = items0
            case 5:
                read["flush"] = r.bool()
            case _:
                r.skip()
    r.leave()
    return read


# A batch of an SDK's changes, numbered from 1 in its session, with the
# instruments and the sets of labels it names that the stream has not
# declared yet.
MetricsBatch = Message(
    "metrics.Batch",
    {
        "number": (1, uint),
        "instruments": (2, list_(MetricsInstrument)),
        "sets": (3, list_(MetricsSet)),
        "changes": (4, list_(MetricsChange)),
        # the store writes its totals before it answers
        "flush": (5, bool_),
    },
    _write_metrics_batch,
    _read_metrics_batch,
)


def _write_metrics_refused(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("instrument")) is not None:
        w.uint(1)
        w.uint(x)
        n += 1
    if (x := v.get("error")) is not None:
        w.uint(2)
        w.str(x)
        n += 1
    w.close_map(head, n)


def _read_metrics_refused(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["instrument"] = r.uint()
            case 2:
                read["error"] = r.str()
            case _:
                r.skip()
    r.leave()
    return read


# An instrument the store refused, by the stream's number for it.
MetricsRefused = Message(
    "metrics.Refused",
    {
        "instrument": (1, uint),
        "error": (2, str_),
    },
    _write_metrics_refused,
    _read_metrics_refused,
)


def _write_metrics_ack(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("applied")) is not None:
        w.uint(1)
        w.uint(x)
        n += 1
    if (x := v.get("known")) is not None:
        w.uint(2)
        w.bool(x)
        n += 1
    if (x := v.get("refused")) is not None:
        w.uint(3)
        w.array(len(x))
        for item0 in x:
            MetricsRefused.write(w, item0)
        n += 1
    w.close_map(head, n)


def _read_metrics_ack(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["applied"] = r.uint()
            case 2:
                read["known"] = r.bool()
            case 3:
                items0 = [MetricsRefused.read(r) for _ in range(r.array())]
                r.leave()
                read["refused"] = items0
            case _:
                r.skip()
    r.leave()
    return read


# The store's answer: the last batch of the session it applied, and whether
# it knew the session; the stream's RESPONSE says it first.
MetricsAck = Message(
    "metrics.Ack",
    {
        "applied": (1, uint),
        "known": (2, bool_),
        "refused": (3, list_(MetricsRefused)),
    },
    _write_metrics_ack,
    _read_metrics_ack,
)


def _write_records_open(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("stream")) is not None:
        w.uint(1)
        w.str(x)
        n += 1
    if (x := v.get("keep")) is not None:
        w.uint(2)
        w.uint(x)
        n += 1
    w.close_map(head, n)


def _read_records_open(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["stream"] = r.str()
            case 2:
                read["keep"] = r.uint()
            case _:
                r.skip()
    r.leave()
    return read


# Opens a stream of events by name, made with its first event.
RecordsOpen = Message(
    "records.Open",
    {
        # [a-z0-9][a-z0-9_-]{0,63}
        "stream": (1, str_),
        # how long it keeps its events: its own, kept in the file
        "keep": (2, uint),
    },
    _write_records_open,
    _read_records_open,
)


def _write_records_new_event(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("name")) is not None:
        w.uint(1)
        w.str(x)
        n += 1
    if (x := v.get("at")) is not None:
        w.uint(2)
        w.int(x)
        n += 1
    if (x := v.get("level")) is not None:
        w.uint(3)
        w.int(x)
        n += 1
    if (x := v.get("message")) is not None:
        w.uint(4)
        w.str(x)
        n += 1
    if (x := v.get("fields")) is not None:
        w.uint(5)
        w.str(x)
        n += 1
    if (x := v.get("context")) is not None:
        w.uint(6)
        w.str(x)
        n += 1
    if (x := v.get("trace_id")) is not None:
        w.uint(7)
        w.bin(x)
        n += 1
    if (x := v.get("span_id")) is not None:
        w.uint(8)
        w.bin(x)
        n += 1
    if (x := v.get("id")) is not None:
        w.uint(9)
        w.str(x)
        n += 1
    w.close_map(head, n)


def _read_records_new_event(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["name"] = r.str()
            case 2:
                read["at"] = r.int()
            case 3:
                read["level"] = r.int()
            case 4:
                read["message"] = r.str()
            case 5:
                read["fields"] = r.str()
            case 6:
                read["context"] = r.str()
            case 7:
                read["trace_id"] = r.bin()
            case 8:
                read["span_id"] = r.bin()
            case 9:
                read["id"] = r.str()
            case _:
                r.skip()
    r.leave()
    return read


# An event to write.
RecordsNewEvent = Message(
    "records.NewEvent",
    {
        "name": (1, str_),
        # the store's clock's now unless given
        "at": (2, int_),
        # -4 debug, 0 info, 4 warn, 8 error, between them as slog counts
        "level": (3, int_),
        "message": (4, str_),
        # an object
        "fields": (5, str_),
        # an object
        "context": (6, str_),
        # 16 bytes
        "trace_id": (7, bin_),
        # 8 bytes
        "span_id": (8, bin_),
        # 1 to 128 bytes: an event of an id its stream holds is written as nothing
        "id": (9, str_),
    },
    _write_records_new_event,
    _read_records_new_event,
)


def _write_records_append(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("handle")) is not None:
        w.uint(1)
        w.uint(x)
        n += 1
    if (x := v.get("events")) is not None:
        w.uint(2)
        w.array(len(x))
        for item0 in x:
            RecordsNewEvent.write(w, item0)
        n += 1
    w.close_map(head, n)


def _read_records_append(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["handle"] = r.uint()
            case 2:
                items0 = [RecordsNewEvent.read(r) for _ in range(r.array())]
                r.leave()
                read["events"] = items0
            case _:
                r.skip()
    r.leave()
    return read


# Events to write to a stream, all of them or none.
RecordsAppend = Message(
    "records.Append",
    {
        "handle": (1, uint),
        "events": (2, list_(RecordsNewEvent)),
    },
    _write_records_append,
    _read_records_append,
)


def _write_records_appended(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("held")) is not None:
        w.uint(1)
        w.uint(x)
        n += 1
    w.close_map(head, n)


def _read_records_appended(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["held"] = r.uint()
            case _:
                r.skip()
    r.leave()
    return read


# What an append wrote: none of an event whose id its stream held, or an
# earlier event of the append held.
RecordsAppended = Message(
    "records.Appended",
    {
        # the events written as nothing
        "held": (1, uint),
    },
    _write_records_appended,
    _read_records_appended,
)


def _write_records_event(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("at")) is not None:
        w.uint(1)
        w.int(x)
        n += 1
    if (x := v.get("stream")) is not None:
        w.uint(2)
        w.str(x)
        n += 1
    if (x := v.get("name")) is not None:
        w.uint(3)
        w.str(x)
        n += 1
    if (x := v.get("level")) is not None:
        w.uint(4)
        w.int(x)
        n += 1
    if (x := v.get("message")) is not None:
        w.uint(5)
        w.str(x)
        n += 1
    if (x := v.get("fields")) is not None:
        w.uint(6)
        w.str(x)
        n += 1
    if (x := v.get("context")) is not None:
        w.uint(7)
        w.str(x)
        n += 1
    if (x := v.get("trace_id")) is not None:
        w.uint(8)
        w.bin(x)
        n += 1
    if (x := v.get("span_id")) is not None:
        w.uint(9)
        w.bin(x)
        n += 1
    if (x := v.get("id")) is not None:
        w.uint(10)
        w.str(x)
        n += 1
    w.close_map(head, n)


def _read_records_event(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["at"] = r.int()
            case 2:
                read["stream"] = r.str()
            case 3:
                read["name"] = r.str()
            case 4:
                read["level"] = r.int()
            case 5:
                read["message"] = r.str()
            case 6:
                read["fields"] = r.str()
            case 7:
                read["context"] = r.str()
            case 8:
                read["trace_id"] = r.bin()
            case 9:
                read["span_id"] = r.bin()
            case 10:
                read["id"] = r.str()
            case _:
                r.skip()
    r.leave()
    return read


# An event as a read gives it.
RecordsEvent = Message(
    "records.Event",
    {
        "at": (1, int_),
        "stream": (2, str_),
        "name": (3, str_),
        "level": (4, int_),
        "message": (5, str_),
        # an object; none when it has no field
        "fields": (6, str_),
        "context": (7, str_),
        "trace_id": (8, bin_),
        "span_id": (9, bin_),
        "id": (10, str_),
    },
    _write_records_event,
    _read_records_event,
)


def _write_records_query(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("handle")) is not None:
        w.uint(1)
        w.uint(x)
        n += 1
    if (x := v.get("since")) is not None:
        w.uint(2)
        w.uint(x)
        n += 1
    if (x := v.get("from")) is not None:
        w.uint(3)
        w.int(x)
        n += 1
    if (x := v.get("to")) is not None:
        w.uint(4)
        w.int(x)
        n += 1
    if (x := v.get("streams")) is not None:
        w.uint(5)
        w.array(len(x))
        for item0 in x:
            w.str(item0)
        n += 1
    if (x := v.get("names")) is not None:
        w.uint(6)
        w.array(len(x))
        for item0 in x:
            w.str(item0)
        n += 1
    if (x := v.get("min_level")) is not None:
        w.uint(7)
        w.int(x)
        n += 1
    if (x := v.get("trace_id")) is not None:
        w.uint(8)
        w.bin(x)
        n += 1
    if (x := v.get("where")) is not None:
        w.uint(9)
        w.map(len(x))
        for name0 in sorted(x):
            w.str(name0)
            w.str(x[name0])
        n += 1
    if (x := v.get("search")) is not None:
        w.uint(10)
        w.str(x)
        n += 1
    if (x := v.get("newest")) is not None:
        w.uint(11)
        w.bool(x)
        n += 1
    if (x := v.get("limit")) is not None:
        w.uint(12)
        w.uint(x)
        n += 1
    if (x := v.get("after")) is not None:
        w.uint(13)
        w.str(x)
        n += 1
    w.close_map(head, n)


def _read_records_query(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["handle"] = r.uint()
            case 2:
                read["since"] = r.uint()
            case 3:
                read["from"] = r.int()
            case 4:
                read["to"] = r.int()
            case 5:
                items0 = [r.str() for _ in range(r.array())]
                r.leave()
                read["streams"] = items0
            case 6:
                items0 = [r.str() for _ in range(r.array())]
                r.leave()
                read["names"] = items0
            case 7:
                read["min_level"] = r.int()
            case 8:
                read["trace_id"] = r.bin()
            case 9:
                named0: dict[str, Any] = {}
                names0: set[str] = set()
                for _ in range(r.map()):
                    name0 = r.name(names0)
                    named0[name0] = r.str()
                r.leave()
                read["where"] = named0
            case 10:
                read["search"] = r.str()
            case 11:
                read["newest"] = r.bool()
            case 12:
                read["limit"] = r.uint()
            case 13:
                read["after"] = r.str()
            case _:
                r.skip()
    r.leave()
    return read


# What a read finds: each condition given holds.
RecordsQuery = Message(
    "records.Query",
    {
        # the stream opened as it; every stream without one
        "handle": (1, uint),
        # the span before now; or from and to
        "since": (2, uint),
        "from": (3, int_),
        "to": (4, int_),
        "streams": (5, list_(str_)),
        "names": (6, list_(str_)),
        "min_level": (7, int_),
        "trace_id": (8, bin_),
        # each key with this value, in fields or in context
        "where": (9, names(str_)),
        "search": (10, str_),
        "newest": (11, bool_),
        # a page's events, top's values, fieldNames' keys
        "limit": (12, uint),
        # a page's next
        "after": (13, str_),
    },
    _write_records_query,
    _read_records_query,
)


def _write_records_page(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("events")) is not None:
        w.uint(1)
        w.array(len(x))
        for item0 in x:
            RecordsEvent.write(w, item0)
        n += 1
    if (x := v.get("next")) is not None:
        w.uint(2)
        w.str(x)
        n += 1
    w.close_map(head, n)


def _read_records_page(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                items0 = [RecordsEvent.read(r) for _ in range(r.array())]
                r.leave()
                read["events"] = items0
            case 2:
                read["next"] = r.str()
            case _:
                r.skip()
    r.leave()
    return read


RecordsPage = Message(
    "records.Page",
    {
        "events": (1, list_(RecordsEvent)),
        # the page after it; the last page has none
        "next": (2, str_),
    },
    _write_records_page,
    _read_records_page,
)


def _write_records_count(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("count")) is not None:
        w.uint(1)
        w.uint(x)
        n += 1
    w.close_map(head, n)


def _read_records_count(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["count"] = r.uint()
            case _:
                r.skip()
    r.leave()
    return read


RecordsCount = Message(
    "records.Count",
    {
        "count": (1, uint),
    },
    _write_records_count,
    _read_records_count,
)


def _write_records_top_asked(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("query")) is not None:
        w.uint(1)
        RecordsQuery.write(w, x)
        n += 1
    if (x := v.get("key")) is not None:
        w.uint(2)
        w.str(x)
        n += 1
    w.close_map(head, n)


def _read_records_top_asked(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["query"] = RecordsQuery.read(r)
            case 2:
                read["key"] = r.str()
            case _:
                r.skip()
    r.leave()
    return read


# A key's most frequent values among what a query finds.
RecordsTopAsked = Message(
    "records.TopAsked",
    {
        "query": (1, RecordsQuery),
        # a field's or a context's, or stream, name or level
        "key": (2, str_),
    },
    _write_records_top_asked,
    _read_records_top_asked,
)


def _write_records_top(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("value")) is not None:
        w.uint(1)
        w.str(x)
        n += 1
    if (x := v.get("count")) is not None:
        w.uint(2)
        w.uint(x)
        n += 1
    w.close_map(head, n)


def _read_records_top(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["value"] = r.str()
            case 2:
                read["count"] = r.uint()
            case _:
                r.skip()
    r.leave()
    return read


RecordsTop = Message(
    "records.Top",
    {
        "value": (1, str_),
        "count": (2, uint),
    },
    _write_records_top,
    _read_records_top,
)


def _write_records_tops(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("values")) is not None:
        w.uint(1)
        w.array(len(x))
        for item0 in x:
            RecordsTop.write(w, item0)
        n += 1
    w.close_map(head, n)


def _read_records_tops(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                items0 = [RecordsTop.read(r) for _ in range(r.array())]
                r.leave()
                read["values"] = items0
            case _:
                r.skip()
    r.leave()
    return read


RecordsTops = Message(
    "records.Tops",
    {
        "values": (1, list_(RecordsTop)),
    },
    _write_records_tops,
    _read_records_tops,
)


def _write_records_aggregate_asked(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("query")) is not None:
        w.uint(1)
        RecordsQuery.write(w, x)
        n += 1
    if (x := v.get("every")) is not None:
        w.uint(2)
        w.uint(x)
        n += 1
    if (x := v.get("by")) is not None:
        w.uint(3)
        w.array(len(x))
        for item0 in x:
            w.str(item0)
        n += 1
    if (x := v.get("stats")) is not None:
        w.uint(4)
        w.array(len(x))
        for item0 in x:
            w.str(item0)
        n += 1
    if (x := v.get("distinct")) is not None:
        w.uint(5)
        w.array(len(x))
        for item0 in x:
            w.str(item0)
        n += 1
    if (x := v.get("calendar")) is not None:
        w.uint(6)
        w.str(x)
        n += 1
    if (x := v.get("time_zone")) is not None:
        w.uint(7)
        w.str(x)
        n += 1
    w.close_map(head, n)


def _read_records_aggregate_asked(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["query"] = RecordsQuery.read(r)
            case 2:
                read["every"] = r.uint()
            case 3:
                items0 = [r.str() for _ in range(r.array())]
                r.leave()
                read["by"] = items0
            case 4:
                items0 = [r.str() for _ in range(r.array())]
                r.leave()
                read["stats"] = items0
            case 5:
                items0 = [r.str() for _ in range(r.array())]
                r.leave()
                read["distinct"] = items0
            case 6:
                read["calendar"] = r.str()
            case 7:
                read["time_zone"] = r.str()
            case _:
                r.skip()
    r.leave()
    return read


# A row for each span of every and each value of the keys of by.
RecordsAggregateAsked = Message(
    "records.AggregateAsked",
    {
        "query": (1, RecordsQuery),
        "every": (2, uint),
        "by": (3, list_(str_)),
        "stats": (4, list_(str_)),
        "distinct": (5, list_(str_)),
        # day, week or month: rows of a calendar in timeZone, in place of every
        "calendar": (6, str_),
        # an IANA name, UTC among them, which calendar takes
        "time_zone": (7, str_),
    },
    _write_records_aggregate_asked,
    _read_records_aggregate_asked,
)


def _write_records_stats(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("samples")) is not None:
        w.uint(1)
        w.uint(x)
        n += 1
    if (x := v.get("sum")) is not None:
        w.uint(2)
        w.str(x)
        n += 1
    if (x := v.get("min")) is not None:
        w.uint(3)
        w.str(x)
        n += 1
    if (x := v.get("max")) is not None:
        w.uint(4)
        w.str(x)
        n += 1
    if (x := v.get("avg")) is not None:
        w.uint(5)
        w.float(x)
        n += 1
    w.close_map(head, n)


def _read_records_stats(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["samples"] = r.uint()
            case 2:
                read["sum"] = r.str()
            case 3:
                read["min"] = r.str()
            case 4:
                read["max"] = r.str()
            case 5:
                read["avg"] = r.float()
            case _:
                r.skip()
    r.leave()
    return read


# A field's numbers in a row: a sum, a min and a max are JSON numbers, an
# integer's exact.
RecordsStats = Message(
    "records.Stats",
    {
        "samples": (1, uint),
        "sum": (2, str_),
        "min": (3, str_),
        "max": (4, str_),
        "avg": (5, float_),
    },
    _write_records_stats,
    _read_records_stats,
)


def _write_records_row(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("at")) is not None:
        w.uint(1)
        w.int(x)
        n += 1
    if (x := v.get("count")) is not None:
        w.uint(2)
        w.uint(x)
        n += 1
    if (x := v.get("by")) is not None:
        w.uint(3)
        w.map(len(x))
        for name0 in sorted(x):
            w.str(name0)
            w.str(x[name0])
        n += 1
    if (x := v.get("stats")) is not None:
        w.uint(4)
        w.map(len(x))
        for name0 in sorted(x):
            w.str(name0)
            RecordsStats.write(w, x[name0])
        n += 1
    if (x := v.get("distinct")) is not None:
        w.uint(5)
        w.map(len(x))
        for name0 in sorted(x):
            w.str(name0)
            w.uint(x[name0])
        n += 1
    w.close_map(head, n)


def _read_records_row(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["at"] = r.int()
            case 2:
                read["count"] = r.uint()
            case 3:
                named0: dict[str, Any] = {}
                names0: set[str] = set()
                for _ in range(r.map()):
                    name0 = r.name(names0)
                    named0[name0] = r.str()
                r.leave()
                read["by"] = named0
            case 4:
                named0: dict[str, Any] = {}
                names0: set[str] = set()
                for _ in range(r.map()):
                    name0 = r.name(names0)
                    named0[name0] = RecordsStats.read(r)
                r.leave()
                read["stats"] = named0
            case 5:
                named0: dict[str, Any] = {}
                names0: set[str] = set()
                for _ in range(r.map()):
                    name0 = r.name(names0)
                    named0[name0] = r.uint()
                r.leave()
                read["distinct"] = named0
            case _:
                r.skip()
    r.leave()
    return read


RecordsRow = Message(
    "records.Row",
    {
        # where the row's span begins, counted from 1970
        "at": (1, int_),
        "count": (2, uint),
        "by": (3, names(str_)),
        "stats": (4, names(RecordsStats)),
        "distinct": (5, names(uint)),
    },
    _write_records_row,
    _read_records_row,
)


def _write_records_rows(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("rows")) is not None:
        w.uint(1)
        w.array(len(x))
        for item0 in x:
            RecordsRow.write(w, item0)
        n += 1
    w.close_map(head, n)


def _read_records_rows(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                items0 = [RecordsRow.read(r) for _ in range(r.array())]
                r.leave()
                read["rows"] = items0
            case _:
                r.skip()
    r.leave()
    return read


RecordsRows = Message(
    "records.Rows",
    {
        "rows": (1, list_(RecordsRow)),
    },
    _write_records_rows,
    _read_records_rows,
)


def _write_records_stream(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("name")) is not None:
        w.uint(1)
        w.str(x)
        n += 1
    if (x := v.get("keep")) is not None:
        w.uint(2)
        w.uint(x)
        n += 1
    if (x := v.get("retained_from")) is not None:
        w.uint(3)
        w.int(x)
        n += 1
    if (x := v.get("newest")) is not None:
        w.uint(4)
        w.int(x)
        n += 1
    w.close_map(head, n)


def _read_records_stream(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["name"] = r.str()
            case 2:
                read["keep"] = r.uint()
            case 3:
                read["retained_from"] = r.int()
            case 4:
                read["newest"] = r.int()
            case _:
                r.skip()
    r.leave()
    return read


RecordsStream = Message(
    "records.Stream",
    {
        "name": (1, str_),
        "keep": (2, uint),
        # its oldest event, or where its keep begins while it holds older
        "retained_from": (3, int_),
        "newest": (4, int_),
    },
    _write_records_stream,
    _read_records_stream,
)


def _write_records_streams(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("streams")) is not None:
        w.uint(1)
        w.array(len(x))
        for item0 in x:
            RecordsStream.write(w, item0)
        n += 1
    w.close_map(head, n)


def _read_records_streams(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                items0 = [RecordsStream.read(r) for _ in range(r.array())]
                r.leave()
                read["streams"] = items0
            case _:
                r.skip()
    r.leave()
    return read


RecordsStreams = Message(
    "records.Streams",
    {
        "streams": (1, list_(RecordsStream)),
    },
    _write_records_streams,
    _read_records_streams,
)


def _write_records_field_names_asked(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("query")) is not None:
        w.uint(1)
        RecordsQuery.write(w, x)
        n += 1
    if (x := v.get("prefix")) is not None:
        w.uint(2)
        w.str(x)
        n += 1
    w.close_map(head, n)


def _read_records_field_names_asked(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["query"] = RecordsQuery.read(r)
            case 2:
                read["prefix"] = r.str()
            case _:
                r.skip()
    r.leave()
    return read


# The keys the events a query finds hold, sorted.
RecordsFieldNamesAsked = Message(
    "records.FieldNamesAsked",
    {
        "query": (1, RecordsQuery),
        "prefix": (2, str_),
    },
    _write_records_field_names_asked,
    _read_records_field_names_asked,
)


def _write_records_names(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("names")) is not None:
        w.uint(1)
        w.array(len(x))
        for item0 in x:
            w.str(item0)
        n += 1
    w.close_map(head, n)


def _read_records_names(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                items0 = [r.str() for _ in range(r.array())]
                r.leave()
                read["names"] = items0
            case _:
                r.skip()
    r.leave()
    return read


RecordsNames = Message(
    "records.Names",
    {
        "names": (1, list_(str_)),
    },
    _write_records_names,
    _read_records_names,
)


def _write_records_poll_asked(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("cursor")) is not None:
        w.uint(1)
        w.str(x)
        n += 1
    if (x := v.get("limit")) is not None:
        w.uint(2)
        w.uint(x)
        n += 1
    w.close_map(head, n)


def _read_records_poll_asked(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["cursor"] = r.str()
            case 2:
                read["limit"] = r.uint()
            case _:
                r.skip()
    r.leave()
    return read


# Every packed event once, a batch a call, from a cursor the program keeps.
RecordsPollAsked = Message(
    "records.PollAsked",
    {
        # the oldest event kept without one
        "cursor": (1, str_),
        # 1000 unless given
        "limit": (2, uint),
    },
    _write_records_poll_asked,
    _read_records_poll_asked,
)


def _write_records_batch(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("events")) is not None:
        w.uint(1)
        w.array(len(x))
        for item0 in x:
            RecordsEvent.write(w, item0)
        n += 1
    if (x := v.get("cursor")) is not None:
        w.uint(2)
        w.str(x)
        n += 1
    if (x := v.get("expired")) is not None:
        w.uint(3)
        w.uint(x)
        n += 1
    w.close_map(head, n)


def _read_records_batch(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                items0 = [RecordsEvent.read(r) for _ in range(r.array())]
                r.leave()
                read["events"] = items0
            case 2:
                read["cursor"] = r.str()
            case 3:
                read["expired"] = r.uint()
            case _:
                r.skip()
    r.leave()
    return read


RecordsBatch = Message(
    "records.Batch",
    {
        "events": (1, list_(RecordsEvent)),
        "cursor": (2, str_),
        # the events keep removed before the cursor reached them
        "expired": (3, uint),
    },
    _write_records_batch,
    _read_records_batch,
)


def _write_records_tail_batch(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("events")) is not None:
        w.uint(1)
        w.array(len(x))
        for item0 in x:
            RecordsEvent.write(w, item0)
        n += 1
    if (x := v.get("missed")) is not None:
        w.uint(2)
        w.uint(x)
        n += 1
    w.close_map(head, n)


def _read_records_tail_batch(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                items0 = [RecordsEvent.read(r) for _ in range(r.array())]
                r.leave()
                read["events"] = items0
            case 2:
                read["missed"] = r.uint()
            case _:
                r.skip()
    r.leave()
    return read


# A tail's batch: the events appended since the last that its query finds,
# and how many left the store's hold before the tail read them.
RecordsTailBatch = Message(
    "records.TailBatch",
    {
        "events": (1, list_(RecordsEvent)),
        "missed": (2, uint),
    },
    _write_records_tail_batch,
    _read_records_tail_batch,
)


def _write_records_piece(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("bytes")) is not None:
        w.uint(1)
        w.bin(x)
        n += 1
    w.close_map(head, n)


def _read_records_piece(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["bytes"] = r.bin()
            case _:
                r.skip()
    r.leave()
    return read


# A piece of another program's output, cut anywhere.
RecordsPiece = Message(
    "records.Piece",
    {
        "bytes": (1, bin_),
    },
    _write_records_piece,
    _read_records_piece,
)


def _write_records_dropped(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("dropped")) is not None:
        w.uint(1)
        w.uint(x)
        n += 1
    w.close_map(head, n)


def _read_records_dropped(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["dropped"] = r.uint()
            case _:
                r.skip()
    r.leave()
    return read


# What a writer of lines dropped: lines past its buffer, longer than an
# event holds, or refused by the file.
RecordsDropped = Message(
    "records.Dropped",
    {
        "dropped": (1, uint),
    },
    _write_records_dropped,
    _read_records_dropped,
)


def _write_records_damage(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("part")) is not None:
        w.uint(1)
        w.str(x)
        n += 1
    if (x := v.get("id")) is not None:
        w.uint(2)
        w.uint(x)
        n += 1
    if (x := v.get("stream")) is not None:
        w.uint(3)
        w.str(x)
        n += 1
    if (x := v.get("first")) is not None:
        w.uint(4)
        w.int(x)
        n += 1
    if (x := v.get("last")) is not None:
        w.uint(5)
        w.int(x)
        n += 1
    if (x := v.get("error")) is not None:
        w.uint(6)
        w.str(x)
        n += 1
    w.close_map(head, n)


def _read_records_damage(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["part"] = r.str()
            case 2:
                read["id"] = r.uint()
            case 3:
                read["stream"] = r.str()
            case 4:
                read["first"] = r.int()
            case 5:
                read["last"] = r.int()
            case 6:
                read["error"] = r.str()
            case _:
                r.skip()
    r.leave()
    return read


# A head row or a segment that no longer reads, as damaged lists it and drop
# takes it: a head row holds events not packed yet, a segment packed ones.
RecordsDamage = Message(
    "records.Damage",
    {
        # head row or segment
        "part": (1, str_),
        "id": (2, uint),
        "stream": (3, str_),
        # its events' oldest time
        "first": (4, int_),
        "last": (5, int_),
        # what broke
        "error": (6, str_),
    },
    _write_records_damage,
    _read_records_damage,
)


def _write_records_damages(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("damages")) is not None:
        w.uint(1)
        w.array(len(x))
        for item0 in x:
            RecordsDamage.write(w, item0)
        n += 1
    w.close_map(head, n)


def _read_records_damages(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                items0 = [RecordsDamage.read(r) for _ in range(r.array())]
                r.leave()
                read["damages"] = items0
            case _:
                r.skip()
    r.leave()
    return read


RecordsDamages = Message(
    "records.Damages",
    {
        "damages": (1, list_(RecordsDamage)),
    },
    _write_records_damages,
    _read_records_damages,
)


def _write_search_field(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("path")) is not None:
        w.uint(1)
        w.str(x)
        n += 1
    if (x := v.get("weight")) is not None:
        w.uint(2)
        w.float(x)
        n += 1
    w.close_map(head, n)


def _read_search_field(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["path"] = r.str()
            case 2:
                read["weight"] = r.float()
            case _:
                r.skip()
    r.leave()
    return read


# A field a search reads, by its path into the document, and what a word
# found there weighs.
SearchField = Message(
    "search.Field",
    {
        "path": (1, str_),
        # above 0, at most 1000
        "weight": (2, float_),
    },
    _write_search_field,
    _read_search_field,
)


def _write_search_set(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("handle")) is not None:
        w.uint(1)
        w.uint(x)
        n += 1
    if (x := v.get("id")) is not None:
        w.uint(2)
        w.str(x)
        n += 1
    if (x := v.get("value")) is not None:
        w.uint(3)
        w.str(x)
        n += 1
    if (x := v.get("vector")) is not None:
        w.uint(4)
        w.bin(x)
        n += 1
    if (x := v.get("no_vector")) is not None:
        w.uint(5)
        w.bool(x)
        n += 1
    w.close_map(head, n)


def _read_search_set(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["handle"] = r.uint()
            case 2:
                read["id"] = r.str()
            case 3:
                read["value"] = r.str()
            case 4:
                read["vector"] = r.bin()
            case 5:
                read["no_vector"] = r.bool()
            case _:
                r.skip()
    r.leave()
    return read


# A document set by its id, replacing it whole.
SearchSet = Message(
    "search.Set",
    {
        "handle": (1, uint),
        # 1 to 1024 bytes
        "id": (2, str_),
        # 1 MiB at most
        "value": (3, str_),
        # four bytes a dimension, little-endian floats
        "vector": (4, bin_),
        # the document has no vector, on an index with vectors
        "no_vector": (5, bool_),
    },
    _write_search_set,
    _read_search_set,
)


def _write_search_document(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("id")) is not None:
        w.uint(1)
        w.str(x)
        n += 1
    if (x := v.get("value")) is not None:
        w.uint(2)
        w.str(x)
        n += 1
    if (x := v.get("vector")) is not None:
        w.uint(3)
        w.bin(x)
        n += 1
    if (x := v.get("no_vector")) is not None:
        w.uint(4)
        w.bool(x)
        n += 1
    w.close_map(head, n)


def _read_search_document(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["id"] = r.str()
            case 2:
                read["value"] = r.str()
            case 3:
                read["vector"] = r.bin()
            case 4:
                read["no_vector"] = r.bool()
            case _:
                r.skip()
    r.leave()
    return read


SearchDocument = Message(
    "search.Document",
    {
        "id": (1, str_),
        "value": (2, str_),
        "vector": (3, bin_),
        "no_vector": (4, bool_),
    },
    _write_search_document,
    _read_search_document,
)


def _write_search_key(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("handle")) is not None:
        w.uint(1)
        w.uint(x)
        n += 1
    if (x := v.get("id")) is not None:
        w.uint(2)
        w.str(x)
        n += 1
    w.close_map(head, n)


def _read_search_key(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["handle"] = r.uint()
            case 2:
                read["id"] = r.str()
            case _:
                r.skip()
    r.leave()
    return read


SearchKey = Message(
    "search.Key",
    {
        "handle": (1, uint),
        "id": (2, str_),
    },
    _write_search_key,
    _read_search_key,
)


def _write_search_deleted(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("deleted")) is not None:
        w.uint(1)
        w.bool(x)
        n += 1
    w.close_map(head, n)


def _read_search_deleted(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["deleted"] = r.bool()
            case _:
                r.skip()
    r.leave()
    return read


SearchDeleted = Message(
    "search.Deleted",
    {
        "deleted": (1, bool_),
    },
    _write_search_deleted,
    _read_search_deleted,
)


def _write_search_got(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("found")) is not None:
        w.uint(1)
        w.bool(x)
        n += 1
    if (x := v.get("value")) is not None:
        w.uint(2)
        w.str(x)
        n += 1
    w.close_map(head, n)


def _read_search_got(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["found"] = r.bool()
            case 2:
                read["value"] = r.str()
            case _:
                r.skip()
    r.leave()
    return read


SearchGot = Message(
    "search.Got",
    {
        "found": (1, bool_),
        "value": (2, str_),
    },
    _write_search_got,
    _read_search_got,
)


def _write_search_query(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("handle")) is not None:
        w.uint(1)
        w.uint(x)
        n += 1
    if (x := v.get("text")) is not None:
        w.uint(2)
        w.str(x)
        n += 1
    if (x := v.get("vector")) is not None:
        w.uint(3)
        w.bin(x)
        n += 1
    if (x := v.get("vector_weight")) is not None:
        w.uint(4)
        w.float(x)
        n += 1
    if (x := v.get("rerank")) is not None:
        w.uint(5)
        w.bool(x)
        n += 1
    if (x := v.get("where")) is not None:
        w.uint(6)
        w.str(x)
        n += 1
    if (x := v.get("offset")) is not None:
        w.uint(7)
        w.uint(x)
        n += 1
    if (x := v.get("limit")) is not None:
        w.uint(8)
        w.uint(x)
        n += 1
    if (x := v.get("prefix")) is not None:
        w.uint(9)
        w.bool(x)
        n += 1
    w.close_map(head, n)


def _read_search_query(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["handle"] = r.uint()
            case 2:
                read["text"] = r.str()
            case 3:
                read["vector"] = r.bin()
            case 4:
                read["vector_weight"] = r.float()
            case 5:
                read["rerank"] = r.bool()
            case 6:
                read["where"] = r.str()
            case 7:
                read["offset"] = r.uint()
            case 8:
                read["limit"] = r.uint()
            case 9:
                read["prefix"] = r.bool()
            case _:
                r.skip()
    r.leave()
    return read


# A search: by `text`'s words, `vector`, or both, kept to what `where` passes.
SearchQuery = Message(
    "search.Query",
    {
        "handle": (1, uint),
        "text": (2, str_),
        "vector": (3, bin_),
        # this search's, in place of the index's
        "vector_weight": (4, float_),
        "rerank": (5, bool_),
        # the conditions, as docs/wire.md's "search's where" writes them
        "where": (6, str_),
        "offset": (7, uint),
        # 10 when absent; offset and limit together 1000 at most
        "limit": (8, uint),
        # the last word read as the start of a word too, as a search box types
        "prefix": (9, bool_),
    },
    _write_search_query,
    _read_search_query,
)


def _write_search_hit(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("id")) is not None:
        w.uint(1)
        w.str(x)
        n += 1
    if (x := v.get("value")) is not None:
        w.uint(2)
        w.str(x)
        n += 1
    if (x := v.get("score")) is not None:
        w.uint(3)
        w.float(x)
        n += 1
    if (x := v.get("row")) is not None:
        w.uint(4)
        w.array(len(x))
        for item0 in x:
            sql_value.write(w, item0)
        n += 1
    w.close_map(head, n)


def _read_search_hit(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["id"] = r.str()
            case 2:
                read["value"] = r.str()
            case 3:
                read["score"] = r.float()
            case 4:
                items0 = [sql_value.read(r) for _ in range(r.array())]
                r.leave()
                read["row"] = items0
            case _:
                r.skip()
    r.leave()
    return read


# A hit: a document and its value, or a table's row, a value a column of
# the answer's columns, as sql.Rows holds one.
SearchHit = Message(
    "search.Hit",
    {
        "id": (1, str_),
        # null for a table's row
        "value": (2, str_),
        "score": (3, float_),
        "row": (4, list_(sql_value)),
    },
    _write_search_hit,
    _read_search_hit,
)


def _write_search_found(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("hits")) is not None:
        w.uint(1)
        w.array(len(x))
        for item0 in x:
            SearchHit.write(w, item0)
        n += 1
    if (x := v.get("corrected")) is not None:
        w.uint(2)
        w.str(x)
        n += 1
    if (x := v.get("columns")) is not None:
        w.uint(3)
        w.array(len(x))
        for item0 in x:
            w.str(item0)
        n += 1
    w.close_map(head, n)


def _read_search_found(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                items0 = [SearchHit.read(r) for _ in range(r.array())]
                r.leave()
                read["hits"] = items0
            case 2:
                read["corrected"] = r.str()
            case 3:
                items0 = [r.str() for _ in range(r.array())]
                r.leave()
                read["columns"] = items0
            case _:
                r.skip()
    r.leave()
    return read


SearchFound = Message(
    "search.Found",
    {
        "hits": (1, list_(SearchHit)),
        # the text the search read in fact, when a word was read as another
        "corrected": (2, str_),
        # the columns of a table's index's rows
        "columns": (3, list_(str_)),
    },
    _write_search_found,
    _read_search_found,
)


def _write_search_status(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("state")) is not None:
        w.uint(1)
        w.str(x)
        n += 1
    if (x := v.get("done")) is not None:
        w.uint(2)
        w.float(x)
        n += 1
    if (x := v.get("error")) is not None:
        w.uint(3)
        w.str(x)
        n += 1
    w.close_map(head, n)


def _read_search_status(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["state"] = r.str()
            case 2:
                read["done"] = r.float()
            case 3:
                read["error"] = r.str()
            case _:
                r.skip()
    r.leave()
    return read


# Where an index stands: ready, rebuilding and how far, or failed and why.
SearchStatus = Message(
    "search.Status",
    {
        # ready, rebuilding or failed
        "state": (1, str_),
        # the share a rebuild has read
        "done": (2, float_),
        # why a rebuild failed
        "error": (3, str_),
    },
    _write_search_status,
    _read_search_status,
)


def _write_search_name(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("name")) is not None:
        w.uint(1)
        w.str(x)
        n += 1
    if (x := v.get("database")) is not None:
        w.uint(2)
        w.uint(x)
        n += 1
    w.close_map(head, n)


def _read_search_name(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["name"] = r.str()
            case 2:
                read["database"] = r.uint()
            case _:
                r.skip()
    r.leave()
    return read


SearchName = Message(
    "search.Name",
    {
        "name": (1, str_),
        # a database's handle, for a table's index
        "database": (2, uint),
    },
    _write_search_name,
    _read_search_name,
)


def _write_search_dropped(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("dropped")) is not None:
        w.uint(1)
        w.bool(x)
        n += 1
    w.close_map(head, n)


def _read_search_dropped(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["dropped"] = r.bool()
            case _:
                r.skip()
    r.leave()
    return read


SearchDropped = Message(
    "search.Dropped",
    {
        "dropped": (1, bool_),
    },
    _write_search_dropped,
    _read_search_dropped,
)


def _write_search_index(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("name")) is not None:
        w.uint(1)
        w.str(x)
        n += 1
    if (x := v.get("documents")) is not None:
        w.uint(2)
        w.uint(x)
        n += 1
    if (x := v.get("vectors")) is not None:
        w.uint(3)
        w.uint(x)
        n += 1
    if (x := v.get("opened")) is not None:
        w.uint(4)
        w.int(x)
        n += 1
    if (x := v.get("state")) is not None:
        w.uint(5)
        w.str(x)
        n += 1
    if (x := v.get("done")) is not None:
        w.uint(6)
        w.float(x)
        n += 1
    if (x := v.get("error")) is not None:
        w.uint(7)
        w.str(x)
        n += 1
    w.close_map(head, n)


def _read_search_index(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["name"] = r.str()
            case 2:
                read["documents"] = r.uint()
            case 3:
                read["vectors"] = r.uint()
            case 4:
                read["opened"] = r.int()
            case 5:
                read["state"] = r.str()
            case 6:
                read["done"] = r.float()
            case 7:
                read["error"] = r.str()
            case _:
                r.skip()
    r.leave()
    return read


# An index as the store's search.db holds it.
SearchIndex = Message(
    "search.Index",
    {
        "name": (1, str_),
        "documents": (2, uint),
        "vectors": (3, uint),
        # when a program last opened it
        "opened": (4, int_),
        "state": (5, str_),
        "done": (6, float_),
        "error": (7, str_),
    },
    _write_search_index,
    _read_search_index,
)


def _write_search_listed(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("indexes")) is not None:
        w.uint(1)
        w.array(len(x))
        for item0 in x:
            SearchIndex.write(w, item0)
        n += 1
    w.close_map(head, n)


def _read_search_listed(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                items0 = [SearchIndex.read(r) for _ in range(r.array())]
                r.leave()
                read["indexes"] = items0
            case _:
                r.skip()
    r.leave()
    return read


SearchListed = Message(
    "search.Listed",
    {
        "indexes": (1, list_(SearchIndex)),
    },
    _write_search_listed,
    _read_search_listed,
)


def _write_server_clock(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("at")) is not None:
        w.uint(1)
        w.int(x)
        n += 1
    if (x := v.get("advance")) is not None:
        w.uint(2)
        w.uint(x)
        n += 1
    w.close_map(head, n)


def _read_server_clock(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["at"] = r.int()
            case 2:
                read["advance"] = r.uint()
            case _:
                r.skip()
    r.leave()
    return read


# A private server's clock: a time to set it to, a span to move it forward by,
# or neither to read it. The answer is the time it reads once moved.
ServerClock = Message(
    "server.Clock",
    {
        "at": (1, int_),
        "advance": (2, uint),
    },
    _write_server_clock,
    _read_server_clock,
)


def _write_sql_migration(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("name")) is not None:
        w.uint(1)
        w.str(x)
        n += 1
    if (x := v.get("sql")) is not None:
        w.uint(2)
        w.str(x)
        n += 1
    w.close_map(head, n)


def _read_sql_migration(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["name"] = r.str()
            case 2:
                read["sql"] = r.str()
            case _:
                r.skip()
    r.leave()
    return read


# A migration as its file has it: the name that numbers it, and its SQL.
SqlMigration = Message(
    "sql.Migration",
    {
        # 0001_notes.sql
        "name": (1, str_),
        "sql": (2, str_),
    },
    _write_sql_migration,
    _read_sql_migration,
)


def _write_sql_open(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("name")) is not None:
        w.uint(1)
        w.str(x)
        n += 1
    if (x := v.get("migrations")) is not None:
        w.uint(2)
        w.array(len(x))
        for item0 in x:
            SqlMigration.write(w, item0)
        n += 1
    if (x := v.get("durability")) is not None:
        w.uint(3)
        w.str(x)
        n += 1
    w.close_map(head, n)


def _read_sql_open(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["name"] = r.str()
            case 2:
                items0 = [SqlMigration.read(r) for _ in range(r.array())]
                r.leave()
                read["migrations"] = items0
            case 3:
                read["durability"] = r.str()
            case _:
                r.skip()
    r.leave()
    return read


# Opens a database. Migrations, when given, are applied and checked; without
# them the file opens as it is.
SqlOpen = Message(
    "sql.Open",
    {
        # [a-z0-9][a-z0-9_-]{0,63}
        "name": (1, str_),
        "migrations": (2, list_(SqlMigration)),
        # full or os: how far its commits go before they return; the store's own when absent
        "durability": (3, str_),
    },
    _write_sql_open,
    _read_sql_open,
)


def _write_sql_text(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("text")) is not None:
        w.uint(1)
        w.str(x)
        n += 1
    if (x := v.get("values")) is not None:
        w.uint(2)
        w.array(len(x))
        for item0 in x:
            sql_value.write(w, item0)
        n += 1
    if (x := v.get("method")) is not None:
        w.uint(3)
        w.uint(x)
        n += 1
    if (x := v.get("body")) is not None:
        w.uint(4)
        w.bin(x)
        n += 1
    w.close_map(head, n)


def _read_sql_text(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["text"] = r.str()
            case 2:
                items0 = [sql_value.read(r) for _ in range(r.array())]
                r.leave()
                read["values"] = items0
            case 3:
                read["method"] = r.uint()
            case 4:
                read["body"] = r.bin()
            case _:
                r.skip()
    r.leave()
    return read


# A statement's text and the values of its ?s, in order. In a batch it may
# be a call of jobs on a queue kept in the database's file instead, jobs.add
# with its request in body and no text, which commits with the batch's rows.
SqlText = Message(
    "sql.Text",
    {
        "text": (1, str_),
        "values": (2, list_(sql_value)),
        # in a batch, jobs.add: the call in body
        "method": (3, uint),
        "body": (4, bin_),
    },
    _write_sql_text,
    _read_sql_text,
)


def _write_sql_statement(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("handle")) is not None:
        w.uint(1)
        w.uint(x)
        n += 1
    if (x := v.get("text")) is not None:
        w.uint(2)
        w.str(x)
        n += 1
    if (x := v.get("values")) is not None:
        w.uint(3)
        w.array(len(x))
        for item0 in x:
            sql_value.write(w, item0)
        n += 1
    w.close_map(head, n)


def _read_sql_statement(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["handle"] = r.uint()
            case 2:
                read["text"] = r.str()
            case 3:
                items0 = [sql_value.read(r) for _ in range(r.array())]
                r.leave()
                read["values"] = items0
            case _:
                r.skip()
    r.leave()
    return read


# A statement on a database: a write, or a read answered by its columns.
SqlStatement = Message(
    "sql.Statement",
    {
        "handle": (1, uint),
        "text": (2, str_),
        "values": (3, list_(sql_value)),
    },
    _write_sql_statement,
    _read_sql_statement,
)


def _write_sql_query(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("handle")) is not None:
        w.uint(1)
        w.uint(x)
        n += 1
    if (x := v.get("text")) is not None:
        w.uint(2)
        w.str(x)
        n += 1
    if (x := v.get("values")) is not None:
        w.uint(3)
        w.array(len(x))
        for item0 in x:
            sql_value.write(w, item0)
        n += 1
    if (x := v.get("want")) is not None:
        w.uint(4)
        w.str(x)
        n += 1
    w.close_map(head, n)


def _read_sql_query(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["handle"] = r.uint()
            case 2:
                read["text"] = r.str()
            case 3:
                items0 = [sql_value.read(r) for _ in range(r.array())]
                r.leave()
                read["values"] = items0
            case 4:
                read["want"] = r.str()
            case _:
                r.skip()
    r.leave()
    return read


# What a statement gives back: its rows, one row or none, or one value. A
# write with returning runs on the writer, its rows given once it is durable.
SqlQuery = Message(
    "sql.Query",
    {
        "handle": (1, uint),
        "text": (2, str_),
        "values": (3, list_(sql_value)),
        # all, one or scalar
        "want": (4, str_),
    },
    _write_sql_query,
    _read_sql_query,
)


def _write_sql_rows(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("columns")) is not None:
        w.uint(1)
        w.array(len(x))
        for item0 in x:
            w.str(item0)
        n += 1
    if (x := v.get("rows")) is not None:
        w.uint(2)
        w.array(len(x))
        for item0 in x:
            w.array(len(item0))
            for item1 in item0:
                sql_value.write(w, item1)
        n += 1
    w.close_map(head, n)


def _read_sql_rows(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                items0 = [r.str() for _ in range(r.array())]
                r.leave()
                read["columns"] = items0
            case 2:
                items0: list[Any] = []
                for _ in range(r.array()):
                    items1 = [sql_value.read(r) for _ in range(r.array())]
                    r.leave()
                    items0.append(items1)
                r.leave()
                read["rows"] = items0
            case _:
                r.skip()
    r.leave()
    return read


# Rows a statement gave, a part of them a message: the first part names the
# columns, and every row holds a value a column, in their order.
SqlRows = Message(
    "sql.Rows",
    {
        "columns": (1, list_(str_)),
        "rows": (2, list_(list_(sql_value))),
    },
    _write_sql_rows,
    _read_sql_rows,
)


def _write_sql_column(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("name")) is not None:
        w.uint(1)
        w.str(x)
        n += 1
    if (x := v.get("integers")) is not None:
        w.uint(2)
        w.bin(x)
        n += 1
    if (x := v.get("reals")) is not None:
        w.uint(3)
        w.bin(x)
        n += 1
    if (x := v.get("nulls")) is not None:
        w.uint(4)
        w.bin(x)
        n += 1
    if (x := v.get("values")) is not None:
        w.uint(5)
        w.array(len(x))
        for item0 in x:
            sql_value.write(w, item0)
        n += 1
    w.close_map(head, n)


def _read_sql_column(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["name"] = r.str()
            case 2:
                read["integers"] = r.bin()
            case 3:
                read["reals"] = r.bin()
            case 4:
                read["nulls"] = r.bin()
            case 5:
                items0 = [sql_value.read(r) for _ in range(r.array())]
                r.leave()
                read["values"] = items0
            case _:
                r.skip()
    r.leave()
    return read


# A column of a statement's rows, a part of it a message, as one of three. A
# column of INTEGERs alone goes as integers and one of REALs alone as reals,
# 8 bytes a row, little end first; where a row holds NULL its bit of nulls is
# set, and its value is 0 among integers and a NaN among reals, which SQLite
# keeps none of. Any other column goes as values: text, blobs, and INTEGERs
# beside REALs, neither read as the other.
SqlColumn = Message(
    "sql.Column",
    {
        "name": (1, str_),
        # an int64 a row
        "integers": (2, bin_),
        # a float64 a row
        "reals": (3, bin_),
        # a bit a row, a byte's lowest first, set where the row holds NULL; absent when none does
        "nulls": (4, bin_),
        "values": (5, list_(sql_value)),
    },
    _write_sql_column,
    _read_sql_column,
)


def _write_sql_columns(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("rows")) is not None:
        w.uint(1)
        w.uint(x)
        n += 1
    if (x := v.get("total")) is not None:
        w.uint(2)
        w.uint(x)
        n += 1
    if (x := v.get("columns")) is not None:
        w.uint(3)
        w.array(len(x))
        for item0 in x:
            SqlColumn.write(w, item0)
        n += 1
    w.close_map(head, n)


def _read_sql_columns(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["rows"] = r.uint()
            case 2:
                read["total"] = r.uint()
            case 3:
                items0 = [SqlColumn.read(r) for _ in range(r.array())]
                r.leave()
                read["columns"] = items0
            case _:
                r.skip()
    r.leave()
    return read


# Rows a statement gave, by their columns, a part of them a message: every
# part holds every column, in the statement's order and as the same one of
# three, for the same rows.
SqlColumns = Message(
    "sql.Columns",
    {
        # the rows of this part
        "rows": (1, uint),
        # the rows of every part
        "total": (2, uint),
        "columns": (3, list_(SqlColumn)),
    },
    _write_sql_columns,
    _read_sql_columns,
)


def _write_sql_done(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("changes")) is not None:
        w.uint(1)
        w.uint(x)
        n += 1
    if (x := v.get("last_insert_rowid")) is not None:
        w.uint(2)
        w.int(x)
        n += 1
    w.close_map(head, n)


def _read_sql_done(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["changes"] = r.uint()
            case 2:
                read["last_insert_rowid"] = r.int()
            case _:
                r.skip()
    r.leave()
    return read


# What a write changed: the rows, and SQLite's rowid of the row it inserted,
# 0 when it inserted none.
SqlDone = Message(
    "sql.Done",
    {
        "changes": (1, uint),
        "last_insert_rowid": (2, int_),
    },
    _write_sql_done,
    _read_sql_done,
)


def _write_sql_batch(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("handle")) is not None:
        w.uint(1)
        w.uint(x)
        n += 1
    if (x := v.get("statements")) is not None:
        w.uint(2)
        w.array(len(x))
        for item0 in x:
            SqlText.write(w, item0)
        n += 1
    w.close_map(head, n)


def _read_sql_batch(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["handle"] = r.uint()
            case 2:
                items0 = [SqlText.read(r) for _ in range(r.array())]
                r.leave()
                read["statements"] = items0
            case _:
                r.skip()
    r.leave()
    return read


# Writes known before they run, written as one in a shared commit: its
# statements, and the adds of a queue kept in the database's file.
SqlBatch = Message(
    "sql.Batch",
    {
        "handle": (1, uint),
        "statements": (2, list_(SqlText)),
    },
    _write_sql_batch,
    _read_sql_batch,
)


def _write_sql_batched(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("done")) is not None:
        w.uint(1)
        w.array(len(x))
        for item0 in x:
            SqlDone.write(w, item0)
        n += 1
    w.close_map(head, n)


def _read_sql_batched(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                items0 = [SqlDone.read(r) for _ in range(r.array())]
                r.leave()
                read["done"] = items0
            case _:
                r.skip()
    r.leave()
    return read


# What each write of a batch did, in their order: a job's add changed one
# job, or none when its id was taken.
SqlBatched = Message(
    "sql.Batched",
    {
        "done": (1, list_(SqlDone)),
    },
    _write_sql_batched,
    _read_sql_batched,
)


def _write_sql_tx_open(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("handle")) is not None:
        w.uint(1)
        w.uint(x)
        n += 1
    w.close_map(head, n)


def _read_sql_tx_open(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["handle"] = r.uint()
            case _:
                r.skip()
    r.leave()
    return read


# Opens a transaction: it holds the database's writer until the client's last
# DATA, five seconds at most.
SqlTxOpen = Message(
    "sql.TxOpen",
    {
        "handle": (1, uint),
    },
    _write_sql_tx_open,
    _read_sql_tx_open,
)


def _write_sql_tx_call(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("text")) is not None:
        w.uint(1)
        w.str(x)
        n += 1
    if (x := v.get("values")) is not None:
        w.uint(2)
        w.array(len(x))
        for item0 in x:
            sql_value.write(w, item0)
        n += 1
    if (x := v.get("want")) is not None:
        w.uint(3)
        w.str(x)
        n += 1
    if (x := v.get("commit")) is not None:
        w.uint(4)
        w.bool(x)
        n += 1
    if (x := v.get("method")) is not None:
        w.uint(5)
        w.uint(x)
        n += 1
    if (x := v.get("body")) is not None:
        w.uint(6)
        w.bin(x)
        n += 1
    w.close_map(head, n)


def _read_sql_tx_call(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["text"] = r.str()
            case 2:
                items0 = [sql_value.read(r) for _ in range(r.array())]
                r.leave()
                read["values"] = items0
            case 3:
                read["want"] = r.str()
            case 4:
                read["commit"] = r.bool()
            case 5:
                read["method"] = r.uint()
            case 6:
                read["body"] = r.bin()
            case _:
                r.skip()
    r.leave()
    return read


# A transaction's call, or its end: the last DATA commits, or not. A call is
# a statement, or a call of kv or jobs on a bucket or a queue kept in the
# database's file: a method of theirs with its request, kv.set or jobs.add,
# which runs in the transaction.
SqlTxCall = Message(
    "sql.TxCall",
    {
        "text": (1, str_),
        "values": (2, list_(sql_value)),
        # all, one, scalar or exec; call for a method; nothing in the last
        "want": (3, str_),
        # in the last: true commits, false rolls back
        "commit": (4, bool_),
        # with want call: the method, its request in body
        "method": (5, uint),
        "body": (6, bin_),
    },
    _write_sql_tx_call,
    _read_sql_tx_call,
)


def _write_sql_tx_answer(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("rows")) is not None:
        w.uint(1)
        SqlRows.write(w, x)
        n += 1
    if (x := v.get("done")) is not None:
        w.uint(2)
        SqlDone.write(w, x)
        n += 1
    if (x := v.get("failure")) is not None:
        w.uint(3)
        Failure.write(w, x)
        n += 1
    if (x := v.get("body")) is not None:
        w.uint(4)
        w.bin(x)
        n += 1
    w.close_map(head, n)


def _read_sql_tx_answer(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["rows"] = SqlRows.read(r)
            case 2:
                read["done"] = SqlDone.read(r)
            case 3:
                read["failure"] = Failure.read(r)
            case 4:
                read["body"] = r.bin()
            case _:
                r.skip()
    r.leave()
    return read


# A call's answer: its rows, what it changed, a method's answer, or why it
# failed, which leaves the transaction as it was before the call.
SqlTxAnswer = Message(
    "sql.TxAnswer",
    {
        "rows": (1, SqlRows),
        "done": (2, SqlDone),
        "failure": (3, Failure),
        # what a method answered, as its own answer
        "body": (4, bin_),
    },
    _write_sql_tx_answer,
    _read_sql_tx_answer,
)


def _write_channels_batch(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("missed")) is not None:
        w.uint(1)
        w.uint(x)
        n += 1
    if (x := v.get("messages")) is not None:
        w.uint(2)
        w.array(len(x))
        for item0 in x:
            ChannelsMessage.write(w, item0)
        n += 1
    w.close_map(head, n)


def _read_channels_batch(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["missed"] = r.uint()
            case 2:
                items0 = [ChannelsMessage.read(r) for _ in range(r.array())]
                r.leave()
                read["messages"] = items0
            case _:
                r.skip()
    r.leave()
    return read


# What a subscription receives at once: how many it missed since its last
# batch, then messages in the order they were released.
ChannelsBatch = Message(
    "channels.Batch",
    {
        # lost because it fell behind its branch's log
        "missed": (1, uint),
        "messages": (2, list_(ChannelsMessage)),
    },
    _write_channels_batch,
    _read_channels_batch,
)


def _write_config_given(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("value")) is not None:
        w.uint(1)
        w.str(x)
        n += 1
    if (x := v.get("from")) is not None:
        w.uint(2)
        ConfigOrigin.write(w, x)
        n += 1
    w.close_map(head, n)


def _read_config_given(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["value"] = r.str()
            case 2:
                read["from"] = ConfigOrigin.read(r)
            case _:
                r.skip()
    r.leave()
    return read


# A value a setting was given, and where it came from.
ConfigGiven = Message(
    "config.Given",
    {
        # as JSON, a span spelled, a secret as ***
        "value": (1, str_),
        "from": (2, ConfigOrigin),
    },
    _write_config_given,
    _read_config_given,
)


def _write_metrics_plan(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("series")) is not None:
        w.uint(1)
        w.uint(x)
        n += 1
    if (x := v.get("bytes")) is not None:
        w.uint(2)
        w.uint(x)
        n += 1
    if (x := v.get("decoded")) is not None:
        w.uint(3)
        w.uint(x)
        n += 1
    if (x := v.get("exceeds")) is not None:
        w.uint(4)
        MetricsExceeds.write(w, x)
        n += 1
    w.close_map(head, n)


def _read_metrics_plan(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["series"] = r.uint()
            case 2:
                read["bytes"] = r.uint()
            case 3:
                read["decoded"] = r.uint()
            case 4:
                read["exceeds"] = MetricsExceeds.read(r)
            case _:
                r.skip()
    r.leave()
    return read


# What a call would spend of each bound, and the bound it would stop at.
MetricsPlan = Message(
    "metrics.Plan",
    {
        "series": (1, uint),
        "bytes": (2, uint),
        "decoded": (3, uint),
        "exceeds": (4, MetricsExceeds),
    },
    _write_metrics_plan,
    _read_metrics_plan,
)


def _write_search_open(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("name")) is not None:
        w.uint(1)
        w.str(x)
        n += 1
    if (x := v.get("fields")) is not None:
        w.uint(2)
        w.array(len(x))
        for item0 in x:
            SearchField.write(w, item0)
        n += 1
    if (x := v.get("filters")) is not None:
        w.uint(3)
        w.array(len(x))
        for item0 in x:
            w.str(item0)
        n += 1
    if (x := v.get("typo_tolerance")) is not None:
        w.uint(4)
        w.bool(x)
        n += 1
    if (x := v.get("dimensions")) is not None:
        w.uint(5)
        w.uint(x)
        n += 1
    if (x := v.get("recall")) is not None:
        w.uint(6)
        w.float(x)
        n += 1
    if (x := v.get("vector_weight")) is not None:
        w.uint(7)
        w.float(x)
        n += 1
    if (x := v.get("rerank")) is not None:
        w.uint(8)
        w.bool(x)
        n += 1
    if (x := v.get("manual")) is not None:
        w.uint(9)
        w.bool(x)
        n += 1
    if (x := v.get("reindex")) is not None:
        w.uint(10)
        w.bool(x)
        n += 1
    if (x := v.get("database")) is not None:
        w.uint(11)
        w.uint(x)
        n += 1
    if (x := v.get("table")) is not None:
        w.uint(12)
        w.str(x)
        n += 1
    if (x := v.get("key")) is not None:
        w.uint(13)
        w.str(x)
        n += 1
    if (x := v.get("vector_column")) is not None:
        w.uint(14)
        w.str(x)
        n += 1
    w.close_map(head, n)


def _read_search_open(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["name"] = r.str()
            case 2:
                items0 = [SearchField.read(r) for _ in range(r.array())]
                r.leave()
                read["fields"] = items0
            case 3:
                items0 = [r.str() for _ in range(r.array())]
                r.leave()
                read["filters"] = items0
            case 4:
                read["typo_tolerance"] = r.bool()
            case 5:
                read["dimensions"] = r.uint()
            case 6:
                read["recall"] = r.float()
            case 7:
                read["vector_weight"] = r.float()
            case 8:
                read["rerank"] = r.bool()
            case 9:
                read["manual"] = r.bool()
            case 10:
                read["reindex"] = r.bool()
            case 11:
                read["database"] = r.uint()
            case 12:
                read["table"] = r.str()
            case 13:
                read["key"] = r.str()
            case 14:
                read["vector_column"] = r.str()
            case _:
                r.skip()
    r.leave()
    return read


# Opens an index, made the first time; one that holds other settings is
# changed to these, as docs/guides/search/rebuilds.md has it. A read
# connection opens an index only as it is. With a database, the index
# follows a table of it: its fields and filters are columns.
SearchOpen = Message(
    "search.Open",
    {
        # [a-z0-9][a-z0-9_-]{0,63}
        "name": (1, str_),
        # in order; none for a document that is its own text
        "fields": (2, list_(SearchField)),
        # the fields a search may filter by
        "filters": (3, list_(str_)),
        # on when absent
        "typo_tolerance": (4, bool_),
        # the vectors' dimensions; none for an index without vectors
        "dimensions": (5, uint),
        # above 0 and below 1; exact when absent
        "recall": (6, float_),
        # 0.5 when absent
        "vector_weight": (7, float_),
        "rerank": (8, bool_),
        # a change that needs reading again waits for reindex
        "manual": (9, bool_),
        # reads the index again beside itself
        "reindex": (10, bool_),
        # a database's handle: the index follows a table in its file
        "database": (11, uint),
        # the table a database's index follows
        "table": (12, str_),
        # the table's column of unique values; its primary key when absent
        "key": (13, str_),
        # the column of each row's vector, a blob of little-endian floats
        "vector_column": (14, str_),
    },
    _write_search_open,
    _read_search_open,
)


def _write_search_set_many(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("handle")) is not None:
        w.uint(1)
        w.uint(x)
        n += 1
    if (x := v.get("documents")) is not None:
        w.uint(2)
        w.array(len(x))
        for item0 in x:
            SearchDocument.write(w, item0)
        n += 1
    w.close_map(head, n)


def _read_search_set_many(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["handle"] = r.uint()
            case 2:
                items0 = [SearchDocument.read(r) for _ in range(r.array())]
                r.leave()
                read["documents"] = items0
            case _:
                r.skip()
    r.leave()
    return read


# Documents set in one commit, all of them or none: 1,000 and 16 MiB at most.
SearchSetMany = Message(
    "search.SetMany",
    {
        "handle": (1, uint),
        "documents": (2, list_(SearchDocument)),
    },
    _write_search_set_many,
    _read_search_set_many,
)


def _write_config_line(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("path")) is not None:
        w.uint(1)
        w.str(x)
        n += 1
    if (x := v.get("value")) is not None:
        w.uint(2)
        w.str(x)
        n += 1
    if (x := v.get("from")) is not None:
        w.uint(3)
        w.str(x)
        n += 1
    if (x := v.get("over")) is not None:
        w.uint(4)
        w.array(len(x))
        for item0 in x:
            w.str(item0)
        n += 1
    if (x := v.get("given")) is not None:
        w.uint(5)
        w.array(len(x))
        for item0 in x:
            ConfigGiven.write(w, item0)
        n += 1
    w.close_map(head, n)


def _read_config_line(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["path"] = r.str()
            case 2:
                read["value"] = r.str()
            case 3:
                read["from"] = r.str()
            case 4:
                items0 = [r.str() for _ in range(r.array())]
                r.leave()
                read["over"] = items0
            case 5:
                items0 = [ConfigGiven.read(r) for _ in range(r.array())]
                r.leave()
                read["given"] = items0
            case _:
                r.skip()
    r.leave()
    return read


# A setting's line of explain.
ConfigLine = Message(
    "config.Line",
    {
        "path": (1, str_),
        # as JSON, a span spelled, a secret as ***
        "value": (2, str_),
        "from": (3, str_),
        # the values it went over, newest first
        "over": (4, list_(str_)),
        # every value it was given, newest first: its own, then the ones it went over
        "given": (5, list_(ConfigGiven)),
    },
    _write_config_line,
    _read_config_line,
)


def _write_config_value(w: Writer, v: Mapping[str, Any]) -> None:
    head = w.open_map()
    n = 0
    if (x := v.get("handle")) is not None:
        w.uint(1)
        w.uint(x)
        n += 1
    if (x := v.get("version")) is not None:
        w.uint(2)
        w.int(x)
        n += 1
    if (x := v.get("value")) is not None:
        w.uint(3)
        w.str(x)
        n += 1
    if (x := v.get("lines")) is not None:
        w.uint(4)
        w.array(len(x))
        for item0 in x:
            ConfigLine.write(w, item0)
        n += 1
    if (x := v.get("notes")) is not None:
        w.uint(5)
        w.array(len(x))
        for item0 in x:
            w.str(item0)
        n += 1
    w.close_map(head, n)


def _read_config_value(r: Reader) -> dict[str, Any]:
    read: dict[str, Any] = {}
    seen: set[int] = set()
    for _ in range(r.map()):
        match r.field(seen):
            case 1:
                read["handle"] = r.uint()
            case 2:
                read["version"] = r.int()
            case 3:
                read["value"] = r.str()
            case 4:
                items0 = [ConfigLine.read(r) for _ in range(r.array())]
                r.leave()
                read["lines"] = items0
            case 5:
                items0 = [r.str() for _ in range(r.array())]
                r.leave()
                read["notes"] = items0
            case _:
                r.skip()
    r.leave()
    return read


# A config's value at a version of its changes, and where each setting's
# value came from.
ConfigValue = Message(
    "config.Value",
    {
        "handle": (1, uint),
        "version": (2, int_),
        # the settings, each span its milliseconds, each secret its text
        "value": (3, str_),
        "lines": (4, list_(ConfigLine)),
        # what was read and not used, each with why
        "notes": (5, list_(str_)),
    },
    _write_config_value,
    _read_config_value,
)


messages: dict[str, Message] = {
    "backup.Write": BackupWrite,
    "backup.Piece": BackupPiece,
    "backup.Written": BackupWritten,
    "blobs.Open": BlobsOpen,
    "blobs.At": BlobsAt,
    "blobs.Info": BlobsInfo,
    "blobs.Write": BlobsWrite,
    "blobs.Written": BlobsWritten,
    "blobs.Piece": BlobsPiece,
    "blobs.Got": BlobsGot,
    "blobs.Read": BlobsRead,
    "blobs.Head": BlobsHead,
    "blobs.Delete": BlobsDelete,
    "blobs.Move": BlobsMove,
    "blobs.Expire": BlobsExpire,
    "blobs.Found": BlobsFound,
    "blobs.List": BlobsList,
    "blobs.Page": BlobsPage,
    "blobs.Folder": BlobsFolder,
    "blobs.Usage": BlobsUsage,
    "channels.Publish": ChannelsPublish,
    "channels.Subscribe": ChannelsSubscribe,
    "channels.Message": ChannelsMessage,
    "config.Open": ConfigOpen,
    "config.Origin": ConfigOrigin,
    "config.Update": ConfigUpdate,
    "config.Reset": ConfigReset,
    "Hello": Hello,
    "Welcome": Welcome,
    "store.Options": StoreOptions,
    "GoAway": GoAway,
    "Failure": Failure,
    "Handle": Handle,
    "Empty": Empty,
    "jobs.Concurrency": JobsConcurrency,
    "jobs.Backoff": JobsBackoff,
    "jobs.Rate": JobsRate,
    "jobs.QueueOpen": JobsQueueOpen,
    "jobs.ScheduleOpen": JobsScheduleOpen,
    "jobs.Call": JobsCall,
    "jobs.Id": JobsId,
    "jobs.Changed": JobsChanged,
    "jobs.Job": JobsJob,
    "jobs.List": JobsList,
    "jobs.Page": JobsPage,
    "jobs.Work": JobsWork,
    "jobs.Held": JobsHeld,
    "jobs.Answer": JobsAnswer,
    "jobs.Step": JobsStep,
    "jobs.Kept": JobsKept,
    "jobs.Op": JobsOp,
    "jobs.Tx": JobsTx,
    "jobs.TxResults": JobsTxResults,
    "kv.BucketOpen": KvBucketOpen,
    "kv.Call": KvCall,
    "kv.Entry": KvEntry,
    "kv.Written": KvWritten,
    "kv.Found": KvFound,
    "kv.Resealed": KvResealed,
    "kv.Branch": KvBranch,
    "kv.List": KvList,
    "kv.Page": KvPage,
    "kv.CountersOpen": KvCountersOpen,
    "kv.Count": KvCount,
    "kv.RateLimitOpen": KvRateLimitOpen,
    "kv.Window": KvWindow,
    "kv.QuotaOpen": KvQuotaOpen,
    "kv.WindowUse": KvWindowUse,
    "kv.Allowance": KvAllowance,
    "kv.OnceOpen": KvOnceOpen,
    "kv.Answer": KvAnswer,
    "kv.Check": KvCheck,
    "kv.Op": KvOp,
    "kv.Tx": KvTx,
    "kv.Outcome": KvOutcome,
    "kv.TxResults": KvTxResults,
    "locks.Hold": LocksHold,
    "locks.Held": LocksHeld,
    "locks.Renew": LocksRenew,
    "locks.Lease": LocksLease,
    "metrics.Condition": MetricsCondition,
    "metrics.Query": MetricsQuery,
    "metrics.Series": MetricsSeries,
    "metrics.Read": MetricsRead,
    "metrics.Newest": MetricsNewest,
    "metrics.Latest": MetricsLatest,
    "metrics.AggregateAsked": MetricsAggregateAsked,
    "metrics.Bucket": MetricsBucket,
    "metrics.Group": MetricsGroup,
    "metrics.Aggregates": MetricsAggregates,
    "metrics.Exceeds": MetricsExceeds,
    "metrics.NewSeries": MetricsNewSeries,
    "metrics.Ingest": MetricsIngest,
    "metrics.Exposition": MetricsExposition,
    "metrics.Upload": MetricsUpload,
    "metrics.Piece": MetricsPiece,
    "metrics.PageAsked": MetricsPageAsked,
    "metrics.Name": MetricsName,
    "metrics.NamePage": MetricsNamePage,
    "metrics.TextPage": MetricsTextPage,
    "metrics.Description": MetricsDescription,
    "metrics.NameAsked": MetricsNameAsked,
    "metrics.SeriesNamed": MetricsSeriesNamed,
    "metrics.Deleted": MetricsDeleted,
    "metrics.Damage": MetricsDamage,
    "metrics.Damages": MetricsDamages,
    "metrics.Session": MetricsSession,
    "metrics.Instrument": MetricsInstrument,
    "metrics.Set": MetricsSet,
    "metrics.Change": MetricsChange,
    "metrics.Batch": MetricsBatch,
    "metrics.Refused": MetricsRefused,
    "metrics.Ack": MetricsAck,
    "records.Open": RecordsOpen,
    "records.NewEvent": RecordsNewEvent,
    "records.Append": RecordsAppend,
    "records.Appended": RecordsAppended,
    "records.Event": RecordsEvent,
    "records.Query": RecordsQuery,
    "records.Page": RecordsPage,
    "records.Count": RecordsCount,
    "records.TopAsked": RecordsTopAsked,
    "records.Top": RecordsTop,
    "records.Tops": RecordsTops,
    "records.AggregateAsked": RecordsAggregateAsked,
    "records.Stats": RecordsStats,
    "records.Row": RecordsRow,
    "records.Rows": RecordsRows,
    "records.Stream": RecordsStream,
    "records.Streams": RecordsStreams,
    "records.FieldNamesAsked": RecordsFieldNamesAsked,
    "records.Names": RecordsNames,
    "records.PollAsked": RecordsPollAsked,
    "records.Batch": RecordsBatch,
    "records.TailBatch": RecordsTailBatch,
    "records.Piece": RecordsPiece,
    "records.Dropped": RecordsDropped,
    "records.Damage": RecordsDamage,
    "records.Damages": RecordsDamages,
    "search.Field": SearchField,
    "search.Set": SearchSet,
    "search.Document": SearchDocument,
    "search.Key": SearchKey,
    "search.Deleted": SearchDeleted,
    "search.Got": SearchGot,
    "search.Query": SearchQuery,
    "search.Hit": SearchHit,
    "search.Found": SearchFound,
    "search.Status": SearchStatus,
    "search.Name": SearchName,
    "search.Dropped": SearchDropped,
    "search.Index": SearchIndex,
    "search.Listed": SearchListed,
    "server.Clock": ServerClock,
    "sql.Migration": SqlMigration,
    "sql.Open": SqlOpen,
    "sql.Text": SqlText,
    "sql.Statement": SqlStatement,
    "sql.Query": SqlQuery,
    "sql.Rows": SqlRows,
    "sql.Column": SqlColumn,
    "sql.Columns": SqlColumns,
    "sql.Done": SqlDone,
    "sql.Batch": SqlBatch,
    "sql.Batched": SqlBatched,
    "sql.TxOpen": SqlTxOpen,
    "sql.TxCall": SqlTxCall,
    "sql.TxAnswer": SqlTxAnswer,
    "channels.Batch": ChannelsBatch,
    "config.Given": ConfigGiven,
    "metrics.Plan": MetricsPlan,
    "search.Open": SearchOpen,
    "search.SetMany": SearchSetMany,
    "config.Line": ConfigLine,
    "config.Value": ConfigValue,
}
"""Every message by its name in the schema."""
