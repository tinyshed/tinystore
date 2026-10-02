---
name: platform-traps
description: Use when writing or debugging TinyStore code that touches files, directories, processes, stdin and stdout, sockets, named pipes, signals or timing, or when something behaves differently on Windows, Linux or macOS; also when editing tools or shell commands mangle backslashes and escapes.
---

# Platform traps TinyStore has already hit

Each line cost a debugging session once. The fix named is the one in the code.

## Windows files

- **A rename over or a removal of a file fails while another handle holds
  it** without sharing its deletion, which Go's `os.Open` and Python's `open`
  both do, and so may Defender scanning a new file: `ERROR_ACCESS_DENIED` or
  `ERROR_SHARING_VIOLATION`. Try again for a moment (`whileHeld` in
  `server/local.go`, blobs' rename); a reader should open, read and close at
  once.
- **A directory's `LOCK` is a file opened with no sharing**
  (`internal/dirlock`); a sharing violation means another store holds it, and
  the operating system lets go when the process dies.
- **A directory for its owner alone** is a protected DACL
  `(A;OICI;FA;;;<user sid>)`, which the files made in it inherit; reading it
  back, Windows adds `AI` of its own (`server/internal/private`).

## Windows pipes, processes and consoles

- **Named pipes share one namespace across every user.** Create the first
  instance with `FILE_FLAG_FIRST_PIPE_INSTANCE` and an owner-only DACL, reject
  remote clients, dial with `SECURITY_IDENTIFICATION`, and remember that a
  name a dead server left is anyone's: a client proves its server
  cryptographically (the `SERVE` proof).
- **A pipe is overlapped, given to `os.NewFile`**, since a synchronous handle
  holds every write behind a pending read; an `OVERLAPPED` the kernel keeps
  lives on the heap, since a goroutine's stack may move.
- **Closing stdin ends a read waiting on it only sometimes**: a pipe's close
  cancels it, a console's close waits for it, and on Unix closing a blocking
  stdin does neither. Read stdin on a goroutine of its own through `io.Pipe`
  and never close it (`cmd/tinystore/serve.go`).
- **`os.Process.Signal(os.Interrupt)` is not supported on Windows**: end a
  child by closing its stdin, or kill it.
- **`python3` may be the Microsoft Store's alias**, which only says Python is
  missing: run `py`, or look for `python` and check it runs.

## Unix and macOS

- **A Unix socket's path holds 107 bytes on Linux and 103 on macOS and the
  BSDs.** macOS's `$TMPDIR` alone is about 48 bytes, so a directory named after
  a store keeps its hash short: 16 hex digits, not 32.
- **`flock` goes when the process dies; the socket file stays.** Remove it
  under the lock before listening again, and remove a `SERVE` left behind
  before listening, since its endpoint may be another's by now.
- **A private directory is 0700 on a directory the user owns**, and one another
  user owns is refused rather than taken.

## Go and its libraries

- **`database/sql` starts a goroutine for every query whose context can end**
  (`Rows.initContextClose`); the driver checks the context inside SQLite
  instead, and answers `INTERRUPT`, which `internal/sqlite` turns back into the
  context's error. With `modernc.org/sqlite`, which started a goroutine of its
  own, a point read with a request's context was 27 to 35 % slower at depth
  than with `context.Background`. A statement by key goes
  through `sqlite.QueryRowByKey`, which checks the context and runs without its
  cancel; `TestAStatementByKeyStartsNoGoroutine` counts them with
  `/sched/goroutines-created:goroutines`.
- **SQLite rolls back the whole transaction of a write statement it
  interrupts**, not only its savepoint: a caller cancelling mid-statement
  failed every write of its group. Grouped statements run without their
  caller's context; `sqlite.UntilDeadline` is the one way back, for the
  application's SQL.
- **A goroutine started for each call grows its stack into SQLite every
  time**, and Go's default GC target collects hundreds of times a second: the
  server keeps workers and sets GOGC 400.
- **Never import `github.com/ncruces/go-sqlite3/driver`.** It registers
  `sqlite3` when it loads, as `mattn/go-sqlite3` does, cgo or not, and a
  program linking both panics before `main`; it also read a time into TEXT
  that looked like one, and scanned a blob into a `*[]byte` by appending to
  what the slice held. `internal/sqlite`'s own driver does neither, and
  `sqlite.OpenDB` opens a database a test or a catalog keeps for itself;
  `TestTheExampleRegistersNoSQLDriver` fails when a name is registered.
- **Windows' scheduler spins in `osyield`** while it steals work, visible as a
  large flat share in a profile when goroutines hand work back and forth.
- **A timing test compares with the clock of the thing it tests**: take the
  start before the object whose own clock began when it was made.

## Tools

- **The file-writing tool and quoted heredocs may turn `\uXXXX`, `\\` and
  `\n` into the characters themselves**: build backslashes in a script with
  `chr(92)`, or change the file with the exact-match editor.
- **Git Bash rewrites `/paths` in arguments**: run `docker` with
  `MSYS_NO_PATHCONV=1`.
- **The race detector needs cgo**, which Windows does not have here: run
  `-race` in the Linux container.
