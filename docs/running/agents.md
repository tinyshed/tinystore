# AI agents

Give an AI coding agent read-only access to your store over MCP, so it can
read your logs, your KV state, your jobs and your SQL tables while it debugs
your program. Agents that write code can also read these docs as plain
markdown.

## Add the store to Claude Code

```sh for=bun
claude mcp add tinystore -- bunx tinystore mcp ./data
```

```sh for=python
claude mcp add tinystore -- uvx --from tinyshed-tinystore tinystore mcp ./data
```

```sh for=go
claude mcp add tinystore -- tinystore mcp ./data
```

`tinystore mcp` speaks the Model Context Protocol on stdin and stdout, so any
agent that supports MCP servers can use it, not only Claude Code.

## What the agent can do

| Tool | Reads |
|---|---|
| `status` | the store's files and sizes, and who serves it |
| `logs` | records from a level up, since a time, containing a text, of a stream; up to 1,000 |
| `kv_get`, `kv_scan` | a bucket's keys and values, including counters, with their expiry |
| `jobs_get`, `jobs_scan` | where a job is, how many jobs are ahead of it, its progress, its error and its last run |
| `sql_query` | the rows of one SQL statement, from one snapshot |

Ask the agent things like "why did the payment job for order 981 fail?" or
"which users have the most sessions?", and it reads the answer from the store.

## The agent can't write

Every tool only reads. `sql_query` runs on a read-only connection, where
SQLite refuses any write. A tool never creates an engine's file: if your
program has never used jobs, `jobs_scan` says so instead of creating
`jobs.db`.

The tools reach the store through its server. If no server runs, `mcp` starts
a sidecar, but only in a directory that already contains a store. For the agent
to reach the store of a running Go program, the program must share its store
with [`server.Share`](../languages.md#share-a-go-programs-store).

`sql_query` reads the databases that the server has opened. A sidecar that the
tool started itself has no database open until your program connects to it.

## These docs, for agents

Every page of these docs is also available as markdown:

| Address | Contains |
|---|---|
| a page's address with `.md` | the page as markdown, links made absolute |
| `/llms.txt` | the list of pages, each with a one-line summary |
| `/llms-full.txt` | every page in one file |

Point a coding agent at `/llms-full.txt`, and it has the whole API in Go, Bun
and Python, with every limit and default. The "Copy as Markdown" button on
each page copies the same text.

## See also

- [The command line](cli.md): `status` and `logs` for people.
