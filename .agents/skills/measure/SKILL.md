---
name: measure
description: Use before running, reading or reporting any performance measurement of TinyStore - a benchmark, a research round, a comparison with a prototype or another engine, a CPU or allocation profile - on Windows or in the Linux container.
---

# Measuring TinyStore without fooling yourself

AGENTS.md holds the rules: a number with its environment, a candidate against
what it replaces on identical input in the same run, storage divided by object,
payload beside file. This is how to keep them.

## Before the run

- **Commit the harness first.** A report names the commit it measured; an
  uncommitted harness is an anecdote.
- **Measure the baseline in the same session**, interleaved: A, B, A, B, each
  at least twice. Never compare with a number from an older report: on
  29 September 2026 the same prototype on the same machine was 20 to 30 %
  slower than the day before, and a comparison with the old report invented a
  regression that was not there.
- **Keep the machine quiet.** No build, test or lint while a measurement runs,
  on Windows either: the Linux container is WSL2's virtual machine on the same
  processors. Do not edit code the harness builds while it runs;
  `server/spike` builds `tinystore` from the working tree at every pass. Prepare
  other work in a separate worktree meanwhile.

## The Linux container

From Git Bash, with `MSYS_NO_PATHCONV=1`:

```sh
docker run --rm -v <repo>:/src -v tinystore-perf:/perf -v tinystore-race-cache:/go \
  -v tinystore-gocache:/root/.cache/go-build -e GOWORK=off -e CGO_ENABLED=0 \
  -e TINYSTORE_SPIKE=1 -e TINYSTORE_RPC_DIR=/perf/rpc \
  -e PATH=/perf/bin:/go/bin:/usr/local/go/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
  -w /src golang:1.27 sh <script>
```

- Stores on the volume (`TINYSTORE_RPC_DIR`), never on the bind mount.
- Bun is `/perf/bin/bun` on the `tinystore-perf` volume; `python3` is in the
  image.
- Build test binaries once with `go test -c`, then run each test as a process
  of its own.
- One call in flight in the container measures waking WSL2's virtual
  processors, not Linux; quote rates from 64 in flight up, and latencies from
  bare Linux only.

## Explaining a number

- A profile suggests; a second measurement that separates the explanation
  from its alternatives confirms. Until then write the explanation as a
  hypothesis. `TestGetsWithAContextThatCanEnd` is the pattern: the suspected
  cause alone, both ways, in one process.
- Profile the process that serves, between a start and a stop its parent
  gives, with its `runtime.MemStats` for allocations a call, as
  `TestServeProfile` does; views: `go tool pprof -top`, `-top -cum`,
  `-peek 'runtime\.cgocall$|runtime\.semasleep$|runtime\.futex$'`.
- CPU a call is the profile's samples over the calls made while it ran; it
  compares servers whose rates hide different amounts of spinning.
- Traps that have moved numbers here: a goroutine started for each call grows
  its stack into SQLite every time; Go's default GC target collects hundreds
  of times a second; `database/sql` starts a goroutine per query whose context
  can end; on Windows the scheduler's work stealing spins in `osyield`.

## The report

- Keep the raw output and publish it beside the report with machine paths
  replaced: `<repo>`, `<tmp>`, no home directory, since a home directory is a
  username published for as long as the history lasts.
- The shape: a table of question, answer and what follows; *Environment and
  reproduction* with the commits, the machine, the versions, the order of the
  runs in UTC and the commands; tables whose cells are the passes, `a; b`;
  a finding per section; *What follows*.
- File and index it where AGENTS.md says rounds go, and say in the document
  that asked for it (`docs/server.md` and the like) what it found.
