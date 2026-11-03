# Running it

This chapter is the operator's page for a store in production, such as the
chat's: what to watch, where to look when something is wrong, and how to let
an AI agent read the store. The commands here read a store beside the program
that holds it, without stopping it and without writing to it (contract
CLI-01).

## Is it healthy?

```sh
tinystore status ./data
```

```text
● ./data  served by tinystore 0.7.0 · pid 37420 · up 12m · durability full

  kv         36 KiB
  jobs       60 KiB
  search    2.1 MiB
  sql/chat  1.2 MiB
  records    24 KiB
  ────────────────
  total     3.4 MiB
```

`status` says who serves the directory, the durability it was started with,
and how large each file is. A program that opens the store with a durability
must ask for the one the server was started with, or the open fails. A
server started without `--durability` says `not set`, and refuses a program
that asks for `full` or `os`. A directory no server holds says
`○ ./data  not served`. On a node it also shows which files are homes, which
are copies and how far behind they are; `tinystore node status` shows the
links. `--json` prints the same for scripts.

## What is it doing?

```sh
tinystore logs ./data -f --level warn               # every warning as it happens
tinystore logs ./data --since 1h --grep "timeout"   # the last hour's lines that mention it
tinystore sql ./data chat "select count(*) from messages where created_at > unixepoch('now', '-1 hour') * 1000"
tinystore kv ./data list sessions 42                # one user's sessions
```

`sql` only reads unless you add `--write`. With it, a statement goes through
the server that holds the store, once, and a store that a program opened
`embedded` refuses it: see [sql](../../guides/cli/kv-sql-schema.md#sql).

## The store's own metrics

Open the store with `selfMetrics` (`tinystore serve --self-metrics`), and it
measures itself into `metrics.db`:

| Watch                             | Series                                                        | A problem looks like             |
|-----------------------------------|---------------------------------------------------------------|----------------------------------|
| how long writes wait for a commit | `tinystore_commit_ms`                                         | a p95 that climbs with load      |
| jobs piling up                    | `tinystore_jobs_waiting`                                      | a line that only goes up         |
| how long jobs take                | `tinystore_jobs_run_ms`                                       | a p95 near the queue's `timeout` |
| retries and failures              | `tinystore_jobs_retried_total`, `tinystore_jobs_failed_total` | a rate above zero for long       |
| the disk                          | `tinystore_disk_free_bytes`, `tinystore_file_bytes`           | free space falling to `keepFree` |
| memory                            | `tinystore_memory_used_bytes`                                 | near its budget                  |
| lost log lines                    | `tinystore_records_lines_dropped_total`                       | any increase                     |
| background work                   | `tinystore_background_failures_total`                         | any increase                     |

[The store's own metrics](../../guides/metrics/self-metrics.md) says what
each series measures, and [How fast it is](../chat/how-fast-it-is.md) reads
the chat's own: `aggregate` with `rate`, `p95` or `last`.

## When the disk fills

The store keeps part of the disk free, as much as `keepFree`
(`--keep-free`) says ([Limits and defaults](limits.md#the-store)). A write
that would leave less fails with a `limit` error and writes nothing, so the
store's files are never left half written by a full disk. Free space by
shortening what streams and metric names keep, by expiring files, or by
moving the store to a larger disk with a backup and a restore.

## When something reads `corrupt`

A whole file read whose bytes no longer match their hash, or a block of logs
or metrics that no longer decodes, fails `corrupt` and names the item.
Damage to one item never stops its neighbours (contract ERR-07).
[Repair](../../guides/records/repair.md) lists and drops a damaged segment
of events, and [Removing a series](../../guides/metrics/reading.md#removing-a-series)
deletes a damaged series. Restore the item from a backup first if you need
it.

## Let an AI agent read the store

```sh
claude mcp add tinystore -- tinystore mcp ./data
```

`tinystore mcp` gives an agent tools that only read, so it can find why the
reply to a message failed and change nothing. `--write` adds the writes it
names. [AI agents](../../guides/cli/mcp.md) lists every tool.

## A checklist

- Back up every night with a schedule, and try a restore once a month on
  another machine: [Keeping it](../chat/keeping-it.md).
- Keep the encryption key apart from the data if you use encrypted buckets.
- Watch the jobs waiting and the free disk, as the table above says.
- Give every stream a `keep` that matches how long you need its events.
- Serve other machines over TLS only, with `data` tokens for programs and
  `read` tokens for dashboards.

## See also

- [The server](../nodes/server.md): serving a store to other machines.
- [Limits and defaults](limits.md): every bound in one place.
