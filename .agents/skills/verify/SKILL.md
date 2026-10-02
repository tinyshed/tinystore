---
name: verify
description: Use before committing or pushing a change to TinyStore, when reporting which checks ran, and whenever a test fails or seems flaky - the checks CI runs, on Windows and in a Linux container, lint for each platform, the race suite, the SDK suites, and how to tell a flaky test from a broken one.
---

# Verifying a change

CI (`.github/workflows/ci.yml`) is the gate, and a red run is real: every
push, to any branch, tests the three modules on Linux, Windows and macOS, with
the race detector off Windows, and both SDKs on all three; it lints as each
platform builds and checks formatting, tidiness, `govulncheck` and the size
probe. `task check` runs the same on this host, so that a failure arrives
before the push instead of after it. Say in your report which checks ran,
where, and which could not.

## The modules

The root, `server/` and `cmd/tinystore/`, each with its own `go.mod`; `tools/`
pins the linters and `task`; `sdk/go.mod` keeps the SDKs out of Go. `task`
runs with `GOWORK=off`. A local, ignored `go.work` lets an editor see every
module: a new module goes into it with `go work use ./<dir>`, or GoLand shows
it red. Anything that copies the repository elsewhere, a container included,
leaves `go.work` behind or sets `GOWORK=off`, since it names no `tools/`.

## Before every commit

- `task check`: tidy, format, lint as Linux, macOS and Windows build the code
  (`task lint:platforms`), the tests of the three modules, both SDKs (bun and
  uv on `PATH`), `govulncheck` and the size probe, as CI would.
- `task race:linux`: the race detector, which this Windows host cannot run
  without cgo, over the three modules in the `golang:1.27` container.
- `task sdk:linux` when a change touches what the SDKs do with processes,
  sockets, pipes or files: both suites in Linux, where a refused socket fails
  at once and pyright narrows platforms otherwise.
- A fallback behind build tags, such as `//go:build !unix && !windows`:
  `GOOS=plan9 go build .`.
- Docker Desktop may be stopped: `platform-traps` says how to start it.
- Never while a measurement runs: see research's `measure` skill.

## A test that fails, or seems to

- Reproduce before theorising: the test alone with `-count=300`, then its
  package with `-count=40 -shuffle=on` while another module's suite runs
  beside it for load. A handoff's "failing" test once passed all of that,
  while a timing test beside it failed under load.
- A test that passes alone and fails under load usually times something from
  the wrong start. `TestAServerGoesIdleAfterItsLastConnection` measured idle
  time from after the server was made, whose quiet began when it was made;
  take the start before the thing whose clock you compare with.
- Prove a new test tests something: break the code it guards for a moment,
  watch it fail, put the code back, and check with `git diff` that it is back.
  `TestAPrivateChildLeavesWhenToldThoughItsParentStays` hangs on the stdin
  handling it replaced; the proof's gate fails when the client skips its check.
- Never weaken or delete a test to make a change pass; when behaviour changes
  on purpose, change the test and say what the new contract is.
- A failure in CI: `gh run list --branch <branch>`, then `gh run view <id>
  --log-failed` for the failing steps' output.

## After the checks

State what changed, which checks ran on which platform, what could not run and
why. Do not claim a platform was tested from another operating system: linting
with `GOOS=darwin` is not running on macOS, and only CI's macOS runner runs
there.
