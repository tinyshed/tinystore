# TinyStore 2.0: the Rust rewrite

On this branch, `rust`, TinyStore is being rewritten in Rust. One core serves
Rust programs directly, and Go, Bun, Node and Python through the wire
protocol: embedded in the process through a byte pipe, beside it as a sidecar,
or remote through the server. This directory is the plan. Read it before
working on the rewrite, and change it in the same commit that changes what it
says.

| File                               | What it holds                                                                                         |
|------------------------------------|-------------------------------------------------------------------------------------------------------|
| [decisions.md](decisions.md)       | what was decided, when and why; where two files disagree, this one wins                               |
| [evidence.md](evidence.md)         | what the Rust research measured and what it did not, and what to take from its prototypes             |
| [architecture.md](architecture.md) | the layers, the crates, the runtime model, config and logger, durability, the modules that come later |
| [ffi.md](ffi.md)                   | the byte pipe: its five functions, the bindings per language, async hosts, what ships per platform    |
| [protocol.md](protocol.md)         | protocol 2: the books as messages, one schema, the codecs generated for every language                |
| [dx.md](dx.md)                     | the API's vocabulary: the rules, the newcomer check, the words decided and the drafts                 |
| [phases.md](phases.md)             | the order of work: each phase's tasks and the gate that closes it, with their status                  |
| [tests.md](tests.md)               | how the Go suite becomes the Rust suite                                                               |

The plan was written on 9 October 2026 from the research rounds of 7–9
October, an inventory of the Go API and tests, and the owner's decisions of
that day. A page in Russian with the same content was published for the owner
and is not the source of truth; these files are.

## Where things stand

| Phase                                              | Status      |
|----------------------------------------------------|-------------|
| 0. Decisions, vocabulary, workspace                | in progress |
| 1. Vertical slice: core and KV through every layer | in progress |
| 2. sqldb, jobs, blobs                              | in progress |
| 3. records and metrics                             | not started |
| 4. Backup, CLI, guest, SDKs, v0.1.0                | not started |
| 5. Channels and locks                              | not started |
| 6. Indexes                                         | not started |

[phases.md](phases.md) has each phase's tasks and gate.

## Working on the rewrite

- **The Go implementation is a reference, not a dependency.** It is the
  commit `e81a050`, the last one on `dashbin-asks` before this branch. Read a
  file with `git show e81a050:kv/kv.go`, or check the commit out in a
  worktree of its own beside this repository. Its tests, GATES.md and the
  design documents in research's
  [tinystore/design](https://github.com/tinyshed/research/tree/main/tinystore/design)
  say what each engine promises and why.
- **Nothing is released, so nothing is compatible.** The Go release
  candidates promise nothing; formats, schemas, the protocol and every name
  change freely until `v0.1.0`, which is the first Rust release.
- **Port the promise, not the code.** A Go function is evidence of what an
  engine must do, and the research rounds are evidence of how to do it fast.
  Neither is a template for the Rust code.
- **Who writes what.** The owner trusts the Rust code to one model only:
  Opus writes and edits every line of Rust. Helper agents, Haiku among them,
  gather facts, take inventories, run builds, tests and measurements, and do
  mechanical chores such as commits in research; they never write or edit
  Rust code.
- **The rest of the repository's rules hold**: AGENTS.md, rewritten for this
  branch, has the runtime and storage invariants, the editing and commit
  rules, and the checks, which run through `just`.
