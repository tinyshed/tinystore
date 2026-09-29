# TinyStore

An embedded data runtime for Go, on SQLite: metrics, records, SQL databases,
key-value state, files and jobs in one directory, a file per engine, with
bounded memory, no daemon and no cgo. Bun and Python reach the same directory
through `tinystore serve`.

**Unreleased: the API moves without notice**, and no file written by an
earlier revision has to be read.

```go
store, err := tinystore.Open(ctx, "./data", tinystore.Options{})
if err != nil {
	return err
}
defer store.Close()

state, err := kv.Open(ctx, store, kv.Options{})
if err != nil {
	return err
}
drafts, err := kv.OpenBucket[string](ctx, state, "drafts")
if err != nil {
	return err
}
if err = drafts.Set(ctx, "note/1", "hello"); err != nil {
	return err
}
text, found, err := drafts.Get(ctx, "note/1")
```

[examples/notes](examples/notes/main.go) is a program using every engine.

## Engines

| Package | What it keeps | File |
|---|---|---|
| [metrics](metrics/README.md) | samples, bit for bit, answered exactly | `metrics.db` |
| [records](records/README.md) | logs and events, read by time, level and keys | `records.db` |
| [sqldb](sqldb/README.md) | the application's own SQL, tables from structs, checked migrations | `sql/<name>.db` |
| [kv](kv/README.md) | current state: typed buckets, counters, expiry, versions | `kv.db` |
| [jobs](jobs/README.md) | work that runs at its time: retries, leases, repeats | `jobs.db` |
| [blobs](blobs/README.md) | files by path, checked when read whole | `blobs/` |
| [backup](backup/) | every engine's files in one checked zip | |

## Other languages

`tinystore serve` in [cmd/tinystore](cmd/tinystore/) serves a directory over
one protocol, [docs/wire.md](docs/wire.md), as a sidecar the SDKs start, a
private child, or a remote server with TLS and tokens. The clients are
[sdk/js](sdk/js/) for Bun and [sdk/python](sdk/python/README.md).

## Layout

| Path | What it is |
|---|---|
| `codec/` | the metrics block codec |
| `metrics/`, `records/`, `sqldb/`, `kv/`, `jobs/`, `blobs/` | one package per engine |
| `backup/` | a store's snapshot as one zip, and its restore |
| `internal/` | SQLite files and transactions, admission, the directory lock |
| `server/` | a module of its own: the store served to other processes |
| `cmd/tinystore/` | a module of its own: `serve`, and `migrate` and `schema` for sqldb |
| `sdk/` | the Bun and Python clients |
| `examples/` | programs using the public API, built and tested with it |
| `docs/` | the design, the formats and the wire protocol |
| `tools/` | a module pinning developer tools |

The measurements and prototypes behind the design are in
[tinyshed/research](https://github.com/tinyshed/research/tree/main/tinystore).

## License

Apache-2.0.
