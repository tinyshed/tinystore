# Search API for Python

Every public class, function and type of the Search engine in `tinystore`, generated from its source. The [Search guide](../../guides/search/README.md) explains how to use them, and the [Bun and Node](../bun/search.md), [Go](../go/search.md) and [Rust](../rust/search.md) pages list the same API.

## Vectors

```python
@dataclass(frozen=True, slots=True)
class Vectors:
    dimensions: int
    recall: float | None = None
    column: str | None = None
```

A vector the program's model gives each document, and recall for searches from clusters.

A table's index names the column of blobs that holds them.

## Hybrid

```python
@dataclass(frozen=True, slots=True)
class Hybrid:
    vector_weight: float | None = None
    rerank: bool | None = None
```

How a search by words and a vector at once combines them: a vector weight, or the words' best reranked.

## Match

```python
@dataclass(frozen=True, slots=True)
class Match:
    field: str  # the field's path, or a table's column; "" for a document that is its own text
    start: int
    end: int
    word: str  # the query's word, as its text wrote it
    item: int | None = None  # the string's place in a field that holds a list
```

Where a search's word matched a hit: ``text[start:end]`` of a field's text, in code points.

## Mark

```python
@dataclass(frozen=True, slots=True)
class Mark:
    start: int
    end: int
```

Where a search's word matched inside a snippet: ``snippet.text[start:end]``, in code points.

## Snippet

```python
@dataclass(frozen=True, slots=True)
class Snippet:
    text: str
    marks: list[Mark]
    item: int | None = None  # the string's place in a field that holds a list
```

The piece of a field around a hit's first match there, the field's own text, … where it was cut.

Its marks are every match inside it. The store adds no markup and escapes nothing.

## Hit

```python
@dataclass(frozen=True, slots=True)
class Hit[T]:
    id: str
    value: T
    score: float
    matches: list[Match] | None = None  # where the search's words matched it, when the search asked with matches
    text_score: float | None = None  # the score its words gave it, as a search by its words alone scores it, where they found it among their best
    similarity: float | None = None  # the cosine of its vector with the search's, from -1 to 1, where both have one
    snippet: Snippet | None = None  # the piece of a field around its first match, when the search asked with snippet and it holds the field
```

A document a search found, and the score that ordered it among this search's hits.

## Result

```python
@dataclass(frozen=True, slots=True)
class Result[T]:
    hits: list[Hit[T]]
    corrected: str | None = None
    total: Total | None = None  # how many it finds, when the search asked with total
```

What a search found, the best first, and the text it read in fact when a word was misspelt.

## Total

```python
@dataclass(frozen=True, slots=True)
class Total:
    count: int
    at_least: bool  # whether counting stopped at 10,000, more being there
```

How many documents a search finds: exactly, or past 10,000, 10,000 and at_least, never an estimate.

## IndexStatus

```python
@dataclass(frozen=True, slots=True)
class IndexStatus:
    state: Literal['ready', 'rebuilding', 'failed']
    done: float = 0.0
    error: str = ''
```

Where an index stands: ready, rebuilding with how much is done, or a rebuild that failed and why.

``error`` beside ``ready`` and ``rebuilding`` is the last maintenance pass's failure while the passes keep failing.

## IndexInfo

```python
@dataclass(frozen=True, slots=True)
class IndexInfo:
    name: str
    documents: int
    vectors: int
    opened: datetime | None  # when a program last opened it
    status: IndexStatus
```

An index as ``store.indexes()`` lists it.

## SearchIndex

```python
class SearchIndex[T](_Searchable[T]):
    ...
```

Documents of one type by id, found again by their words, vectors or both: ``store.index(name, Article)``.

### SearchIndex.set

```python
async def set(
    id: Key,
    value: T,
    *,
    vector: Vector | None = _UNSET,
    aliases: Aliases | None = None,
) -> None: ...
```

Sets the document id to value, replacing it whole; the next search sees it.

On an index with vectors a set gives its vector, or None for a
document only its words find. Its aliases, ``{"кмзи": "name"}``,
find it as a match of the field each stands for and mark that
field's text; a set without them leaves the document none.

### SearchIndex.set_many

```python
async def set_many(
    documents: Sequence[tuple[Key, T] | tuple[Key, T, Vector | None] | Doc[T]],
) -> None: ...
```

Sets every document in one commit, all of them or none: (id, value), (id, value, vector) or a Doc.

A call holds 1,000 documents at most, ``bounds.SET_MANY_DOCUMENTS``, and no
more than one request body holds, 2 MiB, ``bounds.REQUEST_BYTES``. Past
either it raises a ``LimitError`` that names the bound.

### SearchIndex.delete

```python
async def delete(id: Key) -> bool: ...
```

Deletes the document id; says whether there was one.

### SearchIndex.get

```python
async def get(id: Key) -> T | None: ...
```

The document id, or None.

## TableIndex

```python
class TableIndex[T](_Searchable[T]):
    ...
```

A table's rows found again by their words, vectors or both, each hit's value its row as ``db.all`` reads it.

The table owns the rows, and every change of one reaches the index in the
commit that makes it.

## Among

```python
@dataclass(frozen=True, slots=True)
class Among:
    index: SearchIndex[Any] | TableIndex[Any]
    boost: float | None = None
    where: Where | None = None
    limit: int | None = None
    offset: int | None = None
    prefix: bool | Literal['every'] = False
    matches: bool = False
    snippet: Mapping[str, Any] | None = None
```

An index a search among several asks, and its search as a search of it alone says it.

``boost`` is what its hits weigh in ``merged``, a positive number; ``limit``
10 unless said and ``offset`` the hits passed before the first answered,
together 1000 at most, and the group's ``more`` says whether the index
has more, its next page at ``offset + limit``; ``where``, ``prefix``,
``matches`` and ``snippet`` as a search of the index alone takes them.

## ManyGroup

```python
@dataclass(frozen=True, slots=True)
class ManyGroup:
    index: str
    hits: list[Hit[Any]]
    more: bool = False
    corrected: str | None = None
    error: TinystoreError | None = None  # why the index's search failed; its group then holds no hit
```

An index's hits, the best by its own scores first and whether it has more, or why its search failed.

## Merged

```python
@dataclass(frozen=True, slots=True)
class Merged:
    index: str
    group: int
    hit: Hit[Any]
```

A hit of merged: its group's index, the group's place among the groups, and the hit its group holds.

## ManyResult

```python
@dataclass(frozen=True, slots=True)
class ManyResult:
    groups: list[ManyGroup]
    merged: list[Merged]
```

What a search among several indexes found: a group an index, in the order asked, and every hit in one order.

``merged`` is every group's hits by reciprocal rank fusion, never by score:
the hit at rank r of its group, from 1, weighs boost / (60 + r); of equal
weights the group asked first comes first, then the higher rank.

<!-- Generated by task reference from sdk/python/src/tinystore/search.py. Edit the doc comments there, not this file. -->
