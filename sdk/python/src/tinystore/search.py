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
from ._handles import handle_on, key_text, via_link
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
    SearchManyFound,
    SearchManyQuery,
    SearchName,
    SearchOpen,
    SearchQuery,
    SearchSet,
    SearchSetMany,
    SearchStatus,
    methods,
)
from .errors import InvalidError, ProtocolError, TinystoreError, error_of

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

KeyboardLayout = Literal["ru", "en"]
"""A keyboard layout an index's readers type with: ЙЦУКЕН, or QWERTY."""


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
class Match:
    """Where a search's word matched a hit: ``text[start:end]`` of a field's text, in code points."""

    field: str
    """the field's path, or a table's column; "" for a document that is its own text"""
    start: int
    end: int
    word: str
    """the query's word, as its text wrote it"""
    item: int | None = None
    """the string's place in a field that holds a list"""


@dataclass(frozen=True, slots=True)
class Mark:
    """Where a search's word matched inside a snippet: ``snippet.text[start:end]``, in code points."""

    start: int
    end: int


@dataclass(frozen=True, slots=True)
class Snippet:
    """The piece of a field around a hit's first match there, the field's own text, … where it was cut.

    Its marks are every match inside it. The store adds no markup and escapes nothing.
    """

    text: str
    marks: list[Mark]
    item: int | None = None
    """the string's place in a field that holds a list"""


@dataclass(frozen=True, slots=True)
class Hit[T]:
    """A document a search found, and the score that ordered it among this search's hits."""

    id: str
    value: T
    score: float
    matches: list[Match] | None = None
    """where the search's words matched it, when the search asked with matches"""
    text_score: float | None = None
    """the score its words gave it, as a search by its words alone scores it, where they found it among their best"""
    similarity: float | None = None
    """the cosine of its vector with the search's, from -1 to 1, where both have one"""
    snippet: Snippet | None = None
    """the piece of a field around its first match, when the search asked with snippet and it holds the field"""


@dataclass(frozen=True, slots=True)
class Result[T]:
    """What a search found, the best first, and the text it read in fact when a word was misspelt."""

    hits: list[Hit[T]]
    corrected: str | None = None
    total: Total | None = None
    """how many it finds, when the search asked with total"""


@dataclass(frozen=True, slots=True)
class Total:
    """How many documents a search finds: exactly, or past 10,000, 10,000 and at_least, never an estimate."""

    count: int
    at_least: bool
    """whether counting stopped at 10,000, more being there"""


@dataclass(frozen=True, slots=True)
class IndexStatus:
    """Where an index stands: ready, rebuilding with how much is done, or a rebuild that failed and why.

    ``error`` beside ``ready`` and ``rebuilding`` is the last maintenance pass's failure while the passes keep failing.
    """

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
A search's where holds 8 conditions and 100 values in its lists at most. For
a table's index, a value may be a statement of its database, ``sql("select
user_id from members where course_id = ?", course)``: one of the values its
rows give, read in the search's snapshot, 100,000 at most, counting toward
no bound on values; on the table's key no filter is needed.
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

Aliases = Mapping[str, str]
"""Each alias, one word, with the path of the field whose whole text it stands for in its document alone."""


@dataclass(frozen=True, slots=True)
class Doc[T]:
    """A document set_many sets, with its vector, or None for none, and its aliases."""

    id: Key
    value: T
    vector: Vector | None = _UNSET
    aliases: Aliases | None = None


class _Searchable[T]:
    """What every index answers: its searches, and where it stands."""

    def __init__(self, link: Link, via: Via, name: str, opener: Opener, read: Callable[[dict[str, Any]], T]) -> None:
        self.name = name
        self._link = link
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
        min_similarity: float | None = None,
        prefix: bool | Literal["every"] = False,
        matches: bool = False,
        snippet: Mapping[str, Any] | None = None,
        total: bool = False,
        limit: int | None = None,
        offset: int | None = None,
    ) -> Result[T]:
        """The best ten, or limit, by text's words, by a vector, or by both, kept to what where passes.

        Both at once scale each half's best hundred and sum them at the
        index's vector weight. Of equal scores the one set first comes first.
        With prefix, the last word is read as the start of a word too, as a
        search box types: "crypt" finds "cryptography" as well, at half its
        field's weight; a word a space follows is whole, and so is one of a
        single letter. With prefix="every", every word of two letters or more
        is read so, the first eight, as people shorten each word of a name:
        "лин алг" finds "Линейная алгебра"; a document that holds a word for
        each word of the text comes first, its score raised above the rest.
        With matches, each hit says where the search's words matched it, as a
        search box shows them in bold: ranges of code points of each field's
        text, a word found as written, by its stem or once corrected whole,
        and a word a prefix began by the letters typed. With snippet,
        {"field": "plain", "around": 60}, each hit's snippet of that field:
        its own text from around characters before the field's first match to
        around after it, 1 to 1000 and 80 unless said, cut where a word
        starts and ends, … where text was cut; a hit that matched elsewhere,
        or by its vector alone, gets the field's first 2 × around characters.
        With min_similarity,
        the least cosine a document's vector has with the search's for the
        vector to find it: a search by its vector alone leaves out the hits
        under it, and one by words too adds nothing for them to the sum.
        With total, the result counts the documents it finds as well, under
        its where and whatever its limit and offset: exactly up to 10,000,
        and past it 10,000 with at_least; it reads every posting of its
        words, which a search alone skips most of; invalid beside a vector.
        """
        if prefix not in (True, False, "every"):
            raise InvalidError(f"a prefix of {prefix!r}: True, False or 'every'")
        statements: list[dict[str, Any]] = []
        body = _query(
            text,
            vector_bytes=None if vector is None else _vector_bytes(vector),
            where=None if where is None else where_json(where, statements),
            statements=statements,
            hybrid=hybrid,
            min_similarity=min_similarity,
            prefix=prefix,
            matches=matches,
            snippet=_cut(snippet),
            total=total,
            limit=limit,
            offset=offset,
        )
        found = SearchFound.decode(await self._call("search.search", body, "read"))
        return self._result_of(found)

    def _result_of(self, found: dict[str, Any]) -> Result[T]:
        """The hits a search found, each value read as this index reads its documents."""
        return Result(self._hits_of(found), found.get("corrected"), _total_of(found.get("total")))

    def _hits_of(self, found: dict[str, Any]) -> list[Hit[T]]:
        return [
            Hit(
                hit.get("id", ""),
                self._read({**hit, "columns": found.get("columns")}),
                hit.get("score", 0.0),
                _matches_of(hit.get("matches")),
                hit.get("text_score"),
                hit.get("similarity"),
                _snippet_of(hit.get("snippet")),
            )
            for hit in found.get("hits", [])
        ]

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
        super().__init__(link, via_link(link), name, opener, lambda hit: from_json(hit.get("value") or "null", of))
        self._of = of

    async def set(self, id: Key, value: T, *, vector: Vector | None = _UNSET, aliases: Aliases | None = None) -> None:
        """Sets the document id to value, replacing it whole; the next search sees it.

        On an index with vectors a set gives its vector, or None for a
        document only its words find. Its aliases, ``{"кмзи": "name"}``,
        find it as a match of the field each stands for and mark that
        field's text; a set without them leaves the document none.
        """
        said = {**_vector_said(vector), **_aliases_said(aliases)}
        document = _document_json(self.name, value)
        await self._call(
            "search.set",
            lambda handle: SearchSet.encode(handle=handle, id=key_text(id), value=document, **said),
            "write",
        )

    async def set_many(self, documents: Sequence[tuple[Key, T] | tuple[Key, T, Vector | None] | Doc[T]]) -> None:
        """Sets every document in one commit, all of them or none: (id, value), (id, value, vector) or a Doc.

        A call holds 1,000 documents at most, ``bounds.SET_MANY_DOCUMENTS``, and
        16 MiB of JSON, ``bounds.SET_MANY_BYTES``. Past either it raises a
        ``LimitError`` that names the bound.
        """
        encoded = [self._encoded(each) for each in documents]
        await self._call(
            "search.setMany", lambda handle: SearchSetMany.encode(handle=handle, documents=encoded), "write"
        )

    def _encoded(self, document: tuple[Key, T] | tuple[Key, T, Vector | None] | Doc[T]) -> dict[str, Any]:
        """A document of a set_many as the wire takes it."""
        if not isinstance(document, Doc):
            document = Doc(document[0], document[1], document[2] if len(document) == 3 else _UNSET)
        return {
            "id": key_text(document.id),
            "value": _document_json(self.name, document.value),
            **_vector_said(document.vector),
            **_aliases_said(document.aliases),
        }

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

    def __init__(self, link: Link, via: Via, name: str, opener: Opener, of: Any) -> None:
        read_row = reader(of)
        super().__init__(
            link, via, name, opener, lambda hit: read_row(row_of(hit.get("columns") or [], hit.get("row") or []))
        )


@dataclass(frozen=True, slots=True)
class Among:
    """An index a search among several asks, and its search as a search of it alone says it.

    ``boost`` is what its hits weigh in ``merged``, a positive number; ``limit``
    10 unless said and ``offset`` the hits passed before the first answered,
    together 1000 at most, and the group's ``more`` says whether the index
    has more, its next page at ``offset + limit``; ``where``, ``prefix``,
    ``matches`` and ``snippet`` as a search of the index alone takes them.
    """

    index: SearchIndex[Any] | TableIndex[Any]
    boost: float | None = None
    where: Where | None = None
    limit: int | None = None
    offset: int | None = None
    prefix: bool | Literal["every"] = False
    matches: bool = False
    snippet: Mapping[str, Any] | None = None


@dataclass(frozen=True, slots=True)
class ManyGroup:
    """An index's hits, the best by its own scores first and whether it has more, or why its search failed."""

    index: str
    hits: list[Hit[Any]]
    more: bool = False
    corrected: str | None = None
    error: TinystoreError | None = None
    """why the index's search failed; its group then holds no hit"""


@dataclass(frozen=True, slots=True)
class Merged:
    """A hit of merged: its group's index, the group's place among the groups, and the hit its group holds."""

    index: str
    group: int
    hit: Hit[Any]


@dataclass(frozen=True, slots=True)
class ManyResult:
    """What a search among several indexes found: a group an index, in the order asked, and every hit in one order.

    ``merged`` is every group's hits by reciprocal rank fusion, never by score:
    the hit at rank r of its group, from 1, weighs boost / (60 + r); of equal
    weights the group asked first comes first, then the higher rank.
    """

    groups: list[ManyGroup]
    merged: list[Merged]


async def search_many(link: Link, text: str, among: Sequence[Among]) -> ManyResult:
    """Asks text of every index of among in one call over one connection, each handle opened there first."""
    asked = [_among_of(link, each) for each in among]
    for each in among:
        _refuse_around(each.index)

    async def attempt(connection: Any) -> bytes:
        openers = [each.index._opener for each in among]  # pyright: ignore[reportPrivateUsage]
        handles = [await handle_on(connection, methods["search.open"], opener) for opener in openers]
        entries = [body(handle) for body, handle in zip(asked, handles, strict=True)]
        return await connection.session.call(
            methods["search.searchMany"], SearchManyQuery.encode(text=text, among=entries)
        )

    found: dict[str, Any] = SearchManyFound.decode(await link.run("read", attempt))
    answered: list[dict[str, Any]] = found.get("groups", [])
    groups: list[ManyGroup] = [_group_of(each.index, group) for each, group in zip(among, answered, strict=True)]
    return ManyResult(groups, _merged_of(groups, found.get("merged", [])))


def _merged_of(groups: list[ManyGroup], named_all: list[dict[str, Any]]) -> list[Merged]:
    """Each hit merged names, the one its group holds; one the groups do not hold is the server's fault."""
    merged: list[Merged] = []
    for named in named_all:
        at: int = named.get("group", 0)
        hit: int = named.get("hit", 0)
        if not 0 <= at < len(groups) or not 0 <= hit < len(groups[at].hits):
            raise ProtocolError(f"a search among indexes: merged names hit {hit} of group {at}, which it does not hold")
        merged.append(Merged(groups[at].index, at, groups[at].hits[hit]))
    return merged


def _refuse_around(index: _Searchable[Any]) -> None:
    """Refuses a table's index asked from inside its database's transaction, as a search of it alone is.

    The search would wait for the writer the transaction holds until the
    store rolled the transaction back.
    """
    via = index._via  # pyright: ignore[reportPrivateUsage]
    refuse: Callable[[str], None] | None = getattr(getattr(via, "_database", None), "refuse_around", None)
    if refuse is not None:
        refuse(getattr(via, "_what", f"index {index.name}"))


def _among_of(link: Link, among: Among) -> Callable[[int], dict[str, Any]]:
    """An index's part of a search among several, once its handle is known; an index of another store is refused."""
    index = among.index
    if index._link is not link:  # pyright: ignore[reportPrivateUsage]
        raise InvalidError(f"index {index.name}: an index of another store, in a search among this one's")
    if among.prefix not in (True, False, "every"):
        raise InvalidError(f"a prefix of {among.prefix!r}: True, False or 'every'")
    statements: list[dict[str, Any]] = []
    where = None if among.where is None else where_json(among.where, statements)
    snippet = _cut(among.snippet)

    def body(handle: int) -> dict[str, Any]:
        query = {
            "handle": handle,
            "where": where,
            "statements": statements,
            "offset": among.offset,
            "limit": among.limit,
            "prefix": among.prefix is True,
            "prefix_every": True if among.prefix == "every" else None,
            # Python slices a str by its code points
            "matches": "chars" if among.matches else None,
            "snippet": snippet,
        }
        return {"query": {key: value for key, value in query.items() if value is not None}, "boost": among.boost}

    return body


def _group_of(index: _Searchable[Any], group: dict[str, Any]) -> ManyGroup:
    name = group.get("index", "")
    failure = group.get("failure")
    if failure is not None:
        error = error_of(failure.get("code", "internal"), failure.get("message", ""), failure.get("what"))
        return ManyGroup(name, [], error=error)
    hits = index._hits_of(group)  # pyright: ignore[reportPrivateUsage]
    return ManyGroup(name, hits, group.get("more", False), group.get("corrected"))


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
    layouts: Sequence[KeyboardLayout] = (),
    synonyms: Mapping[str, str] | None = None,
    vectors: Vectors | None = None,
    hybrid: Hybrid | None = None,
    rebuild: Literal["automatic", "manual"] | None = None,
    reindex: bool = False,
    database: int | None = None,
    table: str | None = None,
    key: str | Sequence[str] | None = None,
) -> bytes:
    """The open of an index, or of a table's index with its table and its database's handle."""
    check_name(name, "index")
    if rebuild not in (None, "automatic", "manual"):
        raise InvalidError(f"index {name}: a rebuild of {rebuild!r}, not automatic or manual")
    if isinstance(filters, str):
        raise InvalidError(f"index {name}: filters {filters!r}, a list of fields")
    if isinstance(layouts, str):
        raise InvalidError(f"index {name}: layouts {layouts!r}, a list of layouts")
    return SearchOpen.encode(
        name=name,
        fields=_fields_of(fields),
        filters=list(filters),
        typo_tolerance=typo_tolerance,
        # none left out, so that a server before layouts reads the open
        layouts=list(layouts) or None,
        synonyms=dict(synonyms or {}),
        dimensions=None if vectors is None else vectors.dimensions,
        recall=None if vectors is None else vectors.recall,
        vector_weight=None if hybrid is None else hybrid.vector_weight,
        rerank=None if hybrid is None else hybrid.rerank,
        manual=rebuild == "manual",
        reindex=reindex,
        database=database,
        table=table,
        # one column as key, several as keys, which a server before them ignores
        key=key if isinstance(key, str) or key is None else key[0] if len(key) == 1 else None,
        keys=list(key) if not isinstance(key, str) and key is not None and len(key) > 1 else None,
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
    error = status.get("error", "")
    if state == "rebuilding":
        return IndexStatus("rebuilding", status.get("done", 0.0), error)
    if state == "failed":
        return IndexStatus("failed", error=error)
    return IndexStatus("ready", error=error)


def _query(
    text: str,
    *,
    vector_bytes: bytes | None,
    where: str | None,
    statements: list[dict[str, Any]],
    hybrid: Hybrid | None,
    min_similarity: float | None,
    prefix: bool | Literal["every"],
    matches: bool,
    snippet: dict[str, Any] | None,
    total: bool,
    limit: int | None,
    offset: int | None,
) -> Callable[[int], bytes]:
    """A search's body, written for the handle the index is opened with on each connection."""

    def body(handle: int) -> bytes:
        return SearchQuery.encode(
            handle=handle,
            text=text,
            vector=vector_bytes,
            vector_weight=None if hybrid is None else hybrid.vector_weight,
            rerank=None if hybrid is None else hybrid.rerank,
            min_similarity=min_similarity,
            prefix=prefix is True,
            # none unless asked, so that a server before it reads the search
            prefix_every=True if prefix == "every" else None,
            # Python slices a str by its code points
            matches="chars" if matches else None,
            snippet=snippet,
            where=where,
            statements=statements,
            total=total,
            offset=offset,
            limit=limit,
        )

    return body


def _total_of(total: dict[str, Any] | None) -> Total | None:
    if total is None:
        return None
    return Total(total.get("count", 0), total.get("at_least", False))


def _cut(snippet: Mapping[str, Any] | None) -> dict[str, Any] | None:
    """The snippet a search asks for, its marks counted in code points as Python slices a str."""
    if snippet is None:
        return None
    unknown = set(snippet) - {"field", "around"}
    field, around = snippet.get("field"), snippet.get("around")
    if unknown or not isinstance(field, str):
        raise InvalidError(f'a snippet of {dict(snippet)!r}: {{"field": str, "around": int}}')
    if around is not None and (isinstance(around, bool) or not isinstance(around, int)):
        raise InvalidError(f"a snippet's around of {around!r}: the characters on each side of its match, an int")
    return {"field": field, "around": around, "unit": "chars"}


def _snippet_of(found: dict[str, Any] | None) -> Snippet | None:
    if found is None:
        return None
    marks = [Mark(mark.get("start", 0), mark.get("end", 0)) for mark in found.get("marks", [])]
    return Snippet(found.get("text", ""), marks, found.get("item"))


def _matches_of(found: list[dict[str, Any]] | None) -> list[Match] | None:
    if found is None:
        return None
    return [
        Match(each.get("field", ""), each.get("start", 0), each.get("end", 0), each.get("word", ""), each.get("item"))
        for each in found
    ]


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


def _aliases_said(aliases: Aliases | None) -> dict[str, Any]:
    """What a set says of its aliases: none left out."""
    return {"aliases": dict(aliases)} if aliases else {}


def _vector_bytes(vector: Vector) -> bytes:
    """A vector's floats' bytes, little-endian, four a dimension."""
    if isinstance(vector, array) and vector.typecode == "f" and sys.byteorder == "little":
        return vector.tobytes()
    floats = list(vector)
    try:
        return struct.pack(f"<{len(floats)}f", *floats)
    except (struct.error, OverflowError) as err:
        raise InvalidError(f"a vector of {len(floats)} values that are not all floats: {err}") from None


def where_json(where: Where, statements: list[dict[str, Any]] | None = None) -> str:
    """A where as the wire takes it, a JSON list of conditions joined by and.

    ``{"eq":[path,value]}``, ``{"in":[path,[values]]}``, ``{"missing":path}``,
    ``{"range":[path,{"gt"|"gte"|"lt"|"lte":n}]}``, ``{"or":[…]}``, ``{"and":[…]}``;
    a statement is ``{"in":[path,{"statement":n}]}``, the nth of ``statements``.
    """
    held = [] if statements is None else statements
    return json.dumps(_conditions(where, held), separators=(",", ":"), ensure_ascii=False)


def _conditions(where: Where, statements: list[dict[str, Any]]) -> list[Any]:
    if isinstance(where, Group):
        return [{where.join: [{"and": _conditions(part, statements)} for part in where.parts]}]
    return [_condition(path, value, statements) for path, value in where.items()]


def _condition(path: str, value: Any, statements: list[dict[str, Any]]) -> dict[str, Any]:
    from .sql import Sql  # sql imports this module as it loads

    if isinstance(value, Sql):
        statements.append({"text": value.text, "values": list(value.values)})
        return {"in": [path, {"statement": len(statements) - 1}]}
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
