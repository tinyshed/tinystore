"""The application's own SQLite databases, sql/<name>.db, as docs/guides/sql/README.md has them.

The application writes its SQL, or builds it with a table's queries, and the
store owns the file, its connections and its migrations, which every open
applies and checks. A statement leaves as its text and the values of its
``?``s, each as SQLite keeps it::

    db = await store.database("app", migrations=Path(__file__).parent / "migrations")
    mine = await db.all(Note, "select * from notes where author_id = ? order by created_at desc", user_id)
    result = await db.exec("update notes set done = 1 where id = ?", note_id)
"""

from __future__ import annotations

import asyncio
import contextlib
import contextvars
import math
import re
import struct
import sys
from collections.abc import Mapping
from dataclasses import dataclass, replace
from pathlib import Path
from typing import TYPE_CHECKING, Any, Literal, Protocol, cast, overload

from ._connection import Durability, check_durability, check_name, download
from ._handles import Opener, Via, handle_on, via_link
from ._rows import SqlArg, SqlValue, cell_of, check_columns, named_values, reader, row_of, value_reader
from ._values import to_json, values_of
from ._wire.protocol import (
    SqlBatch,
    SqlBatched,
    SqlColumns,
    SqlDone,
    SqlOpen,
    SqlQuery,
    SqlRows,
    SqlStatement,
    SqlTxAnswer,
    SqlTxCall,
    SqlTxOpen,
    methods,
)
from .channels import Channel, TxChannel
from .errors import InvalidError, error_of
from .jobs import Adding, Backoff, Concurrency, Job, Queue, QueueOptions, adding_in, open_queue
from .kv import Bucket, Parts, bucket_open
from .search import Fields, Hybrid, TableIndex, Vectors, drop_index, index_open

if TYPE_CHECKING:
    import os
    from collections.abc import AsyncIterator, Awaitable, Callable, Iterable, Sequence

    from ._connection import Connection, Idempotence, Link
    from ._session import Stream
    from ._time import Duration
    from .jobs import Worker
    from .query import Table


@dataclass(frozen=True, slots=True)
class Sql:
    """A piece of SQL and the values of its ``?``s, which are always values and never SQL.

    ``sql`` makes one; a call, a batch and a table's query take it.
    """

    text: str
    values: tuple[SqlArg, ...] = ()
    bare: bool = False
    """whether it stands beside others in an and or an or without parentheses: one column's comparison"""


Condition = Sql | Mapping[str, Any]
"""A condition: equal values by column, or a piece of SQL."""


class Template(Protocol):
    """A t-string of Python 3.14, ``t"... {value}"``: its text in strings, and its values in interpolations."""

    @property
    def strings(self) -> tuple[str, ...]: ...

    @property
    def interpolations(self) -> tuple[Any, ...]: ...


type Statement = str | Sql | Template
"""A statement as a call takes it: text, then the values of its ?s; a piece from sql; or a t-string."""


class Embeddable(Protocol):
    """Something that writes itself as SQL inside another piece: a table's query."""

    def embedded(self) -> Sql: ...


def _is_template(value: object) -> bool:
    """Whether a value is a t-string of Python 3.14, its text in strings and its values in interpolations."""
    return hasattr(value, "strings") and hasattr(value, "interpolations")


def grouped(text: str) -> bool:
    """Whether text is one group in parentheses, or a bare 0 or 1, which may stand beside others as it is."""
    if not text.startswith("(") or not text.endswith(")"):
        return text in ("0", "1")
    depth = 0
    for at, c in enumerate(text):
        if c == "(":
            depth += 1
        elif c == ")":
            depth -= 1
        if depth == 0 and at < len(text) - 1:
            return False
    return True


_WORD = re.compile(r"[A-Za-z_][A-Za-z0-9_$]*")


def stands_alone(text: str) -> bool:
    """Whether a piece keeps its meaning beside others in an and or an or.

    One group, or text with no and or or of its own outside quotes, comments
    and parentheses, which would bind with its neighbours'.
    """
    if grouped(text):
        return True
    depth = 0
    at = 0
    while at < len(text):
        c = text[at]
        if c in "'\"`":
            at = _closing(text, at, c)
        elif text.startswith("--", at):
            at = text.find("\n", at)
            if at < 0:
                break
        elif text.startswith("/*", at):
            at = text.find("*/", at + 2)
            if at < 0:
                break
            at += 1
        elif c == "(":
            depth += 1
        elif c == ")":
            depth -= 1
        elif depth == 0 and c.isascii() and c.isalpha() and (at == 0 or not _word_char(text[at - 1])):
            word = _WORD.match(text, at)
            assert word is not None
            if word[0].lower() in ("and", "or"):
                return False
            at += len(word[0]) - 1
        at += 1
    return True


def _word_char(c: str) -> bool:
    return c.isascii() and (c.isalnum() or c in "_$")


def joined_with(pieces: Sequence[Sql], joiner: str) -> Sql:
    """Pieces joined with and or or, each in parentheses unless it may stand as it is."""
    several = len(pieces) > 1
    text = f" {joiner} ".join(
        f"({each.text})" if several and not each.bare and not stands_alone(each.text) else each.text for each in pieces
    )
    return Sql(text, tuple(value for each in pieces for value in each.values))


def _closing(text: str, at: int, quote: str) -> int:
    """The index past a quoted run that starts at at, a doubled quote inside it kept."""
    following = at + 1
    while following < len(text):
        if text[following] == quote:
            if following + 1 < len(text) and text[following + 1] == quote:
                following += 2
                continue
            return following
        following += 1
    return len(text)


@dataclass(frozen=True, slots=True)
class Placeholders:
    positional: int
    named: list[str]
    numbered: bool


def placeholders(text: str) -> Placeholders:
    """What a text's placeholders are, counted outside quotes and comments."""
    positional = 0
    numbered = False
    named: list[str] = []
    at = 0
    while at < len(text):
        c = text[at]
        following = text[at + 1] if at + 1 < len(text) else ""
        if c in "'\"`":
            at = _closing(text, at, c)
        elif c == "[":
            at = text.find("]", at + 1)
            if at < 0:
                break
        elif c == "-" and following == "-":
            at = text.find("\n", at)
            if at < 0:
                break
        elif c == "/" and following == "*":
            at = text.find("*/", at + 2)
            if at < 0:
                break
            at += 1
        elif c == "?":
            if following.isdigit():
                numbered = True
            else:
                positional += 1
        elif c in ":@$" and (following.isalpha() or following == "_"):
            word = _WORD.match(text, at + 1)
            assert word is not None
            if word[0] not in named:
                named.append(word[0])
            at += len(word[0])
        at += 1
    return Placeholders(positional, named, numbered)


def _piece(text: str, values: Sequence[Any]) -> Sql:
    """A piece of SQL from text and the values of its ?s; a piece or a query in a ?'s place is written in."""
    counted = placeholders(text)
    if counted.named or counted.numbered:
        raise InvalidError(f"a piece of SQL takes its values at ? alone: {text}")
    if counted.positional != len(values):
        raise InvalidError(f"{counted.positional} placeholders and {len(values)} values: each ? takes one value")
    if not any(isinstance(value, Sql) or _embeddable(value) for value in values):
        return Sql(text, tuple(cell_of(value, f"value {at + 1}") for at, value in enumerate(values)))
    written = _split_at_placeholders(text, counted.positional)
    result = written[0]
    out: list[SqlArg] = []
    for at, value in enumerate(values):
        inner = value if isinstance(value, Sql) else value.embedded() if _embeddable(value) else None
        if inner is None:
            result += "?"
            out.append(cell_of(value, f"value {at + 1}"))
        else:
            result += inner.text
            out.extend(inner.values)
        result += written[at + 1]
    return Sql(result, tuple(out))


def _split_at_placeholders(text: str, count: int) -> list[str]:
    """Text cut at its ?s outside quotes and comments, count + 1 parts."""
    parts: list[str] = []
    start = 0
    at = 0
    while at < len(text):
        c = text[at]
        following = text[at + 1] if at + 1 < len(text) else ""
        if c in "'\"`":
            at = _closing(text, at, c)
        elif c == "[":
            at = max(text.find("]", at + 1), at)
        elif c == "-" and following == "-":
            end = text.find("\n", at)
            at = len(text) if end < 0 else end
        elif c == "/" and following == "*":
            end = text.find("*/", at + 2)
            at = len(text) if end < 0 else end + 1
        elif c == "?":
            parts.append(text[start:at])
            start = at + 1
        at += 1
    parts.append(text[start:])
    assert len(parts) == count + 1
    return parts


def _embeddable(value: object) -> bool:
    return callable(getattr(value, "embedded", None))


def _template_piece(template: Any) -> Sql:
    """A t-string's text with a ? where each value was, a piece or a query written in its place."""
    strings: Sequence[str] = template.strings
    interpolations: Sequence[Any] = template.interpolations
    text = strings[0]
    out: list[SqlArg] = []
    for at, interpolation in enumerate(interpolations):
        value = interpolation.value
        if isinstance(value, Sql):
            text += value.text
            out.extend(value.values)
        elif _embeddable(value):
            inner = value.embedded()
            text += inner.text
            out.extend(inner.values)
        else:
            text += "?"
            out.append(cell_of(value, f"value {at + 1}"))
        text += strings[at + 1]
    return Sql(text, tuple(out))


def ident_of(name: str) -> str:
    """A name as SQL quotes it: "u"."created_at", each part on its own."""
    if not isinstance(name, str) or not name or "\0" in name:  # pyright: ignore[reportUnnecessaryIsInstance]
        raise InvalidError(f"the name {name!r}: a name is text without NUL")
    return ".".join('"' + part.replace('"', '""') + '"' for part in name.split("."))


def _equal(name: str, value: Any) -> Sql:
    column = ident_of(name)
    if value is None:
        return Sql(f"{column} is null", (), True)
    if isinstance(value, list | tuple | set | frozenset):
        listed = sql.list(cast("Iterable[Any]", value))
        return Sql(f"{column} in {listed.text}", listed.values, True)
    return Sql(f"{column} = ?", (cell_of(value, name),), True)


def condition_of(condition: Condition | str) -> Sql:
    """A condition as a piece of SQL: equal values written in the order of their columns' names."""
    if isinstance(condition, Sql):
        return condition
    if isinstance(condition, str):
        return _piece(condition, ())
    if not isinstance(condition, Mapping):  # pyright: ignore[reportUnnecessaryIsInstance]
        raise InvalidError(f"a condition is a dict of equal values or a piece of SQL, not a {type(condition).__name__}")
    pieces = [_equal(name, condition[name]) for name in sorted(condition)]
    if len(pieces) == 1:
        return pieces[0]
    return _group(pieces, "and", "1")


def _group(conditions: Sequence[Condition], joiner: str, empty: str) -> Sql:
    pieces = [condition_of(each) for each in conditions]
    if not pieces:
        return Sql(empty)
    joined = joined_with(pieces, joiner)
    return Sql(f"({joined.text})", joined.values)


def _listed(value: Any) -> Any:
    """A list's item as JSON keeps it: a time as its milliseconds, bytes refused."""
    if value is None or isinstance(value, bool | int | float | str):
        return value
    if isinstance(value, bytes | bytearray):
        raise InvalidError("a list for in (...) holds text, numbers, booleans and None")
    return cell_of(value, "a list's item")


class SqlMaker:
    """``sql("total >= ?", minimum)``: a piece of SQL, and the helpers a query's conditions are made of."""

    def __call__(self, text: str | Any, /, *values: Any) -> Sql:
        """A piece of SQL from text and the values of its ``?``s, or from a t-string of Python 3.14."""
        if _is_template(text):
            if values:
                raise InvalidError("a t-string carries its values: pass it alone")
            return _template_piece(text)
        if not isinstance(text, str):
            raise InvalidError(f"a piece of SQL is text, not a {type(text).__name__}")
        return _piece(text, values)

    def or_(self, *conditions: Condition) -> Sql:
        """Conditions joined with or, in parentheses; an empty or finds nothing."""
        return _group(conditions, "or", "0")

    def and_(self, *conditions: Condition) -> Sql:
        """Conditions joined with and, in parentheses; an empty and finds everything."""
        return _group(conditions, "and", "1")

    def list(self, values: Iterable[Any]) -> Sql:
        """A list for in (...), as one value SQLite reads with json_each: any length, the same statement."""
        return Sql("(select value from json_each(?))", (to_json([_listed(value) for value in values]),))

    def json(self, value: Any) -> Sql:
        """A value kept as its JSON."""
        return Sql("?", (to_json(value),))

    def ident(self, name: str) -> Sql:
        """A name, quoted, never SQL: a column a request chooses."""
        return Sql(ident_of(name))

    def has(self, name: str, value: Any) -> Sql:
        """A JSON list in column name holding value."""
        return Sql(
            f"exists (select 1 from json_each({ident_of(name)}) where value = ?)", (cell_of(value, f"{name}'s value"),)
        )


sql = SqlMaker()


def statement_of(first: str | Sql | Any, values: Sequence[Any]) -> Sql:
    """A statement as a piece of SQL, in whichever way it was given.

    Text and the values of its ``?``s, text and one dict of ``:name`` values, a
    piece from ``sql``, or a t-string.
    """
    if isinstance(first, Sql):
        if values:
            raise InvalidError("a piece of SQL carries its values: pass it alone")
        return first
    if _is_template(first) or not isinstance(first, str):
        return sql(first, *values)
    if len(values) == 1 and isinstance(values[0], Mapping):
        counted = placeholders(first)
        if counted.positional or counted.numbered:
            raise InvalidError("named values and ? in one statement: give the values one way")
        return Sql(first, tuple(named_values(cast("Mapping[str, Any]", values[0]), counted.named)))
    return _piece(first, values)


_WRITES = re.compile(r"\b(insert|update|delete|replace|returning|create|drop|alter|vacuum|reindex|analyze)\b", re.I)


def _may_write(text: str) -> bool:
    """Whether a statement may write, which says whether a lost connection may send it again."""
    return _WRITES.search(text) is not None


@dataclass(frozen=True, slots=True)
class Done:
    """What a write changed: the rows, and SQLite's rowid of the row it inserted, 0 when none."""

    changes: int
    last_insert_rowid: int


def _done_of(done: Mapping[str, Any]) -> Done:
    return Done(done.get("changes", 0), done.get("last_insert_rowid", 0))


@dataclass(frozen=True, slots=True)
class Answered:
    """What one query answered: its columns, and its rows' values."""

    columns: list[str]
    rows: list[list[SqlValue]]


def _joined(parts: Iterable[bytes]) -> Answered:
    answered = Answered([], [])
    for body in parts:
        part = SqlRows.decode(body)
        if not answered.columns:
            answered.columns.extend(part.get("columns", []))
        answered.rows.extend(part.get("rows", []))
    return answered


_LITTLE = sys.byteorder == "little"


def _columns_of(parts: Iterable[bytes]) -> dict[str, list[SqlValue]]:
    """A statement's rows by their columns, from the parts of its answer."""
    names: list[str] = []
    columns: list[list[SqlValue]] = []
    for body in parts:
        part = SqlColumns.decode(body)
        for index, held in enumerate(part.get("columns", [])):
            if len(columns) <= index:
                names.append(held.get("name", ""))
                columns.append([])
            into = columns[index]
            if "reals" in held:
                # SQLite keeps no NaN: the store writes one where a row holds NULL
                into.extend(None if math.isnan(real) else real for real in _numbers(held["reals"], "d"))
            elif "integers" in held:
                nulls: bytes | None = held.get("nulls")
                integers = _numbers(held["integers"], "q")
                into.extend(
                    None if nulls is not None and nulls[row >> 3] >> (row & 7) & 1 else integer
                    for row, integer in enumerate(integers)
                )
            else:
                into.extend(held.get("values", []))
    check_columns(names)
    return dict(zip(names, columns, strict=True))


def _numbers(packed: bytes, code: str) -> tuple[Any, ...]:
    """A column's eight-byte numbers, little-endian, as the store packs them."""
    if len(packed) % 8:
        raise InvalidError(f"a column of {len(packed)} bytes, which holds no whole number of values")
    return struct.unpack(f"<{len(packed) // 8}{code}", packed)


Want = Literal["all", "one", "scalar"]


class Runner(Protocol):
    """What reads and writes a database: the database itself, or a transaction of it."""

    @property
    def describe(self) -> str: ...

    async def rows(self, statement: Sql, want: Want) -> Answered: ...

    async def exec_piece(self, statement: Sql) -> Done: ...

    def each_piece(self, statement: Sql) -> AsyncIterator[dict[str, SqlValue]] | None: ...


async def _all(runner: Runner, of: Any, statement: Sql) -> list[Any]:
    answered = await runner.rows(statement, "all")
    check_columns(answered.columns)
    read = reader(of)
    return [read(row_of(answered.columns, values)) for values in answered.rows]


async def _one(runner: Runner, of: Any, statement: Sql) -> Any:
    answered = await runner.rows(statement, "one")
    check_columns(answered.columns)
    if not answered.rows:
        return None
    return reader(of)(row_of(answered.columns, answered.rows[0]))


async def _scalar(runner: Runner, of: Any, statement: Sql) -> Any:
    answered = await runner.rows(statement, "scalar")
    value = answered.rows[0][0] if answered.rows and answered.rows[0] else None
    return value_reader(of)(value)


def _split(first: Any, rest: tuple[Any, ...]) -> tuple[Any, Any, tuple[Any, ...]]:
    """A call's type, when it gives one first, its statement and the statement's values."""
    if isinstance(first, str | Sql) or _is_template(first):
        return None, first, rest
    if not rest:
        raise InvalidError("a call gives its statement, after the row's type when it gives one")
    return first, rest[0], rest[1:]


@dataclass
class _Running:
    """A transaction whose function is running."""

    database: Database
    ended: bool = False
    """set once the function returned or raised: what it left running is outside the transaction"""


_running: contextvars.ContextVar[tuple[_Running, ...]] = contextvars.ContextVar("tinystore db tx", default=())


def inside_database_tx() -> bool:
    """Whether the caller runs inside the function of a database's transaction."""
    return any(not each.ended for each in _running.get())


type Migrations = str | os.PathLike[str] | Mapping[str, str]
"""A database's migrations: a directory of .sql files named 0001_notes.sql, or the files themselves by name."""


async def _migration_files(given: Migrations) -> list[dict[str, str]]:
    if isinstance(given, Mapping):
        return [{"name": name, "sql": given[name]} for name in sorted(given)]
    directory = Path(given)

    def read() -> list[dict[str, str]]:
        files = sorted(path for path in directory.iterdir() if path.is_file() and path.suffix == ".sql")
        return [{"name": path.name, "sql": path.read_text(encoding="utf-8")} for path in files]

    return await asyncio.to_thread(read)


async def open_database(
    link: Link, workers: set[Worker], name: str, migrations: Migrations | None, durability: Durability | None
) -> Database:
    """Opens a database by its name, its migrations applied and checked before it returns."""
    check_name(name, "database")
    check_durability(durability, f"sql {name}")
    files = None if migrations is None else await _migration_files(migrations)
    opener = SqlOpen.encode(name=name, migrations=files, durability=durability)
    db = Database(link, workers, name, opener)
    await link.run("read", db.handle)
    return db


class Database:
    """An application's database: ``await store.database("app", migrations=...)``.

    The verb says what comes back: all the rows, one or none, one value, or
    what a write changed. A write with ``returning`` goes through a read's
    verb and runs on the writer, its rows given once it is durable.
    """

    def __init__(self, link: Link, workers: set[Worker], name: str, opener: bytes) -> None:
        self.name = name
        self._describe = f"sql {name}"
        self._link = link
        self._workers = workers
        self._opener = opener
        self._guests: dict[tuple[int, bytes], tuple[int, Opener]] = {}
        """the buckets and queues kept in the file, by their open: a transaction opens them first"""

    @property
    def describe(self) -> str:
        return self._describe

    async def handle(self, connection: Connection) -> int:
        return await handle_on(connection, methods["sql.open"], self._opener)

    def keep(self, method: int, body: bytes, make: Callable[[Connection], Awaitable[bytes]]) -> Opener:
        kept = self._guests.get((method, body))
        if kept is not None:
            return kept[1]
        self._guests[(method, body)] = (method, make)
        return make

    def via(self, what: str) -> Via:
        return _GuardedVia(self, what, via_link(self._link))

    def refuse_around(self, what: str) -> None:
        """Refuses a call made around the database's transaction from inside its function.

        It would wait for the writer the transaction holds until the store
        rolled the transaction back, and then commit alone.
        """
        if any(each.database is self and not each.ended for each in _running.get()):
            raise InvalidError(
                f"{self._describe}: {what}: made around the database's transaction from inside it, where it would "
                "wait for the writer the transaction holds: make it through the transaction, as tx.exec or "
                "tx.with_(queue).add"
            )

    @overload
    def bucket[T](
        self,
        name: str,
        of: type[T],
        *,
        ttl: Duration | None = None,
        idle: Duration | None = None,
        encrypted: bool = False,
    ) -> Bucket[T]: ...
    @overload
    def bucket(
        self,
        name: str,
        of: None = None,
        *,
        ttl: Duration | None = None,
        idle: Duration | None = None,
        encrypted: bool = False,
    ) -> Bucket[Any]: ...
    def bucket(
        self,
        name: str,
        of: Any = None,
        *,
        ttl: Duration | None = None,
        idle: Duration | None = None,
        encrypted: bool = False,
    ) -> Bucket[Any]:
        """A bucket of values by key kept in this database's file, beside its tables.

        The same calls as ``store.bucket``'s, and its writes commit with the
        rows in ``db.tx``, through ``tx.with_(bucket)``::

            sessions = db.bucket("sessions", Session, idle="30d")
        """
        body = bucket_open(name, ttl, idle, encrypted, None)

        async def make(connection: Connection) -> bytes:
            return bucket_open(name, ttl, idle, encrypted, await self.handle(connection))

        opener = self.keep(methods["kv.bucket.open"], body, make)
        parts = Parts(self._link, name, opener, "kv.bucket.open", (), values_of(of), self)
        return Bucket(parts, self.via(f"kv bucket {name}"))

    @overload
    def search_index[T](
        self,
        name: str,
        of: type[T],
        *,
        table: str,
        key: str | None = None,
        fields: Fields,
        filters: Sequence[str] = (),
        typo_tolerance: bool | None = None,
        vectors: Vectors | None = None,
        hybrid: Hybrid | None = None,
        rebuild: Literal["automatic", "manual"] | None = None,
    ) -> TableIndex[T]: ...
    @overload
    def search_index(
        self,
        name: str,
        of: None = None,
        *,
        table: str,
        key: str | None = None,
        fields: Fields,
        filters: Sequence[str] = (),
        typo_tolerance: bool | None = None,
        vectors: Vectors | None = None,
        hybrid: Hybrid | None = None,
        rebuild: Literal["automatic", "manual"] | None = None,
    ) -> TableIndex[dict[str, Any]]: ...
    def search_index(
        self,
        name: str,
        of: Any = None,
        *,
        table: str,
        key: str | None = None,
        fields: Fields,
        filters: Sequence[str] = (),
        typo_tolerance: bool | None = None,
        vectors: Vectors | None = None,
        hybrid: Hybrid | None = None,
        rebuild: Literal["automatic", "manual"] | None = None,
    ) -> TableIndex[Any]:
        """An index of a table of this database, kept in its file: its rows found again by their words or vectors.

        Each hit's value is its row as ``all`` reads it. The table owns the
        rows and the index follows them: every change of a row, by any
        statement, trigger or migration, reaches it in the commit that makes
        it, and one by a process beside the store within a second::

            articles = db.search_index("articles_search", Article, table="articles", fields={"title": 2, "body": 1})
        """
        settings: dict[str, Any] = {
            "fields": fields,
            "filters": filters,
            "typo_tolerance": typo_tolerance,
            "vectors": vectors,
            "hybrid": hybrid,
            "rebuild": rebuild,
            "table": table,
            "key": key,
        }
        body = index_open(name, **settings)

        async def make(connection: Connection) -> bytes:
            return index_open(name, **settings, database=await self.handle(connection))

        opener = self.keep(methods["search.open"], body, make)
        return TableIndex(self.via(f"index {name}"), name, opener, of)

    async def drop_search_index(self, name: str) -> bool:
        """Drops a table's index this database keeps, its triggers off the table; says whether there was one."""
        return await drop_index(self._link, name, self.handle)

    @overload
    def queue[T](
        self,
        name: str,
        of: type[T],
        *,
        attempts: int | None = None,
        backoff: Backoff | None = None,
        timeout: Duration | None = None,
        concurrency: int | Concurrency | None = None,
        rate: str | None = None,
        dedupe: Duration | None = None,
        keep: Duration | None = None,
        max_waiting: int | None = None,
    ) -> Queue[T]: ...
    @overload
    def queue(
        self,
        name: str,
        of: None = None,
        *,
        attempts: int | None = None,
        backoff: Backoff | None = None,
        timeout: Duration | None = None,
        concurrency: int | Concurrency | None = None,
        rate: str | None = None,
        dedupe: Duration | None = None,
        keep: Duration | None = None,
        max_waiting: int | None = None,
    ) -> Queue[Any]: ...
    def queue(
        self,
        name: str,
        of: Any = None,
        *,
        attempts: int | None = None,
        backoff: Backoff | None = None,
        timeout: Duration | None = None,
        concurrency: int | Concurrency | None = None,
        rate: str | None = None,
        dedupe: Duration | None = None,
        keep: Duration | None = None,
        max_waiting: int | None = None,
    ) -> Queue[Any]:
        """A queue of jobs kept in this database's file, as ``store.queue`` keeps one in jobs.db.

        The same calls and workers, and its jobs commit with the rows in
        ``db.tx``, through ``tx.with_(queue)``. It shares the database's
        writer, which is the point and its cost.
        """
        options = QueueOptions(attempts, backoff, timeout, concurrency, rate, dedupe, keep, max_waiting)
        return open_queue(self._link, self._workers, name, of, options, self)

    @overload
    async def all[T](self, of: type[T], statement: Statement, /, *values: Any) -> list[T]: ...
    @overload
    async def all(self, statement: Statement, /, *values: Any) -> list[dict[str, Any]]: ...
    async def all(self, first: Any, /, *rest: Any) -> list[Any]:
        """Every row a statement gives: ``await db.all(Note, "select * from notes where author_id = ?", user_id)``."""
        of, statement, values = _split(first, rest)
        return await _all(self, of, statement_of(statement, values))

    @overload
    async def one[T](self, of: type[T], statement: Statement, /, *values: Any) -> T | None: ...
    @overload
    async def one(self, statement: Statement, /, *values: Any) -> dict[str, Any] | None: ...
    async def one(self, first: Any, /, *rest: Any) -> Any:
        """The one row a statement gives, or None; two are InvalidError, and a write that gave them rolls back."""
        of, statement, values = _split(first, rest)
        return await _one(self, of, statement_of(statement, values))

    @overload
    async def scalar[T](self, of: type[T], statement: Statement, /, *values: Any) -> T: ...
    @overload
    async def scalar(self, statement: Statement, /, *values: Any) -> Any: ...
    async def scalar(self, first: Any, /, *rest: Any) -> Any:
        """The one value of the one row a statement gives, as count(*) does; None as SQL answers it."""
        of, statement, values = _split(first, rest)
        return await _scalar(self, of, statement_of(statement, values))

    async def columns(self, statement: Statement, /, *values: Any) -> dict[str, list[Any]]:
        """What ``all`` gives, by its columns: a list a column, by the column's name.

        For rows by the thousand, a chart's points: they come faster, and
        nobody turns rows into columns after.
        """
        piece = statement_of(statement, values)

        def body(handle: int) -> bytes:
            return SqlStatement.encode(handle=handle, text=piece.text, values=list(piece.values))

        return _columns_of(await self._answered(piece, methods["sql.columns"], body))

    @overload
    def each[T](self, of: type[T], statement: Statement, /, *values: Any) -> AsyncIterator[T]: ...
    @overload
    def each(self, statement: Statement, /, *values: Any) -> AsyncIterator[dict[str, Any]]: ...
    def each(self, first: Any, /, *rest: Any) -> AsyncIterator[Any]:
        """The rows a statement gives, one at a time as their parts arrive, from one snapshot."""
        of, statement, values = _split(first, rest)
        piece = statement_of(statement, values)
        read = reader(of)

        async def rows() -> AsyncIterator[Any]:
            async for row in self._each(piece):
                yield read(row)

        return rows()

    @overload
    def table[T](self, name: str, of: type[T]) -> Table[T]: ...
    @overload
    def table(self, name: str, of: None = None) -> Table[dict[str, Any]]: ...
    def table(self, name: str, of: Any = None) -> Table[Any]:
        """A table of the database, typed where it opens: its rows' inserts and upserts, and queries.

        It costs nothing until its first call; open each once, in the module
        that owns it: ``notes = db.table("notes", Note)``.
        """
        from .query import Table

        return Table(self, name, of)

    async def exec(self, statement: Statement, /, *values: Any) -> Done:
        """Runs a write once it is durable; writes from many callers share a commit, each failing alone."""
        return await self.exec_piece(statement_of(statement, values))

    async def batch(self, writes: Sequence[Sql | Adding]) -> list[Done]:
        """Writes known before they run, written as one: all of them or none, in a shared commit.

        A write is a statement, or a job's add of a queue opened from the
        database, ``queue.adding``, which commits with the rows and changed 1
        job, or 0 when its id was taken. The answer says what each did, in
        their order.

            await db.batch([sql("update notes set body = ? where id = ?", body, id), indexing.adding(Index(id))])
        """
        parts: list[dict[str, Any] | Callable[[Connection], Awaitable[dict[str, Any]]]] = []
        for each in writes:
            if isinstance(each, Sql):
                parts.append({"text": each.text, "values": list(each.values)})
            elif isinstance(each, Adding):  # pyright: ignore[reportUnnecessaryIsInstance]
                parts.append(adding_in(each, self))
            else:
                raise InvalidError("a batch takes pieces of SQL, sql(...), and a queue's adding")
        self.refuse_around("a batch")

        async def attempt(connection: Connection) -> bytes:
            statements = [await part(connection) if callable(part) else part for part in parts]
            body = SqlBatch.encode(handle=await self.handle(connection), statements=statements)
            return await connection.session.call(methods["sql.batch"], body)

        answered = await self._link.run("write", attempt)
        return [_done_of(done) for done in SqlBatched.decode(answered).get("done", [])]

    async def tx[R](self, fn: Callable[[SqlTx], Awaitable[R]]) -> R:
        """Runs fn in a transaction that holds the writer, for writes that depend on what its reads found.

        Returning commits, an exception rolls back. It runs once, five seconds
        at most, past which the store rolls it back; inside it, call nothing
        that waits on the world.
        """
        self.refuse_around("a transaction")
        connection = await self._link.connection()
        handle = await self.handle(connection)
        guests = await self._open_guests(connection)
        stream = await connection.session.open(methods["sql.tx"], SqlTxOpen.encode(handle=handle), False)
        await stream.next()
        tx = SqlTx(stream, self, connection, guests)
        inside = _Running(self)
        token = _running.set((*_running.get(), inside))
        try:
            value = await fn(tx)
        except BaseException:
            inside.ended = True
            with contextlib.suppress(Exception):
                await tx.end(False)
            raise
        finally:
            inside.ended = True
            _running.reset(token)
        await tx.end(True)
        return value

    async def _open_guests(self, connection: Connection) -> dict[int, BaseException | None]:
        """Opens on connection the buckets and queues kept in the file, before a transaction holds its writer.

        A first open writes the file, and would wait for the writer the
        transaction holds. One that fails is told when the transaction takes
        it in.
        """
        kept = list(self._guests.values())
        settled = await asyncio.gather(
            *(handle_on(connection, method, opener) for method, opener in kept), return_exceptions=True
        )
        return {
            id(opener): outcome if isinstance(outcome, BaseException) else None
            for (_, opener), outcome in zip(kept, settled, strict=True)
        }

    async def rows(self, statement: Sql, want: Want) -> Answered:
        def body(handle: int) -> bytes:
            return SqlQuery.encode(handle=handle, text=statement.text, values=list(statement.values), want=want)

        return _joined(await self._answered(statement, methods["sql.query"], body))

    async def exec_piece(self, statement: Sql) -> Done:
        self.refuse_around("a write")

        async def attempt(connection: Connection) -> bytes:
            handle = await self.handle(connection)
            body = SqlStatement.encode(handle=handle, text=statement.text, values=list(statement.values))
            return await connection.session.call(methods["sql.exec"], body)

        return _done_of(SqlDone.decode(await self._link.run("write", attempt)))

    def each_piece(self, statement: Sql) -> AsyncIterator[dict[str, SqlValue]] | None:
        return self._each(statement)

    async def _each(self, statement: Sql) -> AsyncIterator[dict[str, SqlValue]]:
        self.refuse_around("a read")
        connection = await self._link.connection()
        handle = await self.handle(connection)
        body = SqlQuery.encode(handle=handle, text=statement.text, values=list(statement.values), want="all")
        stream = await connection.session.open(methods["sql.query"], body, True)
        try:
            head = await stream.next()
            if head.end:
                whole = _joined([head.body])
                check_columns(whole.columns)
                for values in whole.rows:
                    yield row_of(whole.columns, values)
                return
            columns: list[str] = []
            while True:
                event = await stream.next()
                stream.consumed(len(event.body))
                part = _joined([event.body])
                if not columns:
                    columns = part.columns
                    check_columns(columns)
                for values in part.rows:
                    yield row_of(columns, values)
                if event.end:
                    return
        finally:
            stream.cancel()

    async def _answered(self, statement: Sql, method: int, asked: Callable[[int], bytes]) -> list[bytes]:
        """The parts of a statement's answer: a read may be asked again of another connection, a write not."""
        self.refuse_around("a statement")
        idempotence: Idempotence = "write" if _may_write(statement.text) else "read"

        async def attempt(connection: Connection) -> list[bytes]:
            downloaded = await download(connection, method, asked(await self.handle(connection)))
            return [*downloaded.items, downloaded.trailer]

        return await self._link.run(idempotence, attempt)


class _GuardedVia:
    """The link's calls of a handle kept in a database's file, refused from inside its own transaction."""

    def __init__(self, database: Database, what: str, link: Via) -> None:
        self._database = database
        self._what = what
        self._link = link

    async def call(
        self,
        open_method: int,
        opener: Opener,
        method: int,
        body: Callable[[int], bytes],
        idempotence: Idempotence,
    ) -> bytes:
        self._database.refuse_around(self._what)
        return await self._link.call(open_method, opener, method, body, idempotence)


class _TxVia:
    """A handle's calls inside a database's transaction, on its stream."""

    def __init__(self, tx: SqlTx, connection: Connection) -> None:
        self._tx = tx
        self._connection = connection

    async def call(
        self,
        open_method: int,
        opener: Opener,
        method: int,
        body: Callable[[int], bytes],
        idempotence: Idempotence,
    ) -> bytes:
        handle = await handle_on(self._connection, open_method, opener)
        return await self._tx.nested(method, body(handle))


class TxQueue[T]:
    """A queue's calls inside a database's transaction, from ``tx.with_(queue)``.

    A job it adds is there only if the transaction commits, and the queue's
    workers find it once it has. A worker, a watch and a page work outside.
    """

    def __init__(self, queue: Queue[T]) -> None:
        self.name = queue.name
        self._queue = queue

    async def add(self, value: T, **options: Any) -> bool:
        """Adds a job unless its id is taken, as queue.add does, with the transaction."""
        return await self._queue.add(value, **options)

    async def set(self, id: str, value: T, **options: Any) -> None:
        await self._queue.set(id, value, **options)

    async def update(self, id: str, value: T, **options: Any) -> bool:
        return await self._queue.update(id, value, **options)

    async def cancel(self, id: str) -> bool:
        return await self._queue.cancel(id)

    async def get(self, id: str) -> Job[T] | None:
        """The job under id as the transaction sees it, its own writes among it."""
        return await self._queue.get(id)


class SqlTx:
    """A database's transaction, from ``db.tx``.

    Its reads see its writes, and a call that fails leaves it as it was
    before the call. Its calls go one at a time, in order.
    """

    def __init__(
        self, stream: Stream, database: Database, connection: Connection, guests: dict[int, BaseException | None]
    ) -> None:
        self._stream = stream
        self._database = database
        self._guests = guests
        self._via = _TxVia(self, connection)
        self._turn = asyncio.Lock()

    @property
    def describe(self) -> str:
        return self._database.describe

    @overload
    def with_[T](self, handle: Bucket[T]) -> Bucket[T]: ...
    @overload
    def with_[T](self, handle: Queue[T]) -> TxQueue[T]: ...
    @overload
    def with_[T](self, handle: Channel[T]) -> TxChannel[T]: ...
    def with_(self, handle: Bucket[Any] | Queue[Any] | Channel[Any]) -> Bucket[Any] | TxQueue[Any] | TxChannel[Any]:
        """A bucket or a queue opened from this database, or a channel, inside the transaction.

        What it writes commits with the rows or not at all, and what it reads
        sees what the transaction wrote; a message is handed over at the commit.
        """
        if isinstance(handle, Channel):

            async def publish(body: bytes) -> None:
                await self.nested(methods["channels.publish"], body)

            return TxChannel(handle.name, handle.branch, publish)
        if isinstance(handle, Bucket):
            parts = handle.parts
            self._takes(parts.home, parts.opener, f"kv bucket {parts.name}")
            return Bucket(parts, self._via)
        if isinstance(handle, Queue):  # pyright: ignore[reportUnnecessaryIsInstance]
            source = handle.source
            self._takes(source.home, source.opener, f"jobs queue {handle.name}")
            return TxQueue(Queue(replace(source, via=self._via), handle.name))
        raise InvalidError(f"{self.describe}: a transaction takes in a bucket or a queue opened from its database")

    @overload
    async def all[T](self, of: type[T], statement: Statement, /, *values: Any) -> list[T]: ...
    @overload
    async def all(self, statement: Statement, /, *values: Any) -> list[dict[str, Any]]: ...
    async def all(self, first: Any, /, *rest: Any) -> list[Any]:
        of, statement, values = _split(first, rest)
        return await _all(self, of, statement_of(statement, values))

    @overload
    async def one[T](self, of: type[T], statement: Statement, /, *values: Any) -> T | None: ...
    @overload
    async def one(self, statement: Statement, /, *values: Any) -> dict[str, Any] | None: ...
    async def one(self, first: Any, /, *rest: Any) -> Any:
        of, statement, values = _split(first, rest)
        return await _one(self, of, statement_of(statement, values))

    @overload
    async def scalar[T](self, of: type[T], statement: Statement, /, *values: Any) -> T: ...
    @overload
    async def scalar(self, statement: Statement, /, *values: Any) -> Any: ...
    async def scalar(self, first: Any, /, *rest: Any) -> Any:
        of, statement, values = _split(first, rest)
        return await _scalar(self, of, statement_of(statement, values))

    async def exec(self, statement: Statement, /, *values: Any) -> Done:
        return await self.exec_piece(statement_of(statement, values))

    @overload
    def table[T](self, name: str, of: type[T]) -> Table[T]: ...
    @overload
    def table(self, name: str, of: None = None) -> Table[dict[str, Any]]: ...
    def table(self, name: str, of: Any = None) -> Table[Any]:
        """A table inside the transaction: its calls are the transaction's."""
        from .query import Table

        return Table(self, name, of)

    async def rows(self, statement: Sql, want: Want) -> Answered:
        answer = await self._call(statement, want)
        rows = answer.get("rows", {})
        return Answered(list(rows.get("columns", [])), list(rows.get("rows", [])))

    async def exec_piece(self, statement: Sql) -> Done:
        return _done_of((await self._call(statement, "exec")).get("done", {}))

    def each_piece(self, statement: Sql) -> AsyncIterator[dict[str, SqlValue]] | None:
        return None

    async def end(self, commit: bool) -> None:
        """Sends the last call, commit or roll back, and waits for the store's word."""
        async with self._turn:
            await self._stream.send(SqlTxCall.encode(commit=commit), True)
            await self._stream.next()

    def _takes(self, home: Any, opener: Opener, what: str) -> None:
        """Refuses a handle kept elsewhere, and one made after the transaction began.

        The first's writes would not commit with the rows, and the second's
        first open would wait for the writer the transaction holds.
        """
        if home is not self._database:
            kept = "the store" if home is None else home.describe
            raise InvalidError(f"{what}: kept in {kept}, outside this transaction of {self.describe}")
        if id(opener) not in self._guests:
            raise InvalidError(
                f"{self.describe}: {what}: made after the transaction began; make it before db.tx, which opens it first"
            )
        failed = self._guests[id(opener)]
        if failed is not None:
            raise failed

    async def nested(self, method: int, body: bytes) -> bytes:
        """A call of kv or jobs in the transaction: its method and its request, and the method's answer."""
        answer = await self._send(SqlTxCall.encode(want="call", method=method, body=body))
        return answer.get("body", b"")

    async def _call(self, statement: Sql, want: str) -> dict[str, Any]:
        return await self._send(SqlTxCall.encode(text=statement.text, values=list(statement.values), want=want))

    async def _send(self, body: bytes) -> dict[str, Any]:
        async with self._turn:
            await self._stream.send(body, False)
            event = await self._stream.next()
            self._stream.consumed(len(event.body))
        answer = SqlTxAnswer.decode(event.body)
        failure = answer.get("failure")
        if failure is not None:
            raise error_of(failure.get("code", "internal"), failure.get("message", ""), failure.get("what"))
        return answer
