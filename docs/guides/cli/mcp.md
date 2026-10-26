# AI agents

`tinystore mcp` lets an AI agent read a store through the Model Context
Protocol, on stdin and stdout. By default its tools only read: an agent can
find why a job failed, which lines a request logged and what a row holds, and
it can change nothing. `--write` adds the tools that write, one operation
each, only those it names.

```sh
claude mcp add tinystore -- tinystore mcp ./data
claude mcp add tinystore -- tinystore mcp ./data --write kv_set,jobs_add
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

## Writing

An agent that can write can also write the wrong thing, confidently, and a
prompt injection in a log line it reads can ask it to. So `mcp` reads unless
told otherwise, and writing is given an operation at a time:

```sh
tinystore mcp ./data --write kv_set,jobs_add   # these two, and no other
tinystore mcp ./data --write all               # every tool below
```

| Tool               | Takes                                                  | Removes or replaces |
|--------------------|--------------------------------------------------------|---------------------|
| `kv_set`           | `bucket`, `key`, `value`, `as`, `ttl`, `database`      | no                  |
| `kv_delete`        | `bucket`, `key`, `database`                            | yes                 |
| `jobs_add`         | `queue`, `job`, `id`, `delay`, `database`              | no                  |
| `jobs_cancel`      | `queue`, `id`, `database`                              | yes                 |
| `files_put`        | `set`, `path`, `text`, `type`                          | yes                 |
| `files_delete`     | `set`, `path`                                          | yes                 |
| `events_append`    | `stream`, `name`, `fields`, `level`, `id`              | no                  |
| `channels_publish` | `channel`, `message`, `under`                          | no                  |
| `config_update`    | `config`, `changes`, as `{"model.max_tokens": "2048"}` | no                  |
| `config_reset`     | `config`, `path`                                       | yes                 |
| `sql_exec`         | `database`, `statement`                                | yes                 |

- Each tool is one operation and is marked as writing, and as destructive
  where it removes or replaces something, so the agent's host asks its person
  for each, as Claude Code does for any tool not yet allowed.
- A tool `--write` doesn't name is not offered, and a call of it is refused.
- They write as the command line's writes do: through the server that holds
  the directory, or a sidecar they start. A directory a program opened
  `embedded` is refused.
- A flag is no boundary for an agent that also has a shell: it can run
  `tinystore kv set` itself. Give an agent `--write` where its host decides
  what it may run, not as a sandbox.

## See also

- [The command line](README.md).
- [Let an AI agent read the store](../../handbook/production/running-it.md#let-an-ai-agent-read-the-store)
  in the Handbook.
