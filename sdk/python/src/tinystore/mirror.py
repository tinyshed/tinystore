"""A table that mirrors another system: it takes the source's list and says what changed.

The server compares the list with what the table holds by the source's key
and writes only the difference, in one commit; this module only says the
call in Python.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING, Any

from ._rows import SqlArg, cell_of, field_of, fields_of, reader, row_of
from ._wire.protocol import SqlDiff, SqlMirror
from .errors import InvalidError

if TYPE_CHECKING:
    from collections.abc import Awaitable, Callable, Mapping, Sequence

    from .sql import Mark

    MirrorCall = Callable[[Callable[[int], bytes]], Awaitable[bytes]]
    """What sends a mirror's request on the database's connection and gives back its answer."""


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
    ) -> None:
        self.table = table
        self._read: Callable[[dict[str, Any]], T] = reader(of)
        self._key, self._gone, self._change_on = options
        self._call = call

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
        InvalidError.
        """
        columns = list(scope)
        values = [cell_of(scope[column], f"the scope's {column}") for column in columns]
        return await self._run(rows, mode="replace", scope=columns, scope_values=values, min_kept=min_kept)

    async def upsert(self, rows: Sequence[T | Mapping[str, Any]]) -> Diff[T]:
        """Writes what ``rows`` adds and changes, a delta of the source's; its ``gone`` is always empty."""
        return await self._run(rows, mode="upsert")

    async def _run(self, rows: Sequence[T | Mapping[str, Any]], **asked: Any) -> Diff[T]:
        listed = _list_of(rows)
        gone = self._gone

        def body(handle: int) -> bytes:
            return SqlMirror.encode(
                handle=handle,
                table=self.table,
                key=self._key,
                gone=None if gone is None else gone.column,
                # milliseconds go unnamed, which a server older than the unit reads too
                gone_unit="s" if gone is not None and gone.unit == "s" else None,
                change_on=self._change_on,
                rows=listed,
                **{name: value for name, value in asked.items() if value is not None},
            )

        diff = SqlDiff.decode(await self._call(body))
        columns: list[str] = diff.get("columns", [])

        def read(rows: list[list[Any]]) -> list[T]:
            return [self._read(row_of(columns, row)) for row in rows]

        changed = zip(read(diff.get("before", [])), read(diff.get("after", [])), strict=True)
        return Diff(
            added=read(diff.get("added", [])),
            changed=[Changed(before, after) for before, after in changed],
            gone=read(diff.get("gone", [])),
            same=diff.get("same", 0),
            suspect=diff.get("suspect", False),
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
