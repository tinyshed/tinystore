---
name: format-change
description: Use when changing the bytes or the tables an engine keeps in its file - a row's encoding, a block's version, a new layout of a table, a bound raised - so that a store a release wrote still opens and reads, the old format stays out of the way of the new code, and the release before refuses the file rather than read the new bytes as damage.
---

# Changing what a file holds

A store a release wrote opens in every later one (CONTRACTS.md, STORE-11),
and a release refuses a file a later one migrated. A program never chooses a
format: there is no `mode`, no flag and no setting for it. The engine reads
every format a release wrote, writes only the newest, and moves what it
reads to the newest when it would write it anyway.

## Which case it is

| What changes                                            | How                                       | Example                                        |
|---------------------------------------------------------|-------------------------------------------|------------------------------------------------|
| A row's bytes, its table the same                       | a version byte, one parser that branches  | records' blocks 2 to 4, metrics' codec         |
| The tables a format lives in, or a key's rows of them   | a module of the old format, `legacy`      | search's rows of blocks, now runs (0002)       |

A new layout starts with a version byte in its first release (search's
packed directory), so that its next change is the first case, a branch in
its one parser, and not a second legacy module.

Both add a step to the file's migrations: an empty one with a comment when
no table changes, so that the release before, which lacks it, refuses the
file at its open (`sqlite::Migration`, an engine's `MIGRATIONS`). Never edit
a migration a release shipped.

## A version byte

- The row starts with its version; the writer writes the newest, `VERSION`,
  and the old ones keep their names, `VERSION_3`, `VERSION_2`.
- One function opens a row and branches on its version once, at the top
  (`records/block.rs`, `Opened::of`); everything after reads what it
  returns. No second parser of an old version anywhere.
- A row of an old version changes only where the engine rewrites it anyway:
  a merge, a pack, a purge.

## A legacy module

When the tables themselves change, the old format goes into one module of
its engine, `<engine>/legacy.rs` (a folder `<engine>/legacy/`, a file a
format, once there are two). The new format has its own module beside it
(search's `packed.rs`), and the module that writes the file (search's
`pack.rs`) knows only the new one.

What the legacy module holds, and nothing outside it may:

- the old layout's doc table and its codec: decoding only, since nothing
  writes it;
- what a reader needs of it: a list onto a search, a block's postings, every
  document of a key (search's `Legacy::onto`, `legacy::block`,
  `legacy::documents`);
- its conversion to the new format (`Legacy::into_runs`), which a writer
  calls when it would rewrite the thing anyway, a pack or a purge;
- a step of upkeep that converts what no write reaches (`convert_step`,
  bounded by rows a step and by steps a maintenance pass), so that the old
  format leaves a file soon after a release opens it and a cold key is not
  left in it for good;
- under `#[cfg(test)] mod written`, the old writer, kept only to write old
  rows in tests, and a `rewrite` that turns a store's new rows back into
  the old format.

The rest of the engine meets it at one seam: the enum that says how a thing
is stored has a `Legacy` variant, and each match on it hands that variant
to the module in one line. A file that grows a second legacy branch inside
its own logic is the sign the seam leaked.

Old-format-only machinery is dropped, not carried: search's eight blocks
read ahead went with the rows of blocks, since they live only till upkeep
converts them.

## When an old format leaves

Every old format stays until `1.0`, and leaves then (the owner's decision,
1 November 2026):

- The last `0.x` converts everything old in every engine through upkeep,
  search's rows of blocks, records' and metrics' old versions alike, and
  once a file holds no row of an old format, it marks the file so. A command
  runs the conversion to its end and says so, and `status` says what is
  left.
- `1.0` refuses at its open a file without the mark, naming the `0.x` that
  converts it, and its code reads no format before it: the `legacy` modules
  and the old versions' branches go, and STORE-11 promises the formats of
  its own major version.

## Tests

- **The bytes a release wrote**, as hex constants beside the legacy module
  (`BLOCK_OF_V0_5`), each naming the release and its commit, and a test that
  the test writer still writes exactly them.
- **A store written the old way answers as before**: build an index or a
  stream with the new code, `rewrite` it to the old format, and compare its
  answers with one never rewritten, before and after the writes and the
  upkeep that convert it; then nothing of the old format is left
  (`// contract: STORE-11`).
- The new codec's round trips and the invariant each broken byte names, as
  any codec's.

## Measurements

A new format is a claim about bytes and time. Compare it with the one it
replaces on the same data in the same run (search's timing builds the
index, times it, rewrites it to the old format and times it again), and
divide the file by table: each layout's payload and its pages, in bytes a
posting or a sample (`dbstat`), as AGENTS.md's Measurements say. The
numbers go to a research round, not into the code's comments, beyond the
one that says why the format is as it is.

## Docs

The handbook's chapter on the engine says what the new format does for a
program, in plain words; the release's page says a file it opened cannot be
opened by the release before.
