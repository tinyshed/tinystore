# records

An application's structured logs, in `records.db` inside a `tinystore.Store`,
written through `log/slog` and read back by time and level.

```go
logs, err := records.Open(ctx, store, records.Options{Retention: 14 * 24 * time.Hour})

logger := slog.New(slog.NewMultiHandler(console, logs.Handler()))
logger.Warn("slow request", "route", "/notes", "ms", 1200)

lines, err := logs.Read(ctx, records.Query{
	From:     time.Now().Add(-time.Hour),
	To:       time.Now(),
	MinLevel: slog.LevelWarn,
	Limit:    100,
})
```

## Contracts

- The handler never blocks its caller. Records wait in a buffer
  (`Options.Buffer`, 1024) and are written in one transaction every
  `Options.Flush` (a second), on `Close`, or on `Flush(ctx)` in a Manual store;
  when the buffer is full a record is dropped and counted in `Stats`, and so is
  a batch whose write failed. A process that dies loses what was waiting.
- Lines of the records engine itself (`engine=records`, which the store's
  logger adds) are refused, or writing a log would log again.
- Attributes keep their keys, a group's as `group.key`; an error is stored as
  its message, and values read back as `encoding/json` decodes them.
- `Read` returns records in `[From, To)` at `MinLevel` or above, oldest first,
  at most `Limit` (1000 when zero; more than 10000 is `tinystore.ErrInvalid`).
  A zero `MinLevel` is `slog.LevelInfo`, as in `slog`.
- Records older than `Options.Retention` (fourteen days) by the store's clock
  are deleted every hour, or on `Maintain(ctx)` in a Manual store.
- Records are indexed by time only; an index by attribute waits for a query
  that needs one.
