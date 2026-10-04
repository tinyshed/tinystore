---
name: api-change
description: Use when adding, renaming or changing anything a program writes against TinyStore - a public call, option, type, field or error of an engine's Go API or of the Bun or Python SDK - so that Go, Bun and Python keep one vocabulary and every document and example says the same.
---

# Changing what a program writes

A call is one promise in three languages and a dozen documents. It is done
when each of them says it the same way.

## Before the code

- Write the call site first, in Go, Bun and Python, and hold it against the
  rules that keep the three one product:
  - **Same entities and meanings; each language's syntax.** A series, a
    record, a page, a bucket and a queue mean one thing everywhere. Go takes a
    struct, Bun an options object, Python keyword arguments; none imitates
    another.
  - **A type is given once, where a bucket or a queue opens.** The daily calls
    are plain verbs; what the engine chooses goes into open's options, never
    into a call.
  - **The canonical call takes a value, not a chain.** No fluent builder is
    the API: `Range{...}`, `{ ... }`, `name=..., since=...`.
  - **What the store keeps is what comes back.** A value is SQLite's own, a
    sample its bits, a record's field its JSON spelling; no SDK reads a time
    into text or rounds a number on the way.
  - **An error is its code's class**, carrying what it names, in every
    language.
- A name Go already has keeps its word in the SDKs: `since`, `after`, `next`,
  `limit`, camelCase in Bun and snake_case in Python, and a trailing `_` only
  where Python reserves the word (`from_`).
- A proposal shows the call site first, in all three (AGENTS.md). The user
  decides an engine's vocabulary; an outside list of ideas is weighed against
  the rules, as research's design/sdk.md "The proposals, decided" records.

## Where a change goes, in one commit

1. The engine: the API, its README's example and contract lines, and a line in
   GATES.md for each new promise, with the test that keeps it.
2. The wire, when the call crosses it: the `wire-change` skill.
3. The server's handler, and `server/internal/client` when a test needs it.
4. Both SDKs: the call, its options type or signature, its doc comment's
   example, and tests through a real `tinystore serve`.
5. Every place a program reads it: the guides in `docs/`, which show each call
   in all three languages (the `docs` skill), `web/landing.md`'s sample, the
   top-level `README.md` "A first look", the SDK references
   `docs/reference/bun.md` and `docs/reference/python.md`, the SDK packages'
   READMEs, and `examples/notes`, which is built and tested.

Then look for the old spelling everywhere, documents first, since no compiler
reads them:

```sh
git grep -n -e 'oldName' -e 'old_name' -e 'OldName'
```

## What held across the three

- A range is `since`, or `from` and `to`; both kinds of start at once is
  `InvalidError`. `since` becomes an absolute start once, when the call is
  made, so a page's `next` continues the same range however late it is asked
  for.
- `scan` answers a page, `{ items, next }` in Bun, `Page(items, next)` in
  Python, `Page` with `Next` as the next query in Go; `all` walks the pages;
  `read` answers a whole, bounded answer. A `next` is opaque.
- Times and durations, each language's own, and a duration also as text in
  both SDKs (`time.ts`, `_time.py`), each unit once, largest first: `30d`,
  `1h30m`, `250ms`. In Bun the type is the spelling, so `'1hr'` does not
  compile.

  | | Go | Bun | Python |
  |---|---|---|---|
  | a moment | `time.Time`; metrics `int64` unix ms | `Date`; metrics `number` ms, records `bigint` ns | `datetime`; metrics `int` ms, records `int` ns |
  | a span | `time.Duration` | ms as a number, or `'1h30m'` | `timedelta`, seconds, or `"1h30m"` |
- An option is absent rather than zero wherever zero would mean something.
- An error is the store's kind, `errors.Is` in Go and its class in each SDK,
  and it carries what it names: a series its labels, a record its place.
- A reserved spelling is refused, not rewritten: a label beginning `__`.

## Checks

`task check`, which runs both SDKs; `task race:linux`; `task sdk:linux` when
the SDKs' processes, sockets or files moved. The `verify` skill has the rest.
