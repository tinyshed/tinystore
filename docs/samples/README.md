# Samples

Code that shows the shape of what `docs/architecture.md` and `docs/rewrite.md`
describe. None of it is product code, and nothing here is built by the module.

## runtime/: the API a program will use

A prototype of the runtime and three engines, kept as text so the module does
not build it:

| file | what it shows |
|---|---|
| `tinystore.go.txt` | the store: one directory, `Claim`, `Attach`, `Logger`, `Every`, self-metrics, one `Close` |
| `metrics/metrics.go.txt` | how an engine joins the store; a stand-in with the real engine's surface |
| `records/records.go.txt` | records.db and a `slog.Handler` that never blocks and refuses its own lines |
| `sqldb/*.go.txt` | the application's SQL: migrations, `One[T]`, `All[T]`, `Scalar[T]`, `Transaction` |
| `cmd/app/main.go.txt` | a program using all of it |

It compiled, ran and passed the repository's golangci-lint with Go 1.27.1 on
24 September 2026. To run it again:

```sh
cp -r <repo>/docs/samples/runtime /tmp/runtime-sample && cd /tmp/runtime-sample
find . -name '*.txt' -exec sh -c 'mv "$1" "${1%.txt}"' _ {} \;
go run ./cmd/app
```

It prints the store opening, two migrations applied, the user it wrote and
read back, a second `sql/app.db` refused with `ErrInUse`, the program's log
lines read back from `records.db`, a self-metric read back from metrics, and
the engines closing in reverse order.

## The reference rewrite

Branch `claude/tender-cannon-6z3mxv`, commit `407e728`: the metrics ingest
path, from `Ingest` to the packed head, moved function by function into the
style of `docs/rewrite.md`, with behaviour, bytes and speed unchanged. Read it
as a diff against `cad0ea8`:

```sh
git diff cad0ea8 407e728 -- metrics/
```

Start with `metrics/doc.go`, then `metrics/ingest.go`.
