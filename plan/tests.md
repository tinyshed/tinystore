# Tests

The Go suite is the specification. It is not translated line by line: each
test says what an engine promises, and the Rust suite proves the same promise
in the new vocabulary.

## What the Go suite holds

At `e81a050`: 911 `Test` functions, 15 `Fuzz`, 18 `Example`, 18 `Benchmark`
in 212 files; GATES.md lists 466 promises and names 568 tests, every one of
which exists.

| Class                            | Share | What happens to it                                                                                |
|----------------------------------|-------|---------------------------------------------------------------------------------------------------|
| Behaviour through the public API | 44 %  | rewritten in Rust with the same meaning and the new names; 76 % of kv, 63 % of the server         |
| Formats, goldens, frames         | 12 %  | rewritten for the new formats, with golden files and bit-for-bit property tests                   |
| Go internals                     | 33 %  | not ported: 93 % of `internal/sqlite`, 48 % of metrics; the Rust internals get tests of their own |
| Module gates and measurements    | 8 %   | the gates rewritten for cargo; measurements go to research rounds                                 |

## Rules for the port

- **GATES.md is the checklist.** An engine is ported when each of its lines
  names a Rust test that proves the same promise and passes. Rewrite a line
  when its test lands; until then it names the Go test at `e81a050`.
- **Shared vectors stay shared.** Go, Bun and Python read five files today:
  `vectors.json` (83 cases), `messages.json` (185), kv's `config.json` (19),
  `console.json` (37 lines) and `limits.json` (13). They are regenerated for
  the new protocol and read by Rust too; `messages.json` is written from the
  schema, not by hand.
- **Time is a clock, never a sleep.** The store's clock and the server's
  forward-only test clock carry over. The 31 `time.Sleep` calls in the Go
  tests and 33 sleeps in the SDK suites are replaced by moving a clock or
  waiting for a condition with a deadline.
- **Crashes are tested by killing a process.** Five engines prove that a write
  that returned survives an abrupt exit, by re-running the test binary and
  killing it; the Rust suite keeps a harness that does the same.
- **Every SDK test runs twice,** over the pipe and over a socket, with no
  extra test code. Bun's 159 blocks and Python's 121 functions, about 370
  cases, are the start.
- **Platforms are tested on themselves.** CI runs Linux, macOS and Windows; a
  result from one is never claimed for another.

## New in Rust

| Tool             | What it covers                                                                        |
|------------------|---------------------------------------------------------------------------------------|
| cargo-fuzz       | every decoder of untrusted input: files, frames, messages                             |
| proptest         | codecs bit for bit, and model-based tests of engines against a simple in-memory model |
| loom or shuttle  | the group commit and the reader pool; they find interleavings, they prove nothing     |
| AddressSanitizer | the SQLite adapter and the pipe, which call C                                         |
| Miri             | the codecs; it does not run C                                                         |
| insta            | golden outputs: schema SQL, console lines, error messages                             |
| criterion        | benchmarks beside the code; comparisons with Go belong in research rounds             |
