---
name: platform-traps
description: Use when writing or debugging TinyStore code that touches files, directories, processes, stdin and stdout, sockets, named pipes, signals, timing or packaging, or when something behaves differently on Windows, Linux or macOS; also when editing tools or shell commands mangle backslashes and escapes.
---

# Platform traps TinyStore has already hit

Each line cost a debugging session once. The fix named is the one in the code.

## Windows files

- **A rename over or a removal of a file fails while another handle holds
  it** without sharing its deletion, and so may Defender scanning a new file:
  `ERROR_ACCESS_DENIED` or `ERROR_SHARING_VIOLATION`. Try again for a moment,
  as blobs' rename does (`crates/tinystore/src/blobs/disk.rs`); a reader
  opens, reads and closes at once.
- **A directory's `LOCK` is a file opened with no sharing**; a sharing
  violation means another store holds it, and the system lets go when the
  process dies.
- **A loaded DLL or `.node` addon cannot be written over**: a Node process
  left running from an earlier test still holds `tinystore.node`, and the
  next build's copy fails with "used by another process". Find it and stop
  it before building again.

## Windows pipes, processes and consoles

- **Named pipes share one namespace across every user.** The first instance
  is made with `first_pipe_instance` and an owner-only DACL
  (`crates/cli/src/serve.rs`), and a name a dead server left is anyone's: a
  client proves its server cryptographically, the `SERVE` proof.
- **Closing stdin ends a read waiting on it only sometimes**: a pipe's close
  cancels it, a console's close waits for it. A private child is ended by
  closing its stdin, and killed when it does not leave.
- **A child told to leave holds its directory until it has exited**: its
  `LOCK` everywhere, and on Windows every file it opened, so a removal right
  after fails with `EBUSY`. Every SDK's `close` waits for a private child's
  exit.
- **`python3` may be the Microsoft Store's alias**, which only says Python is
  missing: run `py`, or `python`, and check it runs.

## Unix and macOS

- **A Unix socket's path holds 107 bytes on Linux and 103 on macOS.**
  macOS's `$TMPDIR` alone is about 48 bytes, so a directory named after a
  store keeps its hash short: 16 hex digits, not 32.
- **`flock` goes when the process dies; the socket file stays.** Remove it
  under the lock before listening again, and remove a `SERVE` left behind
  before listening, since its endpoint may be another's by now.
- **A private directory is 0700 on a directory the user owns**, and one
  another user owns is refused rather than taken.
- **A `cdylib` on musl needs musl linked dynamically**: Rust's musl targets
  link it statically and refuse a `cdylib`, so a release builds them in
  Alpine with `-C target-feature=-crt-static` (`tools/native.sh`).
- **A JavaScript action runs no step in an Alpine job container on arm64**:
  the runner has no Node for it there. Linux's release builds and Alpine's
  smoke tests run through `docker run` from the host.

- **macOS's `/bin/sh` is bash 3.2**, which takes an apostrophe inside
  `"${VAR:?message}"` for a quote and fails at the end of the file:
  `unexpected EOF while looking for matching`. A script `sh` runs on macOS
  keeps every apostrophe out of `${…}`; `docker run bash:3.2 bash --posix -n
  script.sh` checks it from any machine.

## Rust and its libraries

- **Rust's stdout holds what it is given until a newline**: frames have none,
  so a private child's parent waited for its `WELCOME` for ever. Flush after
  every write (`write_frames` in `crates/cli/src/connection.rs`).
- **tracing's `LevelFilter` parses an empty string as `ERROR`**: read an
  empty or unknown `LOG_LEVEL` as `info` (`level_named` in
  `crates/cli/src/lines.rs`).
- **A synchronous home's write of `metrics.db` waits for a copy no node
  takes**, and answers `CommittedLocally` five seconds later: a node copies
  every file but its own records and metrics. The store's own writes run as
  its own work (`crate::sqlite::own_work`), which waits for no copy; the
  metrics engine's totals at close and the store's own metrics' names do.

## Node, Bun and npm

- **Node's test runner runs each file in a process of its own, and waits for
  it to exit**: a store left open keeps the addon's threadsafe function, and
  so the process, alive, and the run looks slow when it is stuck.
- **Bun installs an import from the registry by itself when there is no
  `node_modules`**: a script run beside an install that failed loads the
  published package, today the Go candidate's, and fails far from the cause.
- **npm and Bun take an optional dependency from the registry when its
  version is there**, even beside a tarball of the same name: the Go
  engine's `0.1.0-rc.1` to `rc.5` are, so a release packed under those
  numbers runs the Go executable. Try packages with a version nobody has
  published.
- **`bunx <name>` may run another program than the package's own**: call
  `./node_modules/.bin/<name>`.
- **Bun's `rmSync` ignores `maxRetries`**: on Windows a held file fails it at
  once with `EBUSY`. Try again in a loop of your own (`removed` in
  `sdk/js/test/ways.ts`).

## Tools

- **The file-writing tool and quoted heredocs may turn `\uXXXX`, `\\` and
  `\n` into the characters themselves**: build backslashes in a script with
  `chr(92)`, or change the file with the exact-match editor.
- **A long script inline in a shell command may be cut, or its quoting
  broken**, a heredoc inside the tool's own quoting above all: bash says
  `unexpected EOF while looking for matching`. Write the script to a file in
  the scratchpad and run the file. A script that edits files asserts each old
  text occurs exactly once before it replaces it, and is run once.
- **`just` runs its recipes in PowerShell on Windows**: `VAR=x cmd` and
  `test` are bash's, so a recipe that needs them says `sh -c "…"`, and `just`
  itself is run from Git Bash, whose `PATH` has `test`.
- **Git Bash rewrites `/paths` in arguments**: run `docker` with
  `MSYS_NO_PATHCONV=1`, and give `-v` a Windows path, `D:/dev/…`.
- **A bind mount from Windows into Docker is slow**: a release build of the
  workspace inside one takes twenty minutes and more.
- **Git Bash's tar takes `C:` in a path for a remote host**: give it a
  relative path, or `--force-local`.
- **A copy of the checkout carries the local `go.work`**, which names modules
  that moved: run Go with `GOWORK=off`.
- **Docker Desktop may be stopped**: `docker info` fails on the pipe
  `dockerDesktopLinuxEngine`. Start `C:\Program Files\Docker\Docker\Docker
  Desktop.exe` and wait until `docker info` answers.
