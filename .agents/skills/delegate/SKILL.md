---
name: delegate
description: Use when handing a mechanical chore to helper agents, Haiku among them - a rename or a path changed across many files outside Rust, an inventory, a build, a test or a measurement run - so that each gets a brief it cannot misread, several run at once without touching the same files, and what they did is checked before it is committed.
---

# Handing a chore to a helper agent

A helper agent starts with nothing: not this conversation, not AGENTS.md,
not what the change is for. What it does well is a chore whose every step
the brief already decided. What it does badly is a decision. So the brief
decides everything, and the agent reports what it could not decide.

## What may be handed over

- Edits outside Rust that a table or a rule settles: a path, a name or a
  word changed in comments, docs, SDK code, `.wire` files, scripts.
- Inventories: where something is used, what a folder holds, which tests
  name a contract.
- Runs: a build, a suite, a measurement, a log to read through.

Never Rust code (AGENTS.md, "Who writes what"), never a choice of names,
never a commit, a push or anything outward-facing. A brief that needs the
words "use your judgement" is a chore you keep.

## The brief

Every brief carries the same parts, in this order:

1. **Where**: the checkout's absolute path and the operating system, and the
   shell it should use (Git Bash on Windows).
2. **Why, in one sentence**: enough to read the files right, not to redesign
   them ("`plan/` is being removed, so every comment that points at it must
   point at `docs/`").
3. **Scope**: the folders it may change, and by name the files it must not,
   above all generated ones (`*/wire/protocol.*`, `docs/reference/`), which
   are written again afterwards.
4. **How to find the work**: the exact command, `grep -rn … | grep -v …`.
5. **The decision, as a table**: every old value and its new one. A value
   the table lacks is left alone and reported, never guessed.
6. **Rules**: change only what the table names; keep the sentence, the
   comment style and the indentation; the width a rewrapped line keeps; the
   one word to change where the old one no longer fits.
7. **No git**: no commit, no stash, no formatter beyond the one named.
8. **The check**, with its command, which must pass: `bun run check`,
   `ruff check`, `gofmt -l .` and `go vet`.
9. **The report**: every file changed with its old and new line, what was
   left unmapped and why, and the check's output.

Effort `medium` is enough for a brief that decided everything; a model
above Haiku is a sign the chore still holds a decision.

## Several at once

Split by folder, one agent a folder, so that no two touch one file:
`sdk/js`, `sdk/python`, `sdk/go` with `protocol/`. They run in the
background in this checkout while you do the work that is yours; a file you
edit is never in an agent's scope.

## After they report

- Read each report for what was left unmapped or could not be checked: a
  reference to a section the new target does not have is a sentence you
  rewrite yourself.
- Grep again for the old value across the whole repository: an agent's
  "none left" covers only its scope.
- Write the generated files again (`just protocol`, `cd web && bun run
  reference`), then `just quick`, then commit the whole change yourself as
  one coherent commit.
