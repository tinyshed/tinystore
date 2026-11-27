"""A table that mirrors another system: it takes the source's list and says what changed.

The server compares the list with what the table holds by the source's key
and writes only the difference, in one commit; this module only says the
call in Python, and cuts a list past one message into the parts of a run.
"""

from __future__ import annotations

import contextlib
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any

from ._connection import download
from ._rows import SqlArg, cell_of, field_of, fields_of, reader, row_of
from ._wire.protocol import SqlAdd, SqlBegin, SqlDiff, SqlMirror, SqlRun, methods
from .bounds import ANSWER_BYTES, REQUEST_BYTES
from .errors import InvalidError, LimitError

if TYPE_CHECKING:
    from collections.abc import Awaitable, Callable, Mapping, Sequence
    from types import TracebackType

    from ._connection import Connection
    from .sql import Mark

    MirrorCall = Callable[[Callable[[int], bytes]], Awaitable[bytes]]
    """What sends a mirror's request on the database's connection and gives back its answer."""

    MirrorBegin = Callable[[Callable[[int], bytes]], Awaitable[tuple[int, Connection]]]
    """What sends a run's begin with the database's handle, and gives the run's handle and the connection it is on."""


@dataclass(frozen=True, slots=True)
class Changed[T]:
    """A row that changed: as the table held it, and as the source gave it."""

    before: T
    after: T


@dataclass(frozen=True, slots=True)
class Diff[T]:
    """What a mirror's call changed; a suspect answer wrote nothing."""

    added: list[T]
    changed: list[Changed[T]]
    gone: list[T]
    same: int
    suspect: bool


class Mirror[T]:
    """A table that mirrors another system, from ``db.mirror``.

    Its ``replace`` takes the source's list of a scope, its ``upsert`` a change
    of some rows, and each answers what was added, changed and gone. A row the
    list gives as it is held is not written at all.

        works = db.mirror("works", Work, key=["course_id", "id"], gone=sql.Mark("gone_at", unit="s"))
        diff = await works.replace({"course_id": 1}, fetched)
    """

    def __init__(
        self,
        table: str,
        of: Any,
        options: tuple[list[str], Mark | None, list[str] | None],
        call: MirrorCall,
        begin: MirrorBegin,
    ) -> None:
        self.table = table
        self._read: Callable[[dict[str, Any]], T] = reader(of)
        key, gone, change_on = options
        self._shape = {
            "table": table,
            "key": key,
            "gone": None if gone is None else gone.column,
            # milliseconds go unnamed, which a server older than the unit reads too
            "gone_unit": "s" if gone is not None and gone.unit == "s" else None,
            "change_on": change_on,
        }
        self._call = call
        self._begin = begin

    async def replace(
        self, scope: Mapping[str, Any], rows: Sequence[T | Mapping[str, Any]], *, min_kept: float | None = None
    ) -> Diff[T]:
        """Makes the scope, the rows whose columns hold its values, what ``rows`` lists.

        ``{}`` is the whole table. It adds what the table lacks, writes what
        changed, and deletes, or marks gone, what the list leaves out; a row
        outside the scope is InvalidError. With ``min_kept``, a list that would
        leave fewer than that fraction of the scope's live rows live writes
        nothing and answers ``suspect``. Values compare as the table keeps
        them: ``4`` given to a text column is ``"4"``, and a key's text
        compares by its unique index's collation; a key holding None is
        InvalidError. A list, or an answer, past one message goes in the
        parts of a run, as ``begin`` makes one, and writes in one commit all
        the same.
        """
        try:
            return await self._run(rows, mode="replace", **_scope_of(scope), min_kept=min_kept)
        except LimitError as err:
            if err.limit not in (REQUEST_BYTES.name, ANSWER_BYTES.name):
                raise
        # refused before it was written: the same list, in parts over one run
        async with self.begin(scope, min_kept=min_kept) as run:
            await run.add(rows)
            return await run.commit()

    async def upsert(self, rows: Sequence[T | Mapping[str, Any]]) -> Diff[T]:
        """Writes what ``rows`` adds and changes, a delta of the source's; its ``gone`` is always empty."""
        return await self._run(rows, mode="upsert")

    def begin(self, scope: Mapping[str, Any], *, min_kept: float | None = None) -> MirrorRun[T]:
        """Begins a ``replace`` of the scope whose list comes in parts.

        Each ``add`` gives a part, and ``commit`` makes the scope what every
        part listed, in one write, so that only what no part named is gone. A
        run never committed writes nothing: closed, dropped, idle for five
        minutes, its connection lost or the store closed. Nothing is sent
        until the first ``add`` or ``commit``.

            async with works.begin({"course_id": 1}) as run:
                async for page in pages: await run.add(page)
                diff = await run.commit()
        """
        asked = {**self._shape, **_scope_of(scope), "min_kept": min_kept}

        async def begun() -> tuple[int, Connection]:
            return await self._begin(lambda handle: SqlBegin.encode(handle=handle, **asked))

        return MirrorRun(begun, self._read)

    async def _run(self, rows: Sequence[T | Mapping[str, Any]], **asked: Any) -> Diff[T]:
        listed = _list_of(rows)
        given = {name: value for name, value in asked.items() if value is not None}

        def body(handle: int) -> bytes:
            return SqlMirror.encode(handle=handle, **self._shape, rows=listed, **given)

        return _diff_of([await self._call(body)], self._read)


class MirrorRun[T]:
    """A mirror's ``replace`` whose list comes in parts, from ``mirror.begin``.

    Its parts wait on the server, outside the table, until ``commit``; one
    never committed writes nothing, and ``close``, or the end of an ``async
    with``, lets go of it at once. Every call of a run goes on the connection
    it began on, since the server keeps the parts there.
    """

    def __init__(
        self, begin: Callable[[], Awaitable[tuple[int, Connection]]], read: Callable[[dict[str, Any]], T]
    ) -> None:
        self._begin = begin
        self._read = read
        self._begun: tuple[int, Connection] | None = None
        self._ended = False

    async def __aenter__(self) -> MirrorRun[T]:
        return self

    async def __aexit__(
        self, kind: type[BaseException] | None, error: BaseException | None, traceback: TracebackType | None
    ) -> None:
        await self.close()

    async def add(self, rows: Sequence[T | Mapping[str, Any]]) -> None:
        """Gives a part of the list: rows of the first part's shape, inside the scope, each key once in the whole run.

        A part past one message goes as several. A part that fails ends the run.
        """
        self._refuse_ended()
        run, connection = await self._started()
        await self._send(run, connection, rows)

    async def commit(self) -> Diff[T]:
        """Makes the scope what every part listed, in one write, and answers what changed."""
        self._refuse_ended()
        self._ended = True
        run, connection = await self._started()
        downloaded = await download(connection, methods["sql.commit"], SqlRun.encode(run=run))
        return _diff_of([*downloaded.items, downloaded.trailer], self._read)

    async def close(self) -> None:
        """Lets go of a run not committed, which writes nothing; nothing after a commit, or before a begin."""
        if self._ended:
            return
        self._ended = True
        if self._begun is None:
            return
        run, connection = self._begun
        # a connection lost let go of the run already
        with contextlib.suppress(Exception):
            await connection.session.call(methods["sql.drop"], SqlRun.encode(run=run))

    async def _started(self) -> tuple[int, Connection]:
        if self._begun is None:
            self._begun = await self._begin()
        return self._begun

    async def _send(self, run: int, connection: Connection, rows: Sequence[Any]) -> None:
        """Sends a part, halved until each half fits one message."""
        body = SqlAdd.encode(run=run, rows=_list_of(rows))
        try:
            await connection.session.call(methods["sql.add"], body)
        except LimitError as err:
            if len(rows) < 2 or err.limit != REQUEST_BYTES.name:
                raise
            half = (len(rows) + 1) // 2
            await self._send(run, connection, rows[:half])
            await self._send(run, connection, rows[half:])

    def _refuse_ended(self) -> None:
        if self._ended:
            raise InvalidError("the run is committed or closed: begin another")


def _scope_of(scope: Mapping[str, Any]) -> dict[str, Any]:
    columns = list(scope)
    return {"scope": columns, "scope_values": [cell_of(scope[column], f"the scope's {column}") for column in columns]}


def _diff_of[T](parts: Sequence[bytes], read: Callable[[dict[str, Any]], T]) -> Diff[T]:
    """A difference from its parts, each list joined in their order, a row as the mirror's type reads it."""
    diffs = [SqlDiff.decode(part) for part in parts]
    none: list[str] = []
    columns: list[str] = next((diff["columns"] for diff in diffs if diff.get("columns")), none)

    def rows(name: str) -> list[T]:
        return [read(row_of(columns, row)) for diff in diffs for row in diff.get(name, [])]

    changed = zip(rows("before"), rows("after"), strict=True)
    return Diff(
        added=rows("added"),
        changed=[Changed(before, after) for before, after in changed],
        gone=rows("gone"),
        same=sum(diff.get("same", 0) for diff in diffs),
        suspect=any(diff.get("suspect", False) for diff in diffs),
    )


def _list_of(rows: Sequence[Any]) -> dict[str, Any]:
    """The rows as cells, the first row's columns once: rows of another shape are InvalidError."""
    fields = [fields_of(row) for row in rows]
    columns = list(fields[0]) if fields else []
    cells: list[list[SqlArg]] = []
    for at, row in enumerate(fields):
        if sorted(row) != sorted(columns):
            raise InvalidError(f"row {at + 1}: not the first row's columns: give rows of one shape")
        cells.append([field_of(row[column], column) for column in columns])
    return {"columns": columns, "rows": cells}
