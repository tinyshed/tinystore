# TinyStore documentation

TinyStore gives an application SQL databases, key-value state, background
jobs, files, search, channels, locks, settings, logs and metrics in one
directory on its own disk. The core is written in Rust. Rust programs call it
directly, and Bun, Node, Python and Go programs reach the same core through
their SDK.

These pages are a book. Start reads in twenty minutes and ends with a working
store. Building the chat writes one application, an AI chat, a chapter per
engine, so that you can stop after any chapter with a program that runs.
Under the hood explains how each part works and why it was built that way.
Every example is shown in TypeScript, Python, Go and Rust.

<!--
This file is the documentation's table of contents, here and on the site,
which is built from these files: each second-level heading is a section of
its sidebar and each item a page, in this order, which is also the order the
pages' previous and next links follow. An item without a link is a page
nobody has written yet; the site shows it and links nowhere. The docs skill
has the shape and the voice: .agents/skills/docs/SKILL.md.
-->

## Start

- [Why TinyStore](start/why.md)
- [Your first store](start/first-store.md)
- [One directory, many engines](start/directory.md)
- [The chat we build](start/chat.md)

## Building the chat

- [Messages](chat/messages.md)
- [Sessions and limits](chat/sessions-and-limits.md)
- [Replies in the background](chat/replies.md)
- [Attachments](chat/attachments.md)
- [Finding old messages](chat/finding-old-messages.md)
- [Live updates](chat/live-updates.md)
- [Settings](chat/settings.md)
- [What it is doing](chat/what-it-is-doing.md)
- [How fast it is](chat/how-fast-it-is.md)
- [Keeping it](chat/keeping-it.md)

## Under the hood

- [The journey of a write](inside/journey-of-a-write.md)
- [SQLite as TinyStore runs it](inside/sqlite.md)
- [One budget of memory](inside/memory.md)
- [Time and errors](inside/time-and-errors.md)
- [How a job lives](inside/how-a-job-lives.md)
- [How logs and metrics pack](inside/how-logs-and-metrics-pack.md)
- [How search finds](inside/how-search-finds.md)
- [One core, four languages](inside/one-core.md)

## Beyond one machine

- [The server](nodes/server.md)
- [Readers in another region](nodes/copies.md)
- [When the home dies](nodes/home-dies.md)

## Production

- [What TinyStore promises](production/promises.md)
- [Limits and defaults](production/limits.md)
- [Running it](production/running-it.md)

## Reference

- [API reference](reference/api.md)
- [Wire protocol](wire.md)
