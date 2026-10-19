# Search API for Bun and Node

Every public class, function and type of the Search engine in `@tinyshed/tinystore`, generated from its source. The [Search guide](../../sql/search.md) explains how to use them, and the [Python](../python/search.md), [Go](../go/search.md) and [Rust](../rust/search.md) pages list the same API.

## IndexOptions

```ts
interface IndexOptions {
    /** the fields whose words are read, each with what a word found there weighs, or a list that weighs each 1 */
    fields?: Record<string, number> | readonly string[] | undefined
    /** the fields a search may keep its hits to */
    filters?: readonly string[] | undefined
    /** a misspelt word read as the index's nearest; on unless false */
    typoTolerance?: boolean | undefined
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

## Vector

```ts
type Vector = readonly number[] | Float32Array
```

A vector the program's own model gave, of the index's dimensions.

## WhereValue

```ts
type WhereValue = string | number | bigint | boolean | null | readonly (string | number | bigint | boolean)[] | {
    gt?: number | bigint
    gte?: number | bigint
    lt?: number | bigint
    lte?: number | bigint
}
```

A value a `where` matches: text, a number, a boolean, one of a list, a range, or `null` for none.

## Where

```ts
type Where = Readonly<Record<string, WhereValue>> | Group
```

What a search keeps its hits to, as sqldb's `where` reads an object: each
field equal to its value, one of a list, within a range, or `null` for
none, every field named passing; `search.or` and `search.and` group them.

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
    /** 10 unless said; offset and limit together 1000 at most */
    limit?: number | undefined
    offset?: number | undefined
}
```

What a search asks: its words, its vector, or both, and what it keeps to.

## Hit

```ts
interface Hit<T> {
    id: string
    value: T
    score: number
}
```

A document a search found, and the score that ordered it among this search's hits.

## Found

```ts
interface Found<T> {
    hits: Hit<T>[]
    corrected?: string | undefined
}
```

What a search found, the best first, and the text it read in fact when a word was misspelt.

## IndexStatus

```ts
type IndexStatus = {
    state: 'ready'
} | {
    state: 'rebuilding'
    done: number
} | {
    state: 'failed'
    error: string
}
```

Where an index stands: serving what it was opened with, read again beside it, or a rebuild that failed.

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
    /** the column of unique values a row is found by, the table's primary key unless said */
    key?: string | undefined
    /** the columns whose words are read, each with what a word found there weighs, or a list that weighs each 1 */
    fields: Record<string, number> | readonly string[]
    /** the columns a search may keep its hits to; a boolean is the 1 or 0 SQLite keeps */
    filters?: readonly string[] | undefined
    typoTolerance?: boolean | undefined
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

Sets every document in one commit, all of them or none: 1,000 and 16 MiB at most.

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

## search

```ts
const search: {
    or: (...parts: Where[]) => Group
    and: (...parts: Where[]) => Group
}
```

The conditions a `where` groups: `search.or({ category: 'boots' }, { tags: 'sale' })`.

<!-- Generated by task reference from sdk/js/src/search.ts. Edit the doc comments there, not this file. -->
