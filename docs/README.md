# TinyStore documentation

TinyStore gives an application SQL databases, key-value state, background
jobs, files, search, channels, locks, settings, logs and metrics in one
directory on its own disk. The core is written in Rust. Rust programs call it
directly, and Bun, Node, Python and Go programs reach the same core through
their SDK.

The documentation has three parts, each for a different moment:

- **[Learn from scratch](handbook/README.md)**: the Handbook builds one
  application, an AI chat, chapter by chapter, from a first store to copies
  in another region, and explains why TinyStore is built the way it is.
- **[Explore the engines](guides/README.md)**: the Guides cover each engine,
  a page per feature, for when you know what you need: expiry in KV, retries
  in Jobs, filters in Search.
- **[Browse the API](reference/api.md)**: the Reference lists every call of
  every SDK, generated from the source.

Every example is shown in TypeScript, Python, Go and Rust.

<!--
This file is the documentation's table of contents, here and on the site,
which is built from these files. Each second-level heading is a part, a tab
of the site's header; each third-level heading a section of that part's
sidebar, and each item a page, in this order, which is also the order the
pages' previous and next links follow. A part with no third-level heading is
one section of its own name. An item without a link is a page nobody has
written yet; the site shows it and links nowhere. The docs skill has the shape
and the voice: .agents/skills/docs/SKILL.md.
-->

## Handbook

### Start

- [Learn TinyStore](handbook/README.md)
- [Why TinyStore](handbook/start/why.md)
- [Your first store](handbook/start/first-store.md)
- [One directory, many engines](handbook/start/directory.md)
- [The chat we build](handbook/start/chat.md)

### Building the chat

- [Messages](handbook/chat/messages.md)
- [Sessions and limits](handbook/chat/sessions-and-limits.md)
- [Replies in the background](handbook/chat/replies.md)
- [Attachments](handbook/chat/attachments.md)
- [Finding old messages](handbook/chat/finding-old-messages.md)
- [Live updates](handbook/chat/live-updates.md)
- [Settings](handbook/chat/settings.md)
- [What it is doing](handbook/chat/what-it-is-doing.md)
- [How fast it is](handbook/chat/how-fast-it-is.md)
- [Keeping it](handbook/chat/keeping-it.md)

### Under the hood

- [The journey of a write](handbook/inside/journey-of-a-write.md)
- [SQLite as TinyStore runs it](handbook/inside/sqlite.md)
- [One budget of memory](handbook/inside/memory.md)
- [Time and errors](handbook/inside/time-and-errors.md)
- [How a job lives](handbook/inside/how-a-job-lives.md)
- [How logs and metrics pack](handbook/inside/how-logs-and-metrics-pack.md)
- [How search finds](handbook/inside/how-search-finds.md)
- [One core, four languages](handbook/inside/one-core.md)

### Beyond one machine

- [The server](handbook/nodes/server.md)
- [Readers in another region](handbook/nodes/copies.md)
- [When the home dies](handbook/nodes/home-dies.md)

### Production

- [What TinyStore promises](handbook/production/promises.md)
- [Limits and defaults](handbook/production/limits.md)
- [Running it](handbook/production/running-it.md)

## Guides

### Engines

- [All engines](guides/README.md)
- Connecting

### SQL

- [Overview](guides/sql/README.md)
- [Migrations](guides/sql/migrations.md)
- [Writes and transactions](guides/sql/writes.md)
- [Tables and queries](guides/sql/tables.md)
- [Rows that include rows](guides/sql/include.md)
- [Buckets and queues in a database](guides/sql/buckets-and-queues.md)
- [FTS5 and R*Tree](guides/sql/fts5.md)

### KV

- [Overview](guides/kv/README.md)
- [Expiry](guides/kv/expiry.md)
- [Versions](guides/kv/versions.md)
- [Branches](guides/kv/branches.md)
- [Encrypted buckets](guides/kv/encrypted.md)
- [Counters](guides/kv/counters.md)
- [Rate limits](guides/kv/rate-limits.md)
- [Quotas](guides/kv/quotas.md)
- [Once](guides/kv/once.md)
- [Transactions](guides/kv/transactions.md)

### Jobs

- [Overview](guides/jobs/README.md)
- [Ids](guides/jobs/ids.md)
- [Schedules and cron](guides/jobs/schedules.md)
- [Concurrency and rate](guides/jobs/concurrency.md)
- [Watching a job](guides/jobs/watching.md)
- [Steps](guides/jobs/steps.md)
- [Jobs in your database](guides/jobs/your-database.md)
- [Testing](guides/jobs/testing.md)

### Blobs

- [Overview](guides/blobs/README.md)
- [Uploads](guides/blobs/uploads.md)
- [Serving files](guides/blobs/serving.md)
- [Copies and expiry](guides/blobs/copies-and-expiry.md)
- [Integrity](guides/blobs/integrity.md)

### Search

- [Overview](guides/search/README.md)
- [Fields and weights](guides/search/fields.md)
- [Typos](guides/search/typos.md)
- [Filters](guides/search/filters.md)
- [Vectors](guides/search/vectors.md)
- [Words and vectors together](guides/search/hybrid.md)
- [A table's index](guides/search/table-index.md)
- [Rebuilds](guides/search/rebuilds.md)

### Channels

- [Overview](guides/channels/README.md)
- [Subscriptions and missed messages](guides/channels/subscriptions.md)
- [Publishing from a transaction](guides/channels/transactions.md)

### Locks

- [Overview](guides/locks/README.md)
- [Holding and waiting](guides/locks/holding.md)
- [Leases over a connection](guides/locks/leases.md)
- [Fences](guides/locks/fences.md)
- [Semaphores](guides/locks/semaphores.md)

### Config

- [Overview](guides/config/README.md)
- [Layers and files](guides/config/layers.md)
- [Secrets and required settings](guides/config/secrets.md)
- [Live settings](guides/config/live.md)
- [Watching](guides/config/watching.md)
- [Explain and validate](guides/config/explain.md)

### Records

- [Overview](guides/records/README.md)
- [Logging](guides/records/logging.md)
- [Console output](guides/records/console.md)
- [Events](guides/records/events.md)
- [Reading](guides/records/reading.md)
- [Traces](guides/records/traces.md)
- [Tail and poll](guides/records/tail-and-poll.md)
- [Other programs' output](guides/records/other-programs.md)
- [Repair](guides/records/repair.md)

### Metrics

- [Overview](guides/metrics/README.md)
- Instruments
- Reading
- Aggregates
- Ingesting
- Prometheus
- The store's own metrics
- The metrics crate

### Backup

- [Overview](guides/backup/README.md)
- Restore
- The encryption key

### Command line

- [Overview](guides/cli/README.md)
- serve
- status and logs
- kv, sql and schema
- Backup and restore
- Nodes
- AI agents

## Reference

- [API reference](reference/api.md)
- [Wire protocol](wire.md)
