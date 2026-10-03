# TinyStore documentation

TinyStore gives an application SQL, key-value state, durable jobs, files,
metrics and logs in one directory, with one lifecycle, one memory budget and
one backup. Go programs embed it; Bun, Node and Python programs reach the same
directory through a sidecar their SDK starts.

<!--
This file is the documentation's table of contents, here and on the site,
which is built from these files: each second-level heading is a section of
its sidebar and each item a page, in this order. An item without a link is a
page nobody has written yet; the site shows it and links nowhere. How a page
is written, its code in three languages and its callouts, is the docs skill:
.agents/skills/docs/SKILL.md.
-->

## Start here

- What TinyStore is
- Your first store
- How it fits into an app

## Store

- KV
- SQL
- Jobs
- Blobs
- Records
- [Metrics](store/metrics.md)

## Understand

- Durability
- Concurrency
- Storage layout
- Sidecar and remote
- Backups
- Memory and limits

## Ship

- Docker
- Production setup
- Migrations
- Upgrading

## Reference

- Go API
- [Bun API](../sdk/js/README.md)
- [Python API](../sdk/python/README.md)
- CLI

## Design

- [Architecture](architecture.md)
- [The metrics design](design.md)
- [Exact aggregates](aggregate-contract.md)
- [Group commit](group-commit-contract.md)
- [The payload format](format.md)
- [Records](records.md)
- [sqldb](sqldb.md)
- [KV](kv.md)
- [Jobs](jobs.md)
- [Blobs](blobs.md)
- [The server](server.md)
- [The wire protocol](wire.md)
- [The SDKs' vocabulary](sdk.md)
