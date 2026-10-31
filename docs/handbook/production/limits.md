# Limits and defaults

Every bound and default of TinyStore in one place, by engine. A call past a
bound fails with a `limit` or `invalid` error that names the bound, and
changes nothing. Each chapter of the book has the same numbers beside the
calls they bound.

## The store

| What                                                       | Limit or default                                                     |
|------------------------------------------------------------|----------------------------------------------------------------------|
| durability                                                 | `'full'` for kv, jobs, sql and blobs; `'os'` for records and metrics |
| memory budget                                              | none unless set; 1 GiB for `tinystore serve --listen`                |
| a call waiting for memory                                  | 10 seconds, then `limit`                                             |
| free disk the store keeps                                  | 1 GiB, `keepFree`                                                    |
| a time in the future                                       | 10 minutes ahead of the store's clock                                |
| a sidecar with no clients                                  | exits after 30 seconds                                               |
| a name of a bucket, queue, stream, database, index, config | `[a-z0-9][a-z0-9_-]{0,63}`                                           |

## SQL

| What                               | Limit or default                                           |
|------------------------------------|------------------------------------------------------------|
| a transaction                      | 5 seconds                                                  |
| rows `all` or `columns` holds      | 64 MiB of values                                           |
| one `each` read                    | 5 seconds of one snapshot                                  |
| writes in one shared commit        | 1,024, or 8 MiB                                            |
| readers                            | one per processor, 4 to 16, idle ones close after a minute |
| statements compiled per connection | 128                                                        |

## KV

| What                               | Limit or default                            |
|------------------------------------|---------------------------------------------|
| a key, with its owners             | 1 KiB                                       |
| a value                            | 1 MiB                                       |
| a page of `list`                   | 1,000 keys                                  |
| a quota                            | one to eight windows                        |
| rate limits and counters in memory | 100,000 keys per name, written every second |
| an idle key's renewal              | once a thirtieth of its span has passed     |

## Jobs

| What                     | Limit or default                               |
|--------------------------|------------------------------------------------|
| `attempts`               | 10                                             |
| `backoff`                | from 1 s, doubling, up to 1 h, ±10 % at random |
| `timeout`                | 1 minute per run                               |
| a lease                  | 30 seconds, renewed every 15 while a run lasts |
| `keep` of failed jobs    | 7 days                                         |
| waiting jobs             | 10,000,000 per queue, `maxWaiting`             |
| an id, a group           | 1 to 1,024 bytes                               |
| a value, a step's answer | 1 MiB of JSON                                  |
| progress                 | 4 KiB of JSON                                  |
| a page                   | 1,000 jobs or 4 MiB                            |

## Blobs

| What                    | Limit or default                                 |
|-------------------------|--------------------------------------------------|
| a file                  | none, unless `maxFileSize`                       |
| a path with its folders | 1 KiB and 16 segments                            |
| a content type          | 256 bytes                                        |
| `meta`                  | 2 KiB in all                                     |
| a page                  | 1,000 files                                      |
| a file kept as a row    | up to 16 KiB; larger ones are files of their own |

## Search

| What               | Limit or default                                                              |
|--------------------|-------------------------------------------------------------------------------|
| a document         | 1 MiB                                                                         |
| `setMany`          | 1,000 documents; 16 MiB of JSON in Rust, one request body, 2 MiB, from an SDK |
| a search's text    | 1 KiB, its first 32 words                                                     |
| a vector           | 4,096 dimensions                                                              |
| `limit` + `offset` | 1,000; `limit` is 10                                                          |
| a field's weight   | above 0, at most 1,000                                                        |
| a filter           | 8 conditions, 100 values                                                      |
| `vectorWeight`     | 0.5                                                                           |

## Channels and locks

| What                          | Limit or default                   |
|-------------------------------|------------------------------------|
| a message                     | 1 MiB                              |
| a branch's log                | its newest 1,024 messages or 4 MiB |
| every channel's messages      | 64 MiB of the store's memory       |
| a store's subscriptions       | 10,000                             |
| a connection's pending output | 8 MiB                              |
| a lock's `wait`               | 10 seconds                         |
| `maxHold`                     | none                               |

## Records and metrics

| What                          | Limit or default                           |
|-------------------------------|--------------------------------------------|
| a stream's `keep`             | 14 days                                    |
| a logger's buffer             | 1,024 lines, written every second          |
| a page of events              | 1,000, at most 10,000                      |
| a name's `keep` in metrics    | 30 days                                    |
| instruments' writes           | every 15 seconds, at `flush` and at close  |
| sets of labels per instrument | 2,000 between two sends, then `__overflow` |
| series in a store             | 100,000                                    |
| buckets of an aggregate       | 100,000                                    |

## Over the wire

Bun, Node, Python and Go send each call to the core as one request. A
request is at most 2 MiB, the body that every SDK agrees with the core when
it connects. The SDK checks the size before it sends the call, so a larger
request fails at once and changes nothing. The error names the call:

```text
search.setMany: a request of 2203997 bytes, past the 2097152 a body holds
```

This is the bound that a `setMany` of long documents meets first. For
example, 1,000 documents of 3 KiB each are 3 MiB together. A Rust program
calls the engines directly, so it never meets this bound.

## Bounds by name

Each bound that one call meets has a name. When a call passes it, the limit
error (`LimitError` in Bun and Python, `ErrLimit` in Go, `ErrorKind::Limit`
in Rust) carries three facts:

- `limit`, the bound's name, for example `set_many documents`
- `wanted`, what the call would have taken of it, for example 1001
- `bound`, the most it takes, for example 1000

Each SDK exports the same bounds, so a program can size its batches before
it calls, and can tell which bound an error met without reading its
message:

```ts
import { limits } from '@tinyshed/tinystore'

const courses = store.index<Course>('courses', { fields: { name: 1 } })
const most = limits.setManyDocuments.bound
for (let at = 0; at < catalogue.length; at += most) {
	await courses.setMany(catalogue.slice(at, at + most))
}
```

```python
from tinystore import bounds

courses = store.index("courses", Course, fields={"name": 1})
most = bounds.SET_MANY_DOCUMENTS.bound
for at in range(0, len(catalogue), most):
    await courses.set_many(catalogue[at : at + most])
```

```go
courses, err := search.OpenIndex[Course](store, "courses", search.Fields{"name": 1})
for batch := range slices.Chunk(catalogue, int(limits.SetManyDocuments.Bound)) {
	if err := courses.SetMany(ctx, batch); err != nil {
		return err
	}
}
```

```rust
use tinystore::limits::SET_MANY_DOCUMENTS;

let courses = store.index::<Course>("courses").field("name", 1.0).open()?;
for batch in catalogue.chunks(SET_MANY_DOCUMENTS.bound as usize) {
    courses.set_many(batch.iter().map(|(id, course)| (id, course)))?;
}
```

The bounds live in `limits` in Bun and Node, in `tinystore.bounds` in
Python, in the `limits` package in Go and in `tinystore::limits` in Rust.
Python's `tinystore.limits` is a different module: rate limits and quotas.
Each constant has the name below in its language's case:
`limits.setManyDocuments`, `bounds.SET_MANY_DOCUMENTS`,
`limits.SetManyDocuments` and `limits::SET_MANY_DOCUMENTS`.

| Constant             | The error's `limit`      | Bound     | What it bounds                                                     |
|----------------------|--------------------------|-----------|--------------------------------------------------------------------|
| `VALUE_BYTES`        | `value bytes`            | 1 MiB     | a bucket's value                                                   |
| `SETTING_BYTES`      | `setting bytes`          | 16 KiB    | a live setting's change, as JSON                                   |
| `CONFIG_BYTES`       | `config bytes`           | 256 KiB   | every change a config keeps                                        |
| `LOCK_WAITERS`       | `lock waiters`           | 10,000    | callers waiting for one lock or semaphore                          |
| `SUBSCRIPTIONS`      | `subscriptions`          | 10,000    | subscriptions to the store's channels                              |
| `JOB_BYTES`          | `job bytes`              | 1 MiB     | a job's payload, result or step's answer, as JSON                  |
| `PROGRESS_BYTES`     | `progress bytes`         | 4 KiB     | a run's progress, as JSON                                          |
| `ROWS_BYTES`         | `bytes of a call's rows` | 64 MiB    | the values one call reads from a database                          |
| `EVENT_BYTES`        | `event bytes`            | 256 KiB   | one event                                                          |
| `APPEND_BYTES`       | `append bytes`           | 4 MiB     | the events of one append                                           |
| `AGGREGATE_ROWS`     | `aggregate rows`         | 10,000    | the rows of one `aggregate` of events                              |
| `COUNT_BYTES`        | `count bytes`            | 64 MiB    | the different values one `top` or `distinct` holds                 |
| `INGEST_SAMPLES`     | `ingest samples`         | 10,000    | the samples of one `ingest`                                        |
| `INGEST_BYTES`       | `ingest bytes`           | 4 MiB     | the samples of one `ingest` with their labels                      |
| `TEXT_SAMPLES`       | `text samples`           | 100,000   | the samples of one exporter's text                                 |
| `TEXT_BYTES`         | `text bytes`             | 32 MiB    | one exporter's text                                                |
| `QUERY_SERIES`       | `series`                 | 1,000     | the series one metrics query finds                                 |
| `QUERY_BYTES`        | `bytes read`             | 16 MiB    | what one metrics query reads from the file                         |
| `QUERY_SAMPLES`      | `samples decoded`        | 1,048,576 | the samples one metrics query decodes                              |
| `QUERY_ANSWER`       | `answer`                 | 100,000   | the samples or buckets one metrics query answers                   |
| `SET_MANY_DOCUMENTS` | `set_many documents`     | 1,000     | the documents of one `setMany`                                     |
| `SET_MANY_BYTES`     | `set_many bytes`         | 16 MiB    | the documents of one `setMany` as JSON; over the wire, 2 MiB first |
| `DOCUMENT_BYTES`     | `document bytes`         | 1 MiB     | one document as JSON, or a table's row as its index reads it       |
| `SEARCH_HITS`        | `search hits`            | 1,000     | `offset` and `limit` of one search together                        |
| `SEARCH_TEXT`        | `search text`            | 1 KiB     | a search's text                                                    |
| `FILTER_CONDITIONS`  | `filter conditions`      | 8         | the conditions of one search's filter                              |
| `FILTER_VALUES`      | `filter values`          | 100       | the values in the lists of one search's conditions                 |
| `FILTER_TEXT`        | `filter text`            | 256 bytes | a text that a filter keeps or asks for                             |
| `VECTOR_DIMENSIONS`  | `vector dimensions`      | 4,096     | a vector                                                           |
| `REQUEST_BYTES`      | `request bytes`          | 2 MiB     | one request from an SDK                                            |
| `ANSWER_BYTES`       | `answer bytes`           | 2 MiB     | one answer to an SDK                                               |

A bound that the store's options set is named the same way, but it has no
constant: `store memory` and `store memory, now` for the memory budget,
`series in a store` for `maxSeries`, `waiting jobs` for a queue's
`maxWaiting`, and `file bytes` for a set of files' `maxFileSize`. The error's
`bound` gives the number that this store uses.

`testdata/wire/limits.json` holds the table, and each SDK's tests check
their constants against it.

## See also

- [One budget of memory](../inside/memory.md): how bounds and the budget fit
  together.
- [What TinyStore promises](promises.md): the contracts behind the bounds.
