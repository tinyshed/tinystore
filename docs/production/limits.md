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

| What               | Limit or default          |
|--------------------|---------------------------|
| a document         | 1 MiB                     |
| a search's text    | 1 KiB, its first 32 words |
| a vector           | 4,096 dimensions          |
| `limit` + `offset` | 1,000; `limit` is 10      |
| a field's weight   | above 0, at most 1,000    |
| a filter           | 8 conditions, 100 values  |
| `vectorWeight`     | 0.5                       |

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

## See also

- [One budget of memory](../inside/memory.md): how bounds and the budget fit
  together.
- [What TinyStore promises](promises.md): the contracts behind the bounds.
