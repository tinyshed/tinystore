---
name: api-change
description: Use when adding, renaming or changing anything a program writes against TinyStore - a call, option, type, field or error of the Rust core or of the Bun and Node, Python or Go SDK - so that the four keep one vocabulary and every guide, test and example says the same.
---

# Changing what a program writes

A call is one promise in four languages: Rust, which calls the engines
directly, and Bun and Node, Python and Go, which reach the same core through
the protocol. It is done when each of them says it the same way and the
guide shows it in all four.

## Before the code

- A change starts from the call site, in all four languages, before any
  code: what a program writes and what it gets back. The owner accepts the
  names. The engine's guide, `docs/guides/<engine>/`, shows the call sites
  as they are today; research's `tinystore/plan/api/` keeps the books they
  were accepted in, and why.
- The rules every name keeps:
  - **One concept, one word,** in every engine and every language.
  - **A name is at most two words.** A third means the concept wants a
    nested option or a verb of its own.
  - **A flag never changes what a call does.** Another behaviour is another
    verb (`set`, `runDue`), an enumeration, or another constructor; a boolean
    only turns a feature on or off.
  - **Time is typed**: text such as `'30s'`, `Duration`, `timedelta`,
    `Date`. A bare number is refused, as input and as output.
  - **Storage words stay inside**: head, segment, block, seal, merge, lease,
    lookback, watermark.
  - **Take the word people already know**: BullMQ (`add`, `delay`,
    `concurrency`), Redis (`ttl`, `publish`), S3 (`ifMatch`), Prometheus
    (`rate`, `increase`), `tracing` and `slog`.
  - **Names on the store say what a thing is**, not which engine keeps it:
    `store.bucket`, never `store.kv.bucket`; where it lives is said by what
    opened it, the store or a database (`db.bucket`). Go is the exception
    its language makes, since a Go method takes no type parameter:
    `kv.OpenBucket[T](store, …)`, and `kv.OpenBucket[T](db, …)`.
  - A type is given once, where a handle opens; the daily calls are plain
    verbs; what the engine chooses goes into the builder that opens a
    handle, never into a call.
- Each language its own syntax: Rust a builder ending in `open()`, Bun an
  options object, Python keyword arguments, Go functional options or a
  struct; none imitates another. A name keeps one word across them,
  camelCase in TypeScript, snake_case in Python and Rust, exported in Go, a
  trailing `_` only where Python reserves the word (`search.or_`).
- **The newcomer check.** After an API change, three agents that have read
  nothing about TinyStore each get one call site, a line or a few, and say
  in one sentence what it does and what it returns. A wrong or hesitant
  answer means a rename, never a comment. The owner has the last word on
  names.

## Where a change goes, in one commit

1. The core: the engine's module in `crates/tinystore/src/<engine>/`, its
   doc comment's example, a test beside it naming the contract it keeps, and
   CONTRACTS.md changed for a promise the change makes or moves.
2. The wire, when the call crosses it: the `wire-change` skill.
3. Every SDK, with a test through each way a store is reached, the core in
   the process, a private child, a sidecar and a remote server: `sdk/js`
   (Bun's suites run under Node too), `sdk/python`, `sdk/go`. The `sdk`
   skill has how.
4. What a program reads: the engine's guide in `docs/guides/<engine>/` and
   any handbook page that shows the call, in all four languages; the
   reference, written again from the doc comments with `cd web && bun run
   reference`; `web/landing.md`'s sample, then `task readme`; the SDKs' and
   the crates' READMEs; AGENTS.md's Status row when what is built changed.

Then look for the old spelling everywhere, documents first, since no
compiler reads them:

```sh
git grep -n -e 'oldName' -e 'old_name' -e 'OldName'
```

## What held across the four

- A range is `since`, or `from` and `to`; both kinds at once is `invalid`.
  `since` becomes an absolute start once, when the call is made.
- A page is `{ items, next }`; `next` is opaque; `all` walks the pages.
- A duration is each language's own, and text everywhere, each unit once,
  largest first: `30d`, `1h30m`, `250ms`; in TypeScript the type is the
  spelling, so `'1hr'` does not compile.
- An option is absent rather than zero wherever zero would mean something.
- An error is its kind, `ErrorKind` in Rust, its class in TypeScript and
  Python, `errors.Is` in Go, and it names what failed: `kv bucket sessions:
  key "42": conflict`.
- A name that starts with `__` is the store's, and one that starts with
  `tinystore_` is its own metrics': refused from a program, not rewritten.

## Checks

`just quick` before the commit, which runs every SDK a change reaches, then
push and watch CI. The `verify` skill has the rest.
