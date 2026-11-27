# Search API for Bun and Node

Every public class, function and type of the Search engine in `@tinyshed/tinystore`, generated from its source. The [Search guide](../../guides/search/README.md) explains how to use them, and the [Python](../python/search.md), [Go](../go/search.md) and [Rust](../rust/search.md) pages list the same API.

## IndexOptions

```ts
interface IndexOptions {
    /** the fields whose words are read, each with what a word found there weighs, or a list that weighs each 1 */
    fields?: Record<string, number> | readonly string[] | undefined
    /** the fields a search may keep its hits to */
    filters?: readonly string[] | undefined
    /** a misspelt word read as the index's nearest; on unless false */
    typoTolerance?: boolean | undefined
    /**
     * the keyboard layouts the index's readers type with, `['ru', 'en']`: a
     * word that finds nothing as typed is read as the same keys type in the
     * other layout, when it finds something there, and `corrected` says so
     */
    layouts?: readonly KeyboardLayout[] | undefined
    /**
     * each short form, one word, with the phrase of 1 to 8 words it stands
     * for, `{ ml: 'machine learning' }`: a search for the short form finds a
     * document that holds the phrase, its words adjacent and in order, and a
     * search for the phrase's words one that holds the short form; read when
     * a document is set, so a search pays nothing; other synonyms read an
     * index that holds documents again, and none given leaves it none
     */
    synonyms?: Readonly<Record<string, string>> | undefined
    /** a vector the program's model gives each document, and `recall` for searches from clusters */
    vectors?: {
        dimensions: number
        recall?: number | undefined
    } | undefined
    /** how a search by words and a vector at once combines them unless it says */
    hybrid?: {
        vectorWeight?: number | undefined
        rerank?: boolean | undefined
    } | undefined
    /** whether a change that needs every document read again waits for `store.reindex` */
    rebuild?: 'automatic' | 'manual' | undefined
}
```

How an index reads its documents and combines its halves, given where it opens.

## KeyboardLayout

```ts
type KeyboardLayout = 'ru' | 'en'
```

A keyboard layout an index's readers type with: ЙЦУКЕН, or QWERTY.

## Vector

```ts
type Vector = readonly number[] | Float32Array
```

A vector the program's own model gave, of the index's dimensions.

## WhereValue

```ts
type WhereValue = Sql | string | number | bigint | boolean | null | readonly (string | number | bigint | boolean)[] | {
    gt?: number | bigint
    gte?: number | bigint
    lt?: number | bigint
    lte?: number | bigint
}
```

A value a `where` matches: text, a number, a boolean, one of a list, a
range, `null` for none, or, for a table's index, one of the values a
statement of its database gives: ``sql`select user_id from members where course_id = ${id}` ``.

## Where

```ts
type Where = Readonly<Record<string, WhereValue>> | Group
```

What a search keeps its hits to, as sqldb's `where` reads an object: each
field equal to its value, one of a list, within a range, or `null` for
none, every field named passing; `search.or` and `search.and` group them.
A search's where holds 8 conditions and 100 values in its lists at most.
A statement's rows, read in the search's snapshot, count toward no bound
on values: 100,000 at most, and on the table's key no filter is needed.

## Query

```ts
interface Query {
    text?: string | undefined
    vector?: Vector | undefined
    where?: Where | undefined
    /** this search's way of combining its words and its vector, in place of the index's */
    hybrid?: {
        vectorWeight?: number | undefined
        rerank?: boolean | undefined
    } | undefined
    /**
     * the least cosine, from -1 to 1, a document's vector has with the
     * search's for the vector to find it: a search by its vector alone leaves
     * out the hits under it, and one by words too adds nothing for them to
     * the sum; invalid beside a rerank
     */
    minSimilarity?: number | undefined
    /**
     * the last word read as the start of a word too, as a search box types:
     * `crypt` finds `cryptography` as well, at half its field's weight; a
     * word a space follows is whole, and so is one of a single letter. With
     * `'every'`, every word of two letters or more is read so, the first
     * eight, as people shorten each word of a name: `лин алг` finds
     * `Линейная алгебра`; a document that holds a word for each word of the
     * text comes first, its score raised above the rest
     */
    prefix?: boolean | 'every' | undefined
    /**
     * each hit says where the search's words matched it, in `matches`, as a
     * search box shows them in bold: a word found as written, by its stem or
     * once corrected whole, and a word a prefix began by the letters typed
     */
    matches?: boolean | undefined
    /**
     * each hit's `snippet` of `field`, a path the index reads: its own text
     * from `around` characters before the field's first match to `around`
     * after it, 1 to 1000 and 80 unless said, cut where a word starts and
     * ends and `…` where text was cut; a hit that matched elsewhere, or by
     * its vector alone, gets the field's first `2 × around` characters
     */
    snippet?: {
        field: string
        around?: number | undefined
    } | undefined
    /**
     * counts the documents it finds as well, in `total`, under its `where`
     * and whatever its `limit` and `offset`: exactly up to 10,000, and past
     * it 10,000 with `atLeast`; it reads every posting of its words, which a
     * search alone skips most of; invalid beside a vector
     */
    total?: boolean | undefined
    /** 10 unless said; offset and limit together 1000 at most */
    limit?: number | undefined
    offset?: number | undefined
}
```

What a search asks: its words, its vector, or both, and what it keeps to.

## Match

```ts
interface Match {
    /** the field's path, or a table's column; `''` for a document that is its own text */
    field: string
    /** the string's place in a field that holds a list */
    item?: number
    start: number
    end: number
    /** the query's word, as its text wrote it */
    word: string
}
```

Where a search's word matched a hit: `start` to `end` of a field's text in
UTF-16 units, so that `text.slice(start, end)` is what matched.

## Snippet

```ts
interface Snippet {
    text: string
    marks: {
        start: number
        end: number
    }[]
    /** the string's place in a field that holds a list */
    item?: number
}
```

The piece of a field around a hit's first match there: the field's own
text, `…` where it was cut, and every match inside it as `start` to `end`
in UTF-16 units of `text`. The store adds no markup and escapes nothing.

## Hit

```ts
interface Hit<T> {
    id: string
    value: T
    score: number
    /** where the search's words matched it, when the search asked with `matches` */
    matches?: Match[]
    /** the piece of a field around its first match, when the search asked with `snippet` and it holds the field */
    snippet?: Snippet
    /** the score its words gave it, as a search by its words alone scores it, where they found it among their best */
    textScore?: number
    /** the cosine of its vector with the search's, from -1 to 1, where both have one */
    similarity?: number
}
```

A document a search found, and the score that ordered it among this search's hits.

## Found

```ts
interface Found<T> {
    hits: Hit<T>[]
    corrected?: string | undefined
    /** how many it finds, when the search asked with `total` */
    total?: Total | undefined
}
```

What a search found, the best first, and the text it read in fact when a word was misspelt.

## Total

```ts
interface Total {
    count: number
    /** whether counting stopped at 10,000, more being there */
    atLeast: boolean
}
```

How many documents a search finds: exactly, or past 10,000, 10,000 and `atLeast`, never an estimate.

## IndexStatus

```ts
type IndexStatus = {
    state: 'ready'
    error?: string | undefined
} | {
    state: 'rebuilding'
    done: number
    error?: string | undefined
} | {
    state: 'failed'
    error: string
}
```

Where an index stands: serving what it was opened with, read again beside it, or a rebuild that failed.
`error` beside `ready` and `rebuilding` is the last maintenance pass's failure while the passes keep failing.

## IndexInfo

```ts
interface IndexInfo {
    name: string
    documents: number
    vectors: number
    /** when a program last opened it */
    opened?: Date | undefined
    status: IndexStatus
}
```

An index as `store.indexes()` lists it.

## TableIndexOptions

```ts
interface TableIndexOptions {
    /** the table whose rows the index holds and follows */
    table: string
    /**
     * the column of unique values a row is found by, or the columns whose
     * values are unique together, `['space_id', 'n']`: the table's primary
     * key unless said; a hit's `id` is a key of several columns' values as a
     * JSON array, `'[1,6]'`
     */
    key?: string | readonly string[] | undefined
    /** the columns whose words are read, each with what a word found there weighs, or a list that weighs each 1 */
    fields: Record<string, number> | readonly string[]
    /** the columns a search may keep its hits to; a boolean is the 1 or 0 SQLite keeps */
    filters?: readonly string[] | undefined
    typoTolerance?: boolean | undefined
    layouts?: readonly KeyboardLayout[] | undefined
    synonyms?: Readonly<Record<string, string>> | undefined
    /** a column of blobs of little-endian floats, one a row or null, and `recall` for searches from clusters */
    vectors?: {
        column: string
        dimensions: number
        recall?: number | undefined
    } | undefined
    hybrid?: {
        vectorWeight?: number | undefined
        rerank?: boolean | undefined
    } | undefined
    rebuild?: 'automatic' | 'manual' | undefined
}
```

How a table's index reads a table of its database: its columns of text,
those a search may filter by, and a column of vectors the program fills.

## IndexDocument

```ts
interface IndexDocument<T> {
    id: Key
    value: T
    vector?: Vector | null | undefined
}
```

A document `setMany` sets: its id, its value, and its vector, or `null` for none.

## SearchIndex

```ts
class SearchIndex<T> extends Searchable<T> {}
```

Documents of one type by id, found again by their words, vectors or both: `store.index(name, options)`.

### SearchIndex.set

```ts
set(id: Key, value: T, options?: {
        vector?: Vector | null | undefined
    }): Promise<void>
```

Sets the document `id` to `value`, replacing it whole; the next search
sees it. On an index with vectors a set gives its vector, or `null` for
a document only its words find.

### SearchIndex.setMany

```ts
setMany(documents: readonly IndexDocument<T>[]): Promise<void>
```

Sets every document in one commit, all of them or none: 1,000 at most,
`limits.setManyDocuments`, and no more than one request body holds,
2 MiB, `limits.requestBytes`. Past either it fails with a LimitError
that names the bound.

### SearchIndex.delete

```ts
delete(id: Key): Promise<boolean>
```

Deletes the document `id`; says whether there was one.

### SearchIndex.get

```ts
get(id: Key): Promise<T | undefined>
```

The document `id`, or undefined.

## TableIndex

```ts
class TableIndex<T> extends Searchable<T> {}
```

A table's rows found again by their words, vectors or both, each hit's
value its row as `db.all` reads it: `db.searchIndex(name, options)`. The
table owns the rows, and every change of one reaches the index in the
commit that makes it.

## Among

```ts
interface Among {
    index: SearchIndex<unknown> | TableIndex<unknown>
    /** what its hits weigh in `merged`, a positive number, 1 unless said */
    boost?: number | undefined
    where?: Where | undefined
    /** 1 to 100, 10 unless said; `more` says whether the index has more */
    limit?: number | undefined
    prefix?: boolean | 'every' | undefined
    matches?: boolean | undefined
    snippet?: {
        field: string
        around?: number | undefined
    } | undefined
}
```

An index a search among several asks, and its search as a search of it alone says it.

## ManyQuery

```ts
interface ManyQuery {
    text: string
    in: readonly Among[]
}
```

One question of several indexes: the text every one reads, and 1 to 32 indexes in order.

## ManyGroup

```ts
type ManyGroup = {
    index: string
    hits: Hit<unknown>[]
    more: boolean
    corrected?: string | undefined
} | {
    index: string
    error: TinystoreError
}
```

An index's hits, the best by its own scores first and whether it has more, or why its search failed.

## Merged

```ts
interface Merged {
    index: string
    group: number
    hit: Hit<unknown>
}
```

A hit of `merged`: its group's index, the group's place among the groups, and the hit its group holds.

## ManyFound

```ts
interface ManyFound {
    groups: ManyGroup[]
    /**
     * every group's hits by reciprocal rank fusion, never by score: the hit
     * at rank `r` of its group, from 1, weighs `boost / (60 + r)`; of equal
     * weights the group asked first comes first, then the higher rank
     */
    merged: Merged[]
}
```

What a search among several indexes found: a group an index, in the order asked, and every hit in one order.

## search

```ts
const search: {
    or: (...parts: Where[]) => Group
    and: (...parts: Where[]) => Group
}
```

The conditions a `where` groups: `search.or({ category: 'boots' }, { tags: 'sale' })`.

<!-- Generated by task reference from sdk/js/src/search.ts. Edit the doc comments there, not this file. -->
