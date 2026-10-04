# Limits and defaults

Every limit and default of TinyStore in one place, by engine. Defaults marked
as options can be changed when you open the engine, in Go with its `Options`
or the bucket or queue options, and in Bun and Python with the options of the
bucket or queue. A query can lower its own limits, but not raise them.

## The store

|                                             | Default                                                            |
|---------------------------------------------|--------------------------------------------------------------------|
| Memory budget for all engines               | none in Go and a local sidecar; 1 GiB for a server with `--listen` |
| Clock skew accepted for records and metrics | 10 minutes                                                         |
| Maintenance                                 | every minute, unless the store is `Manual`                         |
| A database or bucket name                   | `[a-z0-9][a-z0-9_-]{0,63}`                                         |

## KV

|                                    | Limit or default                                                              |
|------------------------------------|-------------------------------------------------------------------------------|
| A key, including its branch        | 1 KiB                                                                         |
| A value                            | 1 MiB                                                                         |
| A `scan` page                      | 1,000 keys or 4 MiB of values; 100 by default                                 |
| Default TTL                        | none                                                                          |
| Sliding expiry extension           | at most once per 1/30 of the sliding time                                     |
| Expired keys deleted               | up to 600,000 per minute                                                      |
| `clear` in one transaction         | 10,000 keys; larger branches are hidden at once and deleted in the background |
| A view's snapshot                  | 5 seconds                                                                     |
| Counters in memory (`loseAtMost`)  | 100,000 per bucket                                                            |
| Rate limiter state written to disk | every second                                                                  |
| Quota windows                      | 1 to 8, names `[a-z][a-z0-9_]{0,31}`                                          |
| A `once` result                    | kept 1 day                                                                    |
| A config setting                   | path 256 bytes, value 16 KiB of JSON; 256 KiB of stored changes               |

## Jobs

|                           | Limit or default                              |
|---------------------------|-----------------------------------------------|
| A value                   | 1 MiB of JSON                                 |
| A key                     | 1 to 1,024 bytes                              |
| Lease                     | 30 seconds                                    |
| Handler timeout           | 1 minute                                      |
| Attempts                  | 10                                            |
| Retry delay               | 1 second, doubling up to 1 hour, ±10%         |
| Waiting jobs per queue    | 10,000,000                                    |
| Failed jobs kept          | 7 days                                        |
| Done keys kept            | none, unless `keepDone` is set                |
| Running jobs per queue    | unlimited, unless `maxRunning` is set         |
| A progress report         | 4 KiB of JSON                                 |
| Jobs counted in `ahead`   | 10,000                                        |
| A step's name; its result | 256 bytes; 1 MiB of JSON                      |
| A `scan` page             | 1,000 jobs or 4 MiB of values; 100 by default |

## Blobs

|                              | Limit or default                               |
|------------------------------|------------------------------------------------|
| A path, including its folder | 1 KiB and 16 segments                          |
| A file                       | no limit, unless the bucket sets `maxSize`     |
| Stored inside the database   | files up to 16 KiB                             |
| Content type                 | 256 bytes                                      |
| Meta                         | 2 KiB; keys `[a-z0-9][a-z0-9_-]{0,63}`         |
| Free disk space kept         | 1 GiB                                          |
| Memory per upload            | 16 KiB, up to 80 KiB briefly for a long upload |
| Uploads at the same time     | 1,024                                          |
| `clear` in one transaction   | 10,000 files                                   |
| Scrub                        | every file once per 30 days                    |
| A `scan` page                | 1,000 files; 100 by default                    |

## SQL

|                                    | Limit or default |
|------------------------------------|------------------|
| Rows `all` returns                 | 64 MiB of values |
| A view or `each` snapshot          | 5 seconds        |
| Writes committed together          | 1,024, or 8 MiB  |
| A group's hold on the writer       | 10 seconds       |
| Reader connections                 | 8                |
| Compiled statements per connection | 128              |
| A `data` client's statement        | 30 seconds       |

## Records

|                   | Limit or default                                     |
|-------------------|------------------------------------------------------|
| Retention         | 14 days                                              |
| A record          | 256 KiB; 128 context fields and 128 attributes       |
| An `append`       | 4 MiB                                                |
| A page            | 1,000 records by default, at most 10,000             |
| A logger's buffer | 1,024 lines                                          |
| Logger writes     | every second, or when the buffer is half full        |
| A segment         | 16,384 records or 4 MiB, sealed after at most 1 hour |
| A search text     | 1 KiB                                                |

## Metrics

|                         | Limit or default                                      |
|-------------------------|-------------------------------------------------------|
| Retention               | 30 days                                               |
| Lateness                | 0                                                     |
| Instruments write       | every 15 seconds                                      |
| Series                  | 100,000                                               |
| Labels per series       | 128 pairs and 16 KiB; a name 256 bytes, a value 4 KiB |
| An `ingest`             | 10,000 samples or 4 MiB                               |
| Series a query matches  | 1,000                                                 |
| Bytes a query reads     | 16 MiB                                                |
| Samples a query decodes | 1,048,576                                             |
| Samples a query returns | 100,000                                               |
| A query's snapshot      | 5 seconds                                             |

## The server

|                                | Default                                                        |
|--------------------------------|----------------------------------------------------------------|
| Sidecar idle time              | 30 seconds                                                     |
| A frame's body                 | 1 MiB plus 64 KiB, so the largest KV or jobs value fits in one |
| Calls in flight per connection | 256                                                            |
| Connections                    | 64 local, 1,024 remote                                         |

## See also

- [Memory and limits](../concepts/memory.md): how limits fail, and the
  memory budget.
