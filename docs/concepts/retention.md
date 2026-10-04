# Retention

The records and metrics engines keep data for a fixed time, their retention,
and then remove it. Records keep 14 days and metrics 30 days by default. You
can keep some metric names or record streams longer or shorter than the rest,
for example an audit trail for a year and debug logs for a day.

## Keep some data longer

```go
logs, err := records.Open(ctx, store, records.Options{
	Retention: 14 * 24 * time.Hour, // every other stream
	RetentionOf: map[string]time.Duration{
		"audit": 365 * 24 * time.Hour,
		"debug": 24 * time.Hour,
	},
})

stats, err := metrics.Open(ctx, store, metrics.Options{
	Retention: 30 * 24 * time.Hour, // every other metric
	RetentionOf: map[string]time.Duration{
		"telemetry_":   5 * 365 * 24 * time.Hour,
		"http_request": 7 * 24 * time.Hour,
	},
})
```

Retention is an option of the Go program that opens the engines. A store that
the Bun or Python SDK starts as a sidecar keeps the defaults. To set them for
Bun and Python clients, open the store in Go and serve it with
[`server.Share`](../languages.md#share-a-go-programs-store).

## How a rule matches

- A records rule names a stream exactly: `audit` matches the stream `audit`
  and no other.
- A metrics rule is a prefix of the metric name. If several prefixes match,
  the longest wins: with `api_` and `api_audit_`, the series `api_audit_logins`
  keeps the retention of `api_audit_`, and `api_requests` the one of `api_`.

A stream or a name that no rule matches keeps `Retention`.

## What retention does

Each stream and each series uses its own retention for three things:

- **Writes.** A record or a sample older than its retention is rejected with
  a too-old error (`ErrTooOld` in Go, `TooOldError` in Bun and Python). See
  [Time](time.md#the-time-window).
- **Reads.** A read never returns data older than its retention, even before
  maintenance has removed it. A read across several streams or series clips
  each one at its own retention.
- **Removal.** Maintenance runs every minute and removes what has expired.
  Records go a whole segment at a time, once the newest record in it has
  expired. Metrics go a sample at a time from the head, and a compressed
  block at a time once its newest sample has expired.

## Change the rules

You change the rules by opening the engine with other options. The new rules
apply as soon as the engine opens:

- If a retention gets shorter, reads hide the older data at once, and the
  next maintenance removes it.
- If a retention gets longer, data that maintenance hasn't removed yet is kept
  for the new retention. Data that was already removed doesn't come back.

When the metrics rules change, `metrics.Open` updates the series of the names
whose rule changed, and only those. It finds them by their names, so a store
with many series doesn't read all of them.

## Limits and defaults

|                   | Records                   | Metrics                       |
|-------------------|---------------------------|-------------------------------|
| Default retention | 14 days                   | 30 days                       |
| A rule matches    | a stream's name exactly   | a prefix of the metric's name |
| Removed           | a whole segment at a time | samples, then whole blocks    |
| Maintenance       | every minute              | every minute                  |

## See also

- [Time](time.md): the store's clock and the time window.
- [records/README.md](../../records/README.md) and
  [metrics/README.md](../../metrics/README.md): the full contracts.
