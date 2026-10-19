"""Search, as docs/guides/search/README.md has it.

Documents of one type set by id in a named index and found again by their
words, by vectors the program's own model gives them, or by both, kept to the
documents a ``where`` passes. A search answers the exact best ten, or
``limit``, of what the index holds; an index opened with ``recall`` answers
its vectors from clusters. A table's index, ``db.search_index``, finds a
database's rows, each hit its row::

    articles = store.index("articles", Article, fields={"title": 2, "body": 1})
    await articles.set("a-17", Article(title="Хлеб на закваске", body="…"))
    result = await articles.search("как испечь хлеб")
"""

from __future__ import annotations

import asyncio
import json
import math
import struct
import sys
from array import array
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from datetime import UTC, datetime, timedelta
from typing import TYPE_CHECKING, Any, Literal

from ._connection import check_name
from ._handles import key_text, via_link
from ._rows import reader, row_of
from ._values import from_json, to_json
from ._wire.protocol import (
    Empty,
    Handle,
    SearchDeleted,
    SearchDropped,
    SearchFound,
    SearchGot,
    SearchKey,
    SearchListed,
    SearchName,
    SearchOpen,
    SearchQuery,
    SearchSet,
    SearchSetMany,
    SearchStatus,
    methods,
)
from .errors import InvalidError

if TYPE_CHECKING:
    from collections.abc import Callable

    from ._connection import Idempotence, Link
    from ._handles import Opener, Via
    from ._wire.codec import Key

_READY_EVERY = 0.1
"""How often ready asks whether a rebuild switched in, in seconds."""
_EPOCH = datetime(1970, 1, 1, tzinfo=UTC)

Vector = Sequence[float]
"""A vector the program's own model gave, of the index's dimensions: a list of floats, or an array."""

Fields = Mapping[str, float] | Sequence[str]
"""The fields whose words are read, each with what a word found there weighs, or a list that weighs each 1."""


@dataclass(frozen=True, slots=True)
class Vectors:
    """A vector the program's model gives each document, and recall for searches from clusters.

    A table's index names the column of blobs that holds them.
    """

    dimensions: int
    recall: float | None = None
    column: str | None = None


@dataclass(frozen=True, slots=True)
class Hybrid:
    """How a search by words and a vector at once combines them: a vector weight, or the words' best reranked."""

    vector_weight: float | None = None
    rerank: bool | None = None


@dataclass(frozen=True, slots=True)
class Hit[T]:
    """A document a search found, and the score that ordered it among this search's hits."""

    id: str
    value: T
    score: float


@dataclass(frozen=True, slots=True)
class Result[T]:
    """What a search found, the best first, and the text it read in fact when a word was misspelt."""

    hits: list[Hit[T]]
    corrected: str | None = None


@dataclass(frozen=True, slots=True)
class IndexStatus:
    """Where an index stands: ready, rebuilding with how much is done, or a rebuild that failed and why."""

    state: Literal["ready", "rebuilding", "failed"]
    done: float = 0.0
    error: str = ""


@dataclass(frozen=True, slots=True)
class IndexInfo:
    """An index as ``store.indexes()`` lists it."""

    name: str
    documents: int
    vectors: int
    opened: datetime | None
    """when a program last opened it"""
    status: IndexStatus


@dataclass(frozen=True, slots=True)
class Group:
    """Conditions joined: ``search.or_(…)``, ``search.and_(…)``."""

    join: Literal["or", "and"]
    parts: tuple[Where, ...]


Where = Mapping[str, Any] | Group
"""What a search keeps its hits to, as sqldb's where reads a dict.

Each field equal to its value, one of a list, within a range, ``{"gte": 10,
"lt": 100}``, or None for none, every field named passing; or_ and and_ group them.
"""


def or_(*parts: Where) -> Group:
    """Conditions any of which passes: ``search.or_({"category": "boots"}, {"tags": "sale"})``."""
    return Group("or", parts)


def and_(*parts: Where) -> Group:
    """Conditions every one of which passes."""
    return Group("and", parts)


class _Unset:
    pass


_UNSET: Any = _Unset()
"""A vector a set does not mention, apart from None, which says the document has none."""


class _Searchable[T]:
    """What every index answers: its searches, and where it stands."""

    def __init__(self, via: Via, name: str, opener: Opener, read: Callable[[dict[str, Any]], T]) -> None:
        self.name = name
        self._via = via
        self._opener = opener
        self._read = read

    async def search(
        self,
        text: str = "",
        *,
        vector: Vector | None = None,
        where: Where | None = None,
        hybrid: Hybrid | None = None,
        limit: int | None = None,
        offset: int | None = None,
    ) -> Result[T]:
        """The best ten, or limit, by text's words, by a vector, or by both, kept to what where passes.

        Both at once scale each half's best hundred and sum them at the
        index's vector weight. Of equal scores the one set first comes first.
        """
        vector_bytes = None if vector is None else _vector_bytes(vector)
        asked = None if where is None else where_json(where)

        def body(handle: int) -> bytes:
            return SearchQuery.encode(
                handle=handle,
                text=text,
                vector=vector_bytes,
                vector_weight=None if hybrid is None else hybrid.vector_weight,
                rerank=None if hybrid is None else hybrid.rerank,
                where=asked,
                offset=offset,
                limit=limit,
            )

        found = SearchFound.decode(await self._call("search.search", body, "read"))
        return Result(
            [
                Hit(hit.get("id", ""), self._read({**hit, "columns": found.get("columns")}), hit.get("score", 0.0))
                for hit in found.get("hits", [])
            ],
            found.get("corrected"),
        )

    async def status(self) -> IndexStatus:
        """Where the index stands; a search never waits, and answers by the settings it serves."""
        answer = await self._call("search.status", lambda handle: Handle.encode(handle=handle), "read")
        return _status_of(SearchStatus.decode(answer))

    async def ready(self) -> None:
        """Waits until the index serves the settings it was opened with; a rebuild that failed raises why."""
        while True:
            status = await self.status()
            if status.state == "ready":
                return
            if status.state == "failed":
                raise InvalidError(f"index {self.name}: its rebuild failed: {status.error}")
            await asyncio.sleep(_READY_EVERY)

    async def _call(self, method: str, body: Callable[[int], bytes], idempotence: Idempotence) -> bytes:
        return await self._via.call(methods["search.open"], self._opener, methods[method], body, idempotence)


class SearchIndex[T](_Searchable[T]):
    """Documents of one type by id, found again by their words, vectors or both: ``store.index(name, Article)``."""

    def __init__(self, link: Link, name: str, opener: bytes, of: Any) -> None:
        super().__init__(via_link(link), name, opener, lambda hit: from_json(hit.get("value") or "null", of))
        self._of = of

    async def set(self, id: Key, value: T, *, vector: Vector | None = _UNSET) -> None:
        """Sets the document id to value, replacing it whole; the next search sees it.

        On an index with vectors a set gives its vector, or None for a
        document only its words find.
        """
        said = _vector_said(vector)
        document = _document_json(self.name, value)
        await self._call(
            "search.set",
            lambda handle: SearchSet.encode(handle=handle, id=key_text(id), value=document, **said),
            "write",
        )

    async def set_many(self, documents: Sequence[tuple[Key, T] | tuple[Key, T, Vector | None]]) -> None:
        """Sets every document in one commit, all of them or none: (id, value) or (id, value, vector), 1,000 at most."""
        encoded = [
            {
                "id": key_text(each[0]),
                "value": _document_json(self.name, each[1]),
                **_vector_said(each[2] if len(each) == 3 else _UNSET),
            }
            for each in documents
        ]
        await self._call(
            "search.setMany", lambda handle: SearchSetMany.encode(handle=handle, documents=encoded), "write"
        )

    async def delete(self, id: Key) -> bool:
        """Deletes the document id; says whether there was one."""
        answer = await self._call(
            "search.delete", lambda handle: SearchKey.encode(handle=handle, id=key_text(id)), "write"
        )
        return SearchDeleted.decode(answer).get("deleted", False)

    async def get(self, id: Key) -> T | None:
        """The document id, or None."""
        answer = await self._call("search.get", lambda handle: SearchKey.encode(handle=handle, id=key_text(id)), "read")
        got = SearchGot.decode(answer)
        if not got.get("found") or got.get("value") is None:
            return None
        return from_json(got["value"], self._of)


class TableIndex[T](_Searchable[T]):
    """A table's rows found again by their words, vectors or both, each hit's value its row as ``db.all`` reads it.

    The table owns the rows, and every change of one reaches the index in the
    commit that makes it.
    """

    def __init__(self, via: Via, name: str, opener: Opener, of: Any) -> None:
        read_row = reader(of)
        super().__init__(
            via, name, opener, lambda hit: read_row(row_of(hit.get("columns") or [], hit.get("row") or []))
        )


def _fields_of(fields: Fields | None) -> list[dict[str, Any]]:
    if fields is None:
        return []
    if isinstance(fields, Mapping):
        return [{"path": path, "weight": weight} for path, weight in fields.items()]
    if isinstance(fields, str):
        raise InvalidError(f"fields {fields!r}: a list of fields, or a dict of their weights")
    return [{"path": path, "weight": 1} for path in fields]


def index_open(
    name: str,
    *,
    fields: Fields | None = None,
    filters: Sequence[str] = (),
    typo_tolerance: bool | None = None,
    vectors: Vectors | None = None,
    hybrid: Hybrid | None = None,
    rebuild: Literal["automatic", "manual"] | None = None,
    reindex: bool = False,
    database: int | None = None,
    table: str | None = None,
    key: str | None = None,
) -> bytes:
    """The open of an index, or of a table's index with its table and its database's handle."""
    check_name(name, "index")
    if rebuild not in (None, "automatic", "manual"):
        raise InvalidError(f"index {name}: a rebuild of {rebuild!r}, not automatic or manual")
    if isinstance(filters, str):
        raise InvalidError(f"index {name}: filters {filters!r}, a list of fields")
    return SearchOpen.encode(
        name=name,
        fields=_fields_of(fields),
        filters=list(filters),
        typo_tolerance=typo_tolerance,
        dimensions=None if vectors is None else vectors.dimensions,
        recall=None if vectors is None else vectors.recall,
        vector_weight=None if hybrid is None else hybrid.vector_weight,
        rerank=None if hybrid is None else hybrid.rerank,
        manual=rebuild == "manual",
        reindex=reindex,
        database=database,
        table=table,
        key=key,
        vector_column=None if vectors is None else vectors.column,
    )


async def drop_index(link: Link, name: str, database: Callable[[Any], Any] | None = None) -> bool:
    """Drops an index and every document it holds; says whether there was one."""
    check_name(name, "index")

    async def attempt(connection: Any) -> bytes:
        handle = None if database is None else await database(connection)
        return await connection.session.call(methods["search.dropIndex"], SearchName.encode(name=name, database=handle))

    return SearchDropped.decode(await link.run("write", attempt)).get("dropped", False)


async def list_indexes(link: Link) -> list[IndexInfo]:
    """Every index the store holds, by name."""
    answer = await link.run("read", lambda c: c.session.call(methods["search.list"], Empty.encode()))
    return [
        IndexInfo(
            each.get("name", ""),
            each.get("documents", 0),
            each.get("vectors", 0),
            None if each.get("opened") is None else _EPOCH + timedelta(milliseconds=each["opened"]),
            _status_of(each),
        )
        for each in SearchListed.decode(answer).get("indexes", [])
    ]


def _status_of(status: dict[str, Any]) -> IndexStatus:
    state = status.get("state")
    if state == "rebuilding":
        return IndexStatus("rebuilding", status.get("done", 0.0))
    if state == "failed":
        return IndexStatus("failed", error=status.get("error", ""))
    return IndexStatus("ready")


def _document_json(name: str, value: Any) -> str:
    try:
        return to_json(value)
    except InvalidError as err:
        raise InvalidError(f"index {name}: a document that does not write as JSON: {err}") from None


def _vector_said(vector: Vector | None) -> dict[str, Any]:
    """What a set says of its vector: its bytes, none, or nothing."""
    if vector is _UNSET:
        return {}
    return {"no_vector": True} if vector is None else {"vector": _vector_bytes(vector)}


def _vector_bytes(vector: Vector) -> bytes:
    """A vector's floats' bytes, little-endian, four a dimension."""
    if isinstance(vector, array) and vector.typecode == "f" and sys.byteorder == "little":
        return vector.tobytes()
    floats = list(vector)
    try:
        return struct.pack(f"<{len(floats)}f", *floats)
    except (struct.error, OverflowError) as err:
        raise InvalidError(f"a vector of {len(floats)} values that are not all floats: {err}") from None


def where_json(where: Where) -> str:
    """A where as the wire takes it, a JSON list of conditions joined by and.

    ``{"eq":[path,value]}``, ``{"in":[path,[values]]}``, ``{"missing":path}``,
    ``{"range":[path,{"gt"|"gte"|"lt"|"lte":n}]}``, ``{"or":[…]}``, ``{"and":[…]}``.
    """
    return json.dumps(_conditions(where), separators=(",", ":"), ensure_ascii=False)


def _conditions(where: Where) -> list[Any]:
    if isinstance(where, Group):
        return [{where.join: [{"and": _conditions(part)} for part in where.parts]}]
    return [_condition(path, value) for path, value in where.items()]


def _condition(path: str, value: Any) -> dict[str, Any]:
    if value is None:
        return {"missing": path}
    if isinstance(value, list | tuple):
        return {"in": [path, [_scalar(path, each) for each in value]]}  # pyright: ignore[reportUnknownVariableType]
    if isinstance(value, Mapping):
        bounds: dict[str, Any] = {}
        for bound, n in value.items():  # pyright: ignore[reportUnknownVariableType]
            if bound not in ("gt", "gte", "lt", "lte"):
                raise InvalidError(f"a where of {path}: {bound}, not gt, gte, lt or lte")
            bounds[bound] = _scalar(path, n)
        return {"range": [path, bounds]}
    return {"eq": [path, _scalar(path, value)]}


def _scalar(path: str, value: Any) -> Any:
    if isinstance(value, float) and not math.isfinite(value):
        raise InvalidError(f"a where of {path}: {value}, which is no number JSON writes")
    if value is None or not isinstance(value, str | int | float | bool):
        raise InvalidError(f"a where of {path}: {value!r}, where text, a number or a bool is")
    return value
