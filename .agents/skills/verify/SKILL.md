---
name: verify
description: Use before committing a change to TinyStore, when reporting which checks ran, and whenever a test fails or seems flaky - the checks CI would run, on Windows and in a Linux container, lint for each platform, the race suite, and how to tell a flaky test from a broken one.
---

# Verifying a change

CI on GitHub is off until the account's quota returns, so these local checks
are the gate. Say in your report which of them ran, where, and which could not.

## The modules

The root, `server/` and `cmd/tinystore/`, each with its own `go.mod`; `tools/`
pins the linters; `sdk/go.mod` keeps the SDKs out of Go. `task` runs with
`GOWORK=off`. A local, ignored `go.work` lets an editor see every module: a new
module goes into it with `go work use ./<dir>`, or GoLand shows it red.

## Before every commit

- `task check`: tidy, format, lint and tests of the three modules,
  `govulncheck` and the size probe, as CI would.
- Code behind build tags: lint it for the other platforms too, from inside
  each module:

  ```sh
  GOOS=linux ../bin/golangci-lint run ./...
  GOOS=darwin ../bin/golangci-lint run ./...
  ```

  and build a fallback such as `//go:build !unix && !windows` with
  `GOOS=plan9 go build .`.
- Linux and the race detector, which Windows cannot run without cgo, in the
  container, from Git Bash with `MSYS_NO_PATHCONV=1`, again with
  `-w /src/cmd/tinystore` and `-w /src` for those modules:

  ```sh
  docker run --rm -v <repo>:/src -v tinystore-race-cache:/go -v tinystore-gocache:/root/.cache/go-build \
    -e GOWORK=off -e CGO_ENABLED=1 -w /src/server golang:1.27 go test -race -count=3 -shuffle=on ./...
  ```

- Never while a measurement runs: see the `measure` skill.

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

## After the checks

State what changed, which checks ran on which platform, what could not run and
why. Do not claim a platform was tested from another operating system: linting
with `GOOS=darwin` is not running on macOS.
