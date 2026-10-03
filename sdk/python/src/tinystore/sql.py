"""The application's own SQLite databases, sql/<name>.db, as Go's sqldb keeps them.

The application writes every query; the server owns the file, its
connections and its migrations. What a call's name starts with says where it
runs: exec and its kin on the file's one writer, all, one, scalar and each on
readers. A statement is its text and its arguments, positional after it,
named as keywords, or a template string of Python 3.14::

    await db.all("select * from notes where id = ?", note_id)
    await db.all("select * from notes where id = :id", id=note_id)
    await db.all(t"select * from notes where id = {note_id}")
    await db.all(Note, "select * from notes where author_id = ?", user_id)
"""

from __future__ import annotations

import asyncio
import dataclasses
import typing
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, Any

from ._connection import Connection, Link, check_name, handle_on
from ._values import convert
from ._wire.messages import (
    METHODS,
    SqlColumns,
    SqlDatabase,
    SqlDone,
    SqlResults,
    SqlRow,
    SqlStatement,
    SqlStatements,
)
from .errors import InvalidError

if TYPE_CHECKING:
    import os
    from collections.abc import AsyncIterator, Callable, Mapping, Sequence

    from ._wire.codec import SqlArg

type Row = dict[str, Any]
"""A row as SQLite returned it, by its columns' names."""

type Migrations = str | os.PathLike[str] | Mapping[str, str]
"""A directory whose .sql files they are, at its root or in its one directory, or the files by name."""


@dataclass(frozen=True, slots=True)
class Done:
    """What a write changed: the rows, and the rowid of the last it inserted."""

    changes: int
    last_id: int


def _statement(given: tuple[Any, ...], named: dict[str, Any]) -> tuple[Any, dict[str, Any]]:
    """The row type a call names first, if any, and the statement's fields."""
    of: Any = None
    if given and isinstance(given[0], type):
        of, given = given[0], given[1:]
    if not given:
        raise InvalidError("a statement needs its text")
    text, args = given[0], list(given[1:])
    if hasattr(text, "strings") and hasattr(text, "interpolations"):
        args = [i.value for i in text.interpolations]
        text = "?".join(text.strings)
    if not isinstance(text, str):
        raise InvalidError(f"a statement is text or a template, not a {type(text).__name__}")
    return of, {"sql": text, "args": args or None, "named": named or None}


def _rows(of: Any, columns: Sequence[str], rows: Sequence[Sequence[Any]]) -> list[Any]:
    if len(set(columns)) != len(columns):
        twice = next(c for c in columns if columns.count(c) > 1)
        raise InvalidError(f"two columns named {twice}: name them apart with as")
    dicts = [dict(zip(columns, values, strict=False)) for values in rows]
    if of is None or of is dict or typing.is_typeddict(of):
        return dicts
    if dataclasses.is_dataclass(of) and isinstance(of, type):
        hints = typing.get_type_hints(of)
        return [of(**{k: _field(v, hints.get(k)) for k, v in d.items()}) for d in dicts]
    return [convert(d, of) for d in dicts]


def _field(value: Any, hint: Any) -> Any:
    # what SQLite keeps an integer of reads back as the field's own type
    if hint is bool and isinstance(value, int):
        return bool(value)
    return value


def _one[T](rows: list[T]) -> T | None:
    if len(rows) > 1:
        raise InvalidError(f"a query for one row answered {len(rows)}")
    return rows[0] if rows else None


def _scalar(columns: Sequence[str], rows: Sequence[Sequence[Any]]) -> Any:
    if not rows:
        raise InvalidError("a scalar query answered no row; one always answers, as count(*) does")
    if len(columns) != 1 or len(rows) > 1:
        raise InvalidError(f"a scalar query answered {len(rows)} rows of {len(columns)} columns")
    return rows[0][0]


def _sql_files(directory: Path) -> list[Path]:
    """The .sql files at a directory's root or, when it holds none, in its one directory."""
    found = sorted(p for p in directory.glob("*.sql") if p.is_file())
    if not found:
        inner = [p for p in directory.iterdir() if p.is_dir()]
        if len(inner) == 1:
            found = sorted(p for p in inner[0].glob("*.sql") if p.is_file())
    return found


async def _migration_files(given: Migrations) -> list[dict[str, str]]:
    if not isinstance(given, str) and not hasattr(given, "__fspath__"):
        files = dict(typing.cast("Mapping[str, str]", given))
        return [{"name": name, "text": files[name]} for name in sorted(files)]
    directory = Path(typing.cast("str", given))
    found = await asyncio.to_thread(_sql_files, directory)
    if not found:
        raise InvalidError(f"no .sql migrations in {directory}")
    texts = await asyncio.gather(*(asyncio.to_thread(p.read_text, "utf-8") for p in found))
    return [{"name": p.name, "text": t} for p, t in zip(found, texts, strict=True)]


async def open_database(link: Link, name: str, migrations: Migrations | None) -> Database:
    check_name(name, "database")
    files = None if migrations is None else await _migration_files(migrations)
    db = Database(link, name, SqlDatabase.encode(name=name, migrations=files))
    await link.run("write", db.handle)
    return db


class Database:
    def __init__(self, link: Link, name: str, open_body: bytes) -> None:
        self._link, self.name, self._open = link, name, open_body

    async def handle(self, connection: Connection) -> int:
        return await handle_on(connection, METHODS["sql.open"], self._open)

    async def _query(self, fields: dict[str, Any], write: bool) -> tuple[list[str], list[list[Any]]]:
        async def attempt(connection: Connection) -> tuple[list[str], list[list[Any]]]:
            body = SqlStatement.encode(handle=await self.handle(connection), write=write or None, **fields)
            stream = await connection.session.open(METHODS["sql.query"], body, True)
            try:
                columns: list[str] = SqlColumns.decode((await stream.next()).body).get("columns", [])
                rows: list[list[Any]] = []
                while True:
                    event = await stream.next()
                    stream.consumed(len(event.body))
                    if event.end:
                        return columns, rows
                    rows.append(SqlRow.decode(event.body).get("values", []))
            except asyncio.CancelledError:
                stream.cancel()
                raise

        return await self._link.run("write" if write else "read", attempt)

    async def all(self, *statement: Any, **named: SqlArg) -> list[Any]:
        """Every row a query answers, held in memory: the server refuses past 64 MiB of them."""
        of, fields = _statement(statement, named)
        return _rows(of, *await self._query(fields, False))

    async def one(self, *statement: Any, **named: SqlArg) -> Any:
        """The one row a query answers, None for none; two is InvalidError."""
        return _one(await self.all(*statement, **named))

    async def scalar(self, *statement: Any, **named: SqlArg) -> Any:
        """The one value of a query that always answers one row, as count(*) does."""
        _, fields = _statement(statement, named)
        return _scalar(*await self._query(fields, False))

    async def query(self, *statement: Any, **named: SqlArg) -> tuple[list[str], list[list[Any]]]:
        """The columns and rows as SQLite returned them, lists rather than dicts."""
        _, fields = _statement(statement, named)
        return await self._query(fields, False)

    async def each(self, *statement: Any, **named: SqlArg) -> AsyncIterator[Any]:
        """A query's rows one at a time as they arrive, within the credit the client grants."""
        of, fields = _statement(statement, named)
        connection = await self._link.connection()
        body = SqlStatement.encode(handle=await self.handle(connection), **fields)
        stream = await connection.session.open(METHODS["sql.query"], body, True)
        ended = False
        try:
            columns: list[str] = SqlColumns.decode((await stream.next()).body).get("columns", [])
            while True:
                event = await stream.next()
                stream.consumed(len(event.body))
                if event.end:
                    ended = True
                    return
                yield _rows(of, columns, [SqlRow.decode(event.body).get("values", [])])[0]
        finally:
            if not ended:
                stream.cancel()

    async def exec(self, *statement: Any, **named: SqlArg) -> Done:
        """Runs a statement on the writer and says what it changed."""
        _, fields = _statement(statement, named)

        async def attempt(connection: Connection) -> Done:
            body = SqlStatement.encode(handle=await self.handle(connection), **fields)
            done = SqlDone.decode(await connection.session.call(METHODS["sql.exec"], body))
            return Done(done.get("changes", 0), done.get("last_id", 0))

        return await self._link.run("write", attempt)

    async def exec_all(self, *statement: Any, **named: SqlArg) -> list[Any]:
        """The rows a write's returning clause answers."""
        of, fields = _statement(statement, named)
        return _rows(of, *await self._query(fields, True))

    async def exec_one(self, *statement: Any, **named: SqlArg) -> Any:
        return _one(await self.exec_all(*statement, **named))

    async def exec_scalar(self, *statement: Any, **named: SqlArg) -> Any:
        _, fields = _statement(statement, named)
        return _scalar(*await self._query(fields, True))

    def batch(self) -> SqlBatch:
        """Statements in one transaction, all or none, sent as the async with block ends; a failure names its call."""
        return SqlBatch(self, False)

    def view(self) -> SqlBatch:
        """Reads from one snapshot, sent as the async with block ends."""
        return SqlBatch(self, True)

    async def run_batch(
        self, statements: list[dict[str, Any]], read: bool, jobs: list[tuple[bytes, dict[str, Any]]] | None = None
    ) -> list[dict[str, Any]]:
        async def attempt(connection: Connection) -> list[dict[str, Any]]:
            handle = await self.handle(connection)
            queued = [
                {"handle": await handle_on(connection, METHODS["jobs.open"], open_body), "jobs": [job]}
                for open_body, job in jobs or []
            ]
            body = SqlStatements.encode(handle=handle, statements=statements, read=read or None, jobs=queued or None)
            return SqlResults.decode(await connection.session.call(METHODS["sql.batch"], body)).get("results", [])

        return await self._link.run("read" if read else "write", attempt)


class SqlBatch:
    """The statements of a batch or a view: each returns a future that settles with it."""

    def __init__(self, db: Database, read: bool) -> None:
        self._db, self._read = db, read
        self.database = db.name
        """the database the batch runs in, whose queues alone take its jobs"""
        self._calls: list[tuple[dict[str, Any], Any, asyncio.Future[Any]]] = []
        self._jobs: list[tuple[bytes, dict[str, Any], asyncio.Future[None]]] = []

    def enqueue(self, open_body: bytes, job: dict[str, Any]) -> asyncio.Future[None]:
        """Adds a job after the statements, for a queue's with_tx: a program writes queue.with_tx(tx).enqueue(value)."""
        if self._read:
            raise InvalidError("a job in a view, which reads")
        future: asyncio.Future[None] = asyncio.get_running_loop().create_future()
        self._jobs.append((open_body, job, future))
        return future

    def _record(
        self,
        statement: tuple[Any, ...],
        named: dict[str, Any],
        rows: bool,
        settle: Callable[[Any, dict[str, Any]], Any],
    ) -> asyncio.Future[Any]:
        of, fields = _statement(statement, named)
        future: asyncio.Future[Any] = asyncio.get_running_loop().create_future()
        self._calls.append(({**fields, "rows": True if rows and not self._read else None}, (of, settle), future))
        return future

    def exec(self, *statement: Any, **named: SqlArg) -> asyncio.Future[Done]:
        return self._record(statement, named, False, lambda _, r: Done(r.get("changes", 0), r.get("last_id", 0)))

    def all(self, *statement: Any, **named: SqlArg) -> asyncio.Future[list[Any]]:
        return self._record(statement, named, True, lambda of, r: _rows(of, r.get("columns", []), r.get("rows", [])))

    def one(self, *statement: Any, **named: SqlArg) -> asyncio.Future[Any]:
        return self._record(
            statement,
            named,
            True,
            lambda of, r: _one(_rows(of, r.get("columns", []), r.get("rows", []))),
        )

    def scalar(self, *statement: Any, **named: SqlArg) -> asyncio.Future[Any]:
        return self._record(statement, named, True, lambda _, r: _scalar(r.get("columns", []), r.get("rows", [])))

    async def __aenter__(self) -> SqlBatch:
        return self

    async def __aexit__(self, kind: object, err: object, trace: object) -> None:
        futures = [future for _, _, future in self._calls] + [future for _, _, future in self._jobs]
        if err is not None or not futures:
            for future in futures:
                future.cancel()
            return
        try:
            results = await self._db.run_batch(
                [fields for fields, _, _ in self._calls], self._read, [(o, job) for o, job, _ in self._jobs]
            )
        except BaseException as failure:
            for future in futures:
                future.set_exception(failure)
                future.exception()
            raise
        for (_, (of, settle), future), result in zip(self._calls, results, strict=False):
            try:
                future.set_result(settle(of, result))
            except Exception as failure:
                future.set_exception(failure)
        for _, _, future in self._jobs:
            future.set_result(None)
