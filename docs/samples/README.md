# Samples

Code that shows the shape of what `docs/rewrite.md` describes. The runtime
prototype that used to live here was replaced by
[examples/notes](../../examples/notes/main.go), which the module builds and
tests; it is in the history before that commit.

## The reference rewrite

Commit `407e728`, merged into `main`: the metrics ingest path, from `Ingest` to the packed head, moved function by function into the
style of `docs/rewrite.md`, with behaviour, bytes and speed unchanged. Read it
as a diff against `cad0ea8`:

```sh
git diff cad0ea8 407e728 -- metrics/
```

Start with `metrics/doc.go`, then `metrics/ingest.go`.
