---
name: verify
description: Use before committing or pushing a change to TinyStore, when reporting which checks ran, and whenever a test fails or seems flaky - the checks CI runs, on Windows and in a Linux container, lint for each platform, the feature subsets, the Bun SDK's suites, and how to tell a flaky test from a broken one.
---

# Verifying a change

CI (`.github/workflows/ci.yml`) is the gate, and a red run is real. Every
push, to any branch, lints and tests the workspace on Linux, macOS and Windows,
each linting the code its platform compiles, builds the core's library and
`tinystore`, and runs the Bun SDK's suites over them on all three. Its
`quality` job checks formatting, that every codec and vector is what
`protocol/*.wire` writes, the Bun SDK's lint and types, and the workflows.
`just check` runs the same on this host, so that a failure arrives before the
push instead of after it. Say in your report which checks ran, where, and
which could not.

## Before every commit

- `just check`: formatting, clippy, every crate's tests, the protocol's files,
  the Bun SDK's suites, its lint and types (bun on `PATH`), and the workflows
  (Docker).
- `just test-linux`: clippy and the tests in the `rust:1.99` container, since
  `cfg` hides Linux's code from a Windows host.
- A change to an engine's seams, or to anything behind `#[cfg(feature = …)]`:
  `cargo clippy -p tinystore --no-default-features --features <set> --all-targets -- -D warnings`
  for `kv`, `kv,sql` and `kv,jobs`. CI builds the default features only.
- Docker Desktop may be stopped: `platform-traps` says how to start it.
- Never while a measurement runs: see research's `measure` skill.

## A test that fails, or seems to

- Reproduce before theorising: the test alone a few hundred times,
  `cargo test -p tinystore --lib <name>` in a loop, then its module's tests
  while another suite runs beside it for load. A handoff's "failing" test once
  passed all of that, while a timing test beside it failed under load.
- A test that passes alone and fails under load usually times something from
  the wrong start: take the start before the thing whose clock you compare
  with, or give the test the store's `TestClock`.
- Prove a new test tests something: break the code it guards for a moment,
  watch it fail, put the code back, and check with `git diff` that it is back.
- Never weaken or delete a test to make a change pass; when behaviour changes
  on purpose, change the test and say what the new contract is.
- A failure in CI: `gh run list --branch <branch>`, then `gh run view <id>
  --log-failed` for the failing steps' output.

## After the checks

State what changed, which checks ran on which platform, what could not run and
why. Do not claim a platform was tested from another operating system: the
Linux container is not macOS, and only CI's macOS runner runs there.
