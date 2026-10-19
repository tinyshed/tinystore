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
- Replies in the background
- Attachments
- Finding old messages
- Live updates
- Settings
- What it is doing
- How fast it is
- Keeping it

## Under the hood

- The journey of a write
- SQLite as TinyStore runs it
- One budget of memory
- Time and errors
- How a job lives
- How logs and metrics pack
- How search finds
- One core, four languages

## Beyond one machine

- The server
- Readers in another region
- When the home dies

## Production

- What TinyStore promises
- Limits and defaults
- Running it

## Reference

- [API reference](reference/api.md)
- [Wire protocol](wire.md)
