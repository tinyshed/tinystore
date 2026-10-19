---
name: verify
description: Use before committing or pushing a change to TinyStore, when reporting which checks ran, and whenever a test fails or seems flaky - just quick and just check, CI on Linux, macOS and Windows, lint for each platform and feature set, every SDK's suites, timings, and how to tell a flaky test from a broken one.
---

# Verifying a change

CI (`.github/workflows/ci.yml`) is the gate, and a red run is real. Every
push, to any branch, lints and tests the workspace on Linux, macOS and
Windows, each linting the code its platform compiles and the library with
each feature set alone, then builds the core and `tinystore` and runs Bun's
suites, Go's, Python's and Bun's again under Node over them. Its `quality`
job checks formatting, that every codec and vector is what `protocol/*.wire`
writes, each SDK's lint and types, and the workflows. Say in your report
which checks ran, where, and which could not.

## Before every commit

- `just quick`: what changed since the branch's upstream, the working tree
  and new files included, in minutes: formatting, clippy, the tests of each
  area touched, every lint set that names it, and each SDK the change
  reaches. Its table of what runs for what is at the top of
  `tools/quick.ts`. Then commit and push: the owner does not wait on a
  local full run.

## After the push

- CI is the full run: `gh run list --branch main`, then `gh run view <id>
  --log-failed` for what failed, fixed in the next commit. A push cancels
  the run before it on the same branch, and a change of `docs/`, `web/`,
  `.agents/` or the root's README and AGENTS alone starts none.
- `just check` runs all of CI here, half an hour: for a change too wide for
  `just quick`, or a platform's failure to reproduce before pushing again.
- Run `just` from Git Bash on Windows: a recipe runs in PowerShell, and
  `sdk-go`'s `test -z` finds `test` only on Git's `PATH`.
- `just lint-actions` needs Docker; Docker Desktop may be stopped
  (`platform-traps`).
- `just test-linux`: clippy and the tests in a Linux container, for a host
  that is not Linux; CI runs Linux anyway.
- Never while a measurement runs: a build beside it moves its numbers.

## A test that fails, or seems to

- Reproduce before theorising: the test alone a few hundred times,
  `cargo test -p tinystore --lib <name>` in a loop, then its module's tests
  while another suite runs beside it for load.
- A test that passes alone and fails under load usually times something
  from the wrong start: take the start before the thing whose clock you
  compare with, or give the store a `TestClock`.
- A test that hangs under Node, where Bun passed: a store the test never
  closed keeps Node's loop alive through the addon's wake, so the file's
  process never exits.
- Prove a new test tests something: break the code it guards for a moment,
  watch it fail, put the code back, and check with `git diff` that it is.
- Never weaken or delete a test to make a change pass; when behaviour
  changes on purpose, change the test and say what the new contract is.
- A failure in CI: `gh run list --branch <branch>`, then `gh run view <id>
  --log-failed`.

## Timings

A timing beside the code is an `#[ignore]` test, run by hand with
`cargo test --release -p tinystore --lib <name> -- --ignored --nocapture`.
It compares what it measures against what it replaces in the same run, in
turns, the median of several passes, and its numbers go into a research round with the machine, the versions and the command (AGENTS.md
"Measurements"). A run whose passes differ by more than the effect measured
says nothing: make each pass longer before believing it.

## After the checks

State what changed, which checks ran on which platform, what could not run
and why. Do not claim a platform was tested from another operating system:
the Linux container is not macOS, and only CI's macOS runner runs there.
