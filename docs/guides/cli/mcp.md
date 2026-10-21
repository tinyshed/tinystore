# AI agents

`tinystore mcp` lets an AI agent read a store through the Model Context
Protocol, on stdin and stdout. Its tools only read: an agent can find why a
job failed, which lines a request logged and what a row holds, and it can
change nothing.

```sh
claude mcp add tinystore -- tinystore mcp ./data
```

Any MCP client takes it the same way: the command is `tinystore`, its
arguments `mcp` and the store's directory.

## Tools

| Tool          | Takes                                           | Gives                                                     |
|---------------|-------------------------------------------------|-----------------------------------------------------------|
| `status`      | nothing                                         | the store's files and sizes, its server, its node         |
| `logs`        | `since`, `level`, `grep`, `stream`, `limit`     | the latest lines and events, newest last; 50, 200 at most |
| `kv_get`      | `bucket`, `key`, `database`                     | one value                                                 |
| `kv_list`     | `bucket`, `branch`, `database`, `limit`         | a bucket's keys and values, in key order                  |
| `jobs_list`   | `queue`, `prefix`, `state`, `database`, `limit` | jobs with their state, attempts and last error            |
| `sql_query`   | `database`, `query`                             | the rows of one statement that reads                      |
| `search_list` | nothing                                         | the store's search indexes, with their documents          |

- Every tool reads beside the program or server that holds the directory,
  without its lock and without writing a byte.
- `sql_query` runs on a read-only connection: a statement that writes is
  refused.
- `jobs_list` with `state: 'failed'` and no prefix gives a queue's failed
  jobs, the last failed first: where a question about an error usually
  starts.
- A bucket or a queue the program opened from a database, `db.bucket(…)` or
  `db.queue(…)`, lives in that database's file rather than in `kv.db` or
  `jobs.db`: `kv_get`, `kv_list` and `jobs_list` read it with `database`, as
  `{ "queue": "reminders", "database": "app" }`.

## What an agent can answer

> Why did the reply to message 41 fail?

The agent reads the queue's failed jobs, finds the one for message 41 and its
last error, reads the request's log lines around that time by `grep`, and the
message's row by `sql_query`. Each step is a read; none needs a person to copy
anything into the chat.

## Why read-only

An agent that can write can also write the wrong thing, confidently. Reading
is enough to explain what happened; a fix goes through the program's own code
and a person's review. A token or a flag that let `mcp` write would make every
prompt injection in a log line a write to production.

## See also

- [The command line](README.md).
- [Let an AI agent read the store](../../handbook/production/running-it.md#let-an-ai-agent-read-the-store)
  in the Handbook.
