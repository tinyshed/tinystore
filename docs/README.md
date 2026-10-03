# TinyStore documentation

TinyStore gives an application SQL, key-value state, durable jobs, files,
metrics and logs in one directory, with one lifecycle, one memory budget and
one backup. Go programs embed it; Bun, Node and Python programs reach the same
directory through a sidecar their SDK starts.

Read Start here in order, then the engines your application needs. Each
engine's section opens with an overview, followed by a page for each feature.
Every example is shown in Bun, Python and Go.

<!--
This file is the documentation's table of contents, here and on the site,
which is built from these files: each second-level heading is a section of
its sidebar and each item a page, in this order, which is also the order the
pages' previous and next links follow. An item without a link is a page
nobody has written yet; the site shows it and links nowhere. Where a page
goes, its shape, its voice and its code in three languages are the docs
skill: .agents/skills/docs/SKILL.md.
-->

## Start here

- [Introduction](introduction.md)
- [Getting started](getting-started.md)
- [Go, Bun and Python](languages.md)
- [A tour](tour.md)

## SQL

- [Overview](sql/README.md)
- [Schema in Go](sql/schema.md)
- [Writes and transactions](sql/writes.md)
- [Full-text search](sql/search.md)

## KV

- [Overview](kv/README.md)
- [Expiry](kv/expiry.md)
- [Versions](kv/versions.md)
- [Counters](kv/counters.md)
- [Sessions](kv/sessions.md)
- [Rate limits](kv/rate-limits.md)
- [Quotas](kv/quotas.md)
- [Once](kv/once.md)
- [Configs](kv/configs.md)
- [Transactions](kv/transactions.md)

## Jobs

- [Overview](jobs/README.md)
- [Keys](jobs/keys.md)
- [Schedules](jobs/schedules.md)
- [Watching a job](jobs/watching.md)
- [Steps](jobs/steps.md)
- [Jobs and your data](jobs/your-data.md)

## Blobs

- [Overview](blobs/README.md)
- [Uploads](blobs/uploads.md)
- [Serving files](blobs/serving.md)
- [Copies, moves and expiry](blobs/copies.md)
- [Integrity](blobs/integrity.md)

## Records

- [Overview](records/README.md)
- [Logging](records/logging.md)
- [Events](records/events.md)
- [Reading](records/reading.md)
- [Traces](records/traces.md)
- [Other programs' output](records/lines.md)
- [Following](records/following.md)

## Metrics

- [Overview](metrics/README.md)
- [Instruments](metrics/instruments.md)
- [Reading](metrics/reading.md)
- [Aggregates](metrics/aggregates.md)
- [Ingesting](metrics/ingesting.md)

## Running it

- [The sidecar](running/sidecar.md)
- [A remote server](running/server.md)
- [The command line](running/cli.md)
- [AI agents](running/agents.md)
- [Backups](running/backups.md)
- [Testing](running/testing.md)
- [Upgrading](running/upgrading.md)

## How it works

- [Durability](concepts/durability.md)
- [Concurrency](concepts/concurrency.md)
- [Time](concepts/time.md)
- [Memory and limits](concepts/memory.md)
- [Errors](concepts/errors.md)
- [Files on disk](concepts/files.md)

## Reference

- Go API
- [Bun and Node API](../sdk/js/README.md)
- [Python API](../sdk/python/README.md)
- [Limits and defaults](reference/limits.md)
- [Wire protocol](wire.md)
