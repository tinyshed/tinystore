---
name: api-change
description: Use when adding, renaming or changing anything a program writes against TinyStore - a call, option, type, field or error of the Rust core or of the Bun and Node, Python or Go SDK - so that the four keep one vocabulary and every book, test and example says the same.
---

# Changing what a program writes

A call is one promise in four languages: Rust, which calls the engines
directly, and Bun and Node, Python and Go, which reach the same core through
the protocol. It is done when each of them says it the same way, its book
first.

## Before the code

- Each engine's book, `plan/api/<engine>.md`, is the vocabulary the owner
  accepted: its call sites in Rust, TypeScript, Python and Go, then what each
  call promises. A change starts there, call site first, in all four
  (AGENTS.md: a proposal shows the call site first).
- [plan/dx.md](../../../plan/dx.md) is the rules every name keeps: one
  concept one word, two words at most, a flag never changes what a call
  does, time typed and never a bare number, storage words kept inside, the
  word people know already, names on the store saying what a thing is
  (`store.bucket`, never `store.kv.bucket`). A type is given once, where a
  handle opens; the daily calls are plain verbs; what the engine chooses
  goes into the builder that opens a handle, never into a call.
- Each language its own syntax: Rust a builder ending in `open()`, Bun an
  options object, Python keyword arguments, Go functional options or a
  struct; none imitates another. A name keeps one word across them,
  camelCase in TypeScript, snake_case in Python and Rust, exported in Go, a
  trailing `_` only where Python reserves the word (`search.or_`).
- After an API slice: three agents with no context of the project read the
  new call sites as newcomers and say what they misread; the owner has the
  last word on names.

## Where a change goes, in one commit

1. The core: the engine's module in `crates/tinystore/src/<engine>/`, its
   doc comment's example, a test beside it, and a line in GATES.md for each
   new promise with the test that keeps it.
2. The wire, when the call crosses it: the `wire-change` skill.
3. Every SDK, with a test through each way a store is reached, the core in
   the process, a private child, a sidecar and a remote server: `sdk/js`
   (Bun's suites run under Node too), `sdk/python`, `sdk/go`. The `sdk`
   skill has how.
4. The book, `plan/api/<engine>.md`, and AGENTS.md's Status row when what is
   built changed.
5. What a program reads: the guides in `docs/` once they describe the Rust
   branch (they still describe the Go candidates), `web/landing.md`'s
   sample, the SDKs' READMEs.

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

`just quick` before the commit, which runs every SDK a change reaches;
`just check` before the push. The `verify` skill has the rest.
