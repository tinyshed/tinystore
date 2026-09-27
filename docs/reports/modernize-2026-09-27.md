# Go 1.27 syntax and IDE inspection triage — 2026-09-27

The root module declares `go 1.27.0` and builds with
`toolchain go1.27.1`. The modernizations here stay within that floor.
`go fix ./...` was applied to the root module, then `go fix -diff ./...`
returned no remaining edits. The independent `bench/perf`, `bench/records`
and `bench/tsbs` modules also received their suggested fixes and now have
empty `go fix -diff` output. The other benchmark modules had no suggestions
except `bench/kv`'s `errors.As` check, discussed below.

The root changes use `strings.SplitSeq` for a ranged split, `maps.Copy`,
`slices.Contains`, `bytes.Cut`, `errors.AsType`, and Go 1.26's `new(expr)`
in the historical spike fixture. The spike's now-unused pointer helper was
removed. The benchmark runners use `slices.Sort`, `SplitSeq` and
`CutSuffix` where the modernizer found equivalent code. These changes do
not alter the storage format or SQL schema. The
[Go 1.27 release notes](https://go.dev/doc/go1.27) describe the new
modernizers; [`errors.AsType` is documented by Go](https://pkg.go.dev/errors#AsType)
as available since Go 1.26.

For `records.mergeStream` and `sealHead`, `go fix` first converted an
`errors.As` whose extracted value was not used into `errors.AsType` with a
discarded first result. The repository's `errcheck` policy rejects that
discard. The final code uses the matched `*DamageError` to record the damage
and then continues or counts it; no `//nolint` is needed. The existing
`records.noted` also uses `AsType` and the same one-time damage recorder.
`bench/kv` retains its old `errors.As`: its violation value is intentionally
not otherwise needed, and fabricating a use merely to satisfy `errcheck`
would make the benchmark less clear. No lint rule was weakened.

## Which GoLand findings mattered

- **Real bug found by the full race run:** `examples/notes.useJobs` incremented
  one counter from two workers without synchronization. It now uses
  `atomic.Int64`, and checks the `Scan` and `fmt.Fprintf` errors on that path.
- **Real dead declaration:** `jobs/queue.go:firstDue` was unused and was
  removed. Exported `sqldb.ExecAll` and engine `ErrOutcomeUnknown` values
  stay: internal-use analysis cannot decide a library's public API.
- **Resource-leak warnings investigated:** the reported
  `records/damage.go` and `records/retention.go` row iterators are handed to
  `sqlite.EachRow`, which checks `Rows.Err` and closes them. No leak was
  established in those paths.
- **Nil warnings investigated:** a test's `t.Fatalf` stops its goroutine,
  and the `records/level_test.go` dereference is guarded by a short-circuit
  condition and an earlier consistency check. The screenshot's broad
  `(value, error)` warnings alone do not prove an invalid value is used.
- **Missing `iota` cases:** some switches intentionally leave their zero
  value or an already-validated variant to a fallback. Adding cases
  mechanically could change error handling.
- **Weak style suggestions:** duplicate test setup, mixed value/pointer
  receivers and partial syntax rewrites need context. They were not used as
  a target count. In particular, receiver changes can alter copying and
  method sets, so they are not a cosmetic modernization.
- **README parser errors:** `sqldb/README.md` contains fenced Go examples,
  including `//go:embed`; GoLand's selected-directory inspection reported
  `package expected` and embed placement on Markdown snippets. They are not
  Go source failures. Limit the Go inspection scope to Go files or exclude
  injected Markdown fragments from that inspection.

The local ignored `go.work` was also missing `bench/kv` and
`bench/records`, both independent modules. Those entries were added for
the IDE; `go list` resolves both now. It is not a tracked project change.

## Verification

`task check` passed on Windows after `go fix`: module tidiness,
formatting, lint with zero findings, shuffled tests, vulnerability scan and
the cgo-free import probe. The modified bench/perf, bench/records and
bench/tsbs modules passed `go test ./... -run '^$'` with `GOWORK=off`,
`CGO_ENABLED=0`; these modules have no test cases, so that verifies their
builds. The root and those three modules have empty `go fix -diff` output.

`go test -race -shuffle=on -count=1 ./...` passed in the Go 1.27 Linux
container after the example counter fix. The previous full run had
reported the concurrent increments at `examples/notes/main.go:217`; the
repeated run shows no race. `go -C bench/kv test ./... -run '^$'` also builds
the unchanged benchmark module.

The remaining IDE warnings are not all certified false positives. An
`Unhandled error` or nil warning in production code deserves review at
its exact call and line before changing it. This round fixes the one
confirmed race and the dead constant, applies the standard modernizers,
and leaves the rest as an inspection backlog rather than suppressing
entire categories.
