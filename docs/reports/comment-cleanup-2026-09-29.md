# Comment cleanup on 29 September 2026

A pass over every comment in the repository, to make the long ones readable
without losing what they say. Both rounds are done and the checks below pass;
**nothing is committed**. The tree also holds a person's code edits made in
parallel (`kv`, `cmd/tinystore`, `internal/sqlite`, `sqldb/call.go`), so
`git diff` mixes the two: 236 files, 2,225 insertions and 1,478 deletions.

## Scope and method

- Everything `git ls-files '*.go'` lists, except `spike/`, `bench/` and
  `server/spike/`: about 2,600 comment blocks, production code and tests.
  The other Go files were read block by block; nothing was sampled.
- Round 0, the audit. Four readers, three of them Sonnet agents working
  read-only and one by hand, classified each block as keep, delete, trim, split,
  English or stale, checking the code under any block whose verdict depended on
  it. Directories: `records` + `blobs`, `server` + `sqldb`, `kv` + `jobs`, and
  the root, `codec`, `backup`, `metrics`, `internal`, `cmd`, `examples`.
- Round 1, the rewrite. The same four, each in its own directories, comments
  only.
- Round 2, in progress, after two rules changed: wrapped prose back to 80
  columns, and a type comment that explains individual fields moves onto the
  fields.

The contract is the "Comment prose" bullet in `AGENTS.md` ("Editing rules").
It replaced "one line, lower case, no closing full stop".

## What the audit found

| Area | Blocks | Needed an edit | Stale |
|---|---|---|---|
| `records`, `blobs` | 780 | 59 | 3 (soft) |
| `kv`, `jobs` | 534 | 60 | 5 |
| `server`, `sqldb` | 840 | about 82 | 7 |
| root, `codec`, `backup`, `metrics`, `internal`, `cmd`, `examples` | 443 | about 18 | 0 |

About 8 % of blocks had a defect; the rest were kept as they were. The
substance was right almost everywhere. The defects were of four kinds:

- **Run-ons.** One sentence of 40 to 130 words, chained with `;` and `,`, on
  the central functions (`UpdateGrouped`, `Lines`, `routeToHeads`, `Work`,
  `checkDataSQL`).
- **Restatements.** A comment that says its name (`Engine is what the store
  needs from an opened engine`), and the same sentence on every engine's
  `admit` and `holdMaintenance`. About 90 were deleted.
- **Broken English.** Among them a sentence cut off mid-clause in
  `cmd/tinystore/find.go`, another in `Config.MaxLength`, a third in
  `groupedWriter`, and the text of `errAdminOnly`.
- **Stale claims.** A second mechanism was added later and the comment was not
  updated.

Stale claims fixed in round 1, each checked against the code:

- `refusedOpcodes` in `server/datasql.go` said it covered attach and detach.
  The map has no such opcode; `guarded.refuses` refuses them by the name of
  the function. This is on the security boundary.
- `Serve` and `ServeConn` said a connection lives until it ends or `Close`.
  It also ends when `ctx` ends.
- `handles.go` said the session's end closes the handles. Nothing closes
  them; they are dropped with the session.
- `openQueue` in `jobs/queue.go` said it counts the jobs and sets the alarm.
  Since the `MaxWaiting` triggers moved into the file it reads the count and
  the alarm rings at once.
- `codecFor` in `kv/values.go` omitted `int` and `uintptr`; `scheduleRenewals`
  was cited as `schedule`; `Entry.Repeat` said "cron text and zone" though an
  `Every` repeat has no zone.
- `records/codec.go` gave the head row's hour as a rule; it is the default of
  `Options.SealAge`. A crash-trigger test comment said the first block for the
  second.
- The file lists in `server/doc.go`, `server/wire/doc.go` and `sqldb/doc.go`.

## State after round 1

| Directories | Deleted | Rewritten or split | Tightened |
|---|---|---|---|
| `records`, `blobs` | 23 | about 27 | about 95 |
| `server`, `sqldb` | 30 | 23 | 51 |
| `kv`, `jobs` | 21 | 15 | about 60 |
| root, `codec`, `backup`, `metrics`, `internal`, `cmd`, `examples` | 6 | about 20 | about 15 |

`internal/sqlite` was done by hand first, as the trial slice.

Blocks of three or more lines that are one plain sentence, production and
tests: 264 before, 11 after round 1 (nearly all `// Output:` blocks, which are
not touched).

Code changed by this work: one string, the message of `errAdminOnly` in
`server/datasql.go`, from `an admin connection's to make` to `only an admin
connection can make this repair`. No test, document or client matched the old
text. Nothing else in a `.go` file is code.

## The width correction

Round 1 wrote wrapped prose to about 110 columns. That was wrong for this
repository, where comments had been wrapped at about 80, and it made the long
ones hard to read. A script re-wrapped every prose paragraph that had a line
past 84 columns to 80, tabs counted as four, leaving examples, tables, lists,
directives and `// Output:` blocks alone; a single line may run to 100.

| Comment lines | before | after |
|---|---|---|
| 80 columns or fewer | 4,259 | 5,181 |
| 81 to 100 | 305 | 166 |
| past 100 | 468 | 0 |

The rewrap made 59 two-line comments into three-line single sentences. Those
are the first work of round 2.

## Round 2, done

- The 59 three-line single sentences, by hand. The sweep now finds only
  `// Output:` blocks of example tests and one list in `blobs/read_test.go`.
- Type comments that explain fields, moved onto the fields (`blobs.Object`,
  `records.Record`, `Query`, `Page`, `Stats`, `Maintenance`, `SnapshotFile`,
  `metrics.Stats`, `sqldb.ConstraintError`, the `server/wire` mirrors and
  others). A field whose name and type already say it has no comment. A type
  comment stays where the relation between the fields is the point
  (`records.Cursor`, `RecordsDamage`).
- The files a person was editing in parallel were then rewrapped too.
- Wrapped prose is at 80 columns with tabs as four. The remaining exceptions
  are a summary line of up to 100 columns above a paragraph, and doc art.

## Checks that ran, and what did not

Run on Windows only, at the end of round 2:

- The code with its comments and all whitespace removed is identical to a
  snapshot taken before round 1, file by file, from the printed syntax tree,
  except in 11 files: `server/datasql.go` (the approved `errAdminOnly`
  string) and ten that a person changed in parallel (`cmd/tinystore/serve.go`
  and `serve_test.go`, `internal/sqlite/group.go` and `group_test.go`,
  `kv/clear.go`, `expiry.go`, `expiry_test.go`, `kv.go`, `options.go`,
  `sqldb/call.go`). Comment-inserting edits also make gofmt realign struct
  fields, so a comparison that keeps whitespace flags a few more.
- `gofmt -l` prints nothing. `go vet` is clean for the root, `server` and
  `cmd/tinystore`, and for the root and `server` with `GOOS=linux`, and the
  root with `GOOS=darwin`.
- `go test -count=1 -shuffle=on ./...` passes in the root, in `server` and in
  `cmd/tinystore`, all after the last edit.

Not run: `task lint` and `task check`, the race detector, Linux or macOS
tests. The scripts (block extraction, the syntax-tree comparison, the rewrap,
the sweep and field-comment finders) live outside the repository.

## Open, for the authors

- A comment that names a report or a document by path (`docs/reports/...`,
  `docs/group-commit-contract.md`, `docs/blobs.md`, `docs/server.md`) will
  break when research moves to `tinyshed/research`. They were kept; count them
  and decide once, when the move is done.
- `groupedWriter` in `internal/sqlite/group.go` ended "which only a statement
  that has outrun its deadline is worth"; it now ends "is worth that". The
  ending is inferred.
- `blobs/put_compare_test.go` said "both binaries" without saying which two;
  it now says "every run compared". `records/read_round_test.go` keeps a remark
  about the Read before the budget as history, unconfirmed.
- `Maintain` in `kv/expiry.go`: the limit of ten transactions a call is scoped
  to expired keys, and cleared rows have one per mark.
- `jobs/doc.go` says Work is a loop over Claim and Ack, Retry and the rest; the
  in-process loop settles and claims in one grouped write. Accurate for the
  server's protocol, and left.
- `limits` in `server/server.go` cites "docs/server.md's proposals"; it was
  not checked against `defaultLimits`.
- `distance`, `advance` and `foldSigned` are written twice, in `codec` and in
  `metrics`. Code, not a comment matter; not touched.
