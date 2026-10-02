---
name: api-change
description: Use when adding, renaming or changing anything a program writes against TinyStore - a public call, option, type, field or error of an engine's Go API or of the Bun or Python SDK - so that Go, Bun and Python keep one vocabulary and every document and example says the same.
---

# Changing what a program writes

A call is one promise in three languages and a dozen documents. It is done
when each of them says it the same way.

## Before the code

- Write the call site first, in Go, Bun and Python, and hold it against the
  rules of `docs/sdk.md`: the same entities and names in each language's
  syntax; a type given once, where a bucket or queue opens; plain verbs; what
  the engine chooses in open's options, never in a call; a value, not a chain.
- A name Go already has keeps its word in the SDKs: `since`, `after`, `next`,
  `limit`, camelCase in Bun and snake_case in Python, and a trailing `_` only
  where Python reserves the word (`from_`).
- A proposal shows the call site first, in all three (AGENTS.md). The user
  decides an engine's vocabulary; an outside list of ideas is weighed against
  the rules, as `docs/sdk.md` "The proposals, decided" records.

## Where a change goes, in one commit

1. The engine: the API, its README's example and contract lines, its design
   document's example (`docs/<engine>.md`), and a row in AGENTS.md's gates for
   each new promise, with the test that keeps it.
2. The wire, when the call crosses it: the `wire-change` skill.
3. The server's handler, and `server/internal/client` when a test needs it.
4. Both SDKs: the call, its options type or signature, its doc comment's
   example, and tests through a real `tinystore serve`.
5. Every place a program reads it: `docs/sdk.md`'s side-by-side blocks and
   tables, the top-level `README.md` "A first look", `sdk/js/README.md`,
   `sdk/python/README.md`, `docs/server.md`'s SDK examples, and
   `examples/notes`, which is built and tested.

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
- Times and durations follow the table in `docs/sdk.md`: a duration is also
  text, `1h30m`, in both SDKs (`time.ts`, `_time.py`), and a bare number is
  Bun's milliseconds and Python's seconds.
- An option is absent rather than zero wherever zero would mean something.
- An error is the store's kind, `errors.Is` in Go and its class in each SDK,
  and it carries what it names: a series its labels, a record its place.
- A reserved spelling is refused, not rewritten: a label beginning `__`.

## Checks

`task check`, which runs both SDKs; `task race:linux`; `task sdk:linux` when
the SDKs' processes, sockets or files moved. The `verify` skill has the rest.
