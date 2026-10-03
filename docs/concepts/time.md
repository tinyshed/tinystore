# Time

Every engine reads the time from one clock, the store's. Expiry, schedules,
retention and the time window for records and metrics all use it, and a Go
test can replace it. This page explains the clock and the time window.

## The store's clock

```go
now := store.Now() // the store's clock

store, err := tinystore.Open(ctx, dir, tinystore.Options{
	Clock: func() time.Time { return testNow }, // a clock for tests
})
```

A call reads the clock once and uses that time for all of its work. In Bun and
Python, the store's clock is the clock of the server process. The server sends
its time when a client connects, and the SDK prints a warning if its own clock
differs from the server's by more than the time window allows.

## The time window

Records and metrics store observations: things that happened at a time. They
accept a time only within a window:

```text
now 12:00, retention 14 days, clock skew 10 minutes

older than 14 days ago      rejected: too old, no read would ever return it
between 14 days ago and 12:10   accepted
after 12:10                 rejected: too new
```

A record from a browser whose clock says 2100 would otherwise keep its storage
alive until 2100, and a metrics sample from a wrong clock would block the
series' newer samples. A rejected time fails with `ErrTooOld` or `ErrTooNew`
(`TooOldError`, `TooNewError` in Bun and Python), and the error names the
record or the series.

The window trusts the store's clock. If your server's clock runs behind, it
rejects correct data until you fix the clock or raise the clock skew
(`ClockSkew` in the engine's options).

For events from clocks you don't control, such as browsers, store the time
when you received the event, and keep the device's time as an attribute.

## Schedules are not observations

A job's time, a key's expiry and a blob's expiry are plans for the future,
not observations, so they have no window. A job's time in the past means
"run now", and an expiry in the past means the key is already expired.

## Time zones

A repeating job needs a time zone with a name, such as `Europe/Berlin`. On a
day when daylight saving time skips an hour, a job scheduled in that hour runs
at the first minute after it. On a day when an hour repeats, it runs once.

## How times are stored

| Where | Stored as |
|---|---|
| SQL (`time.Time` in Go) | unix milliseconds, in UTC |
| metrics samples | unix milliseconds |
| records | unix nanoseconds; a `bigint` in Bun |
| the wire protocol | unix milliseconds, nanoseconds for records |

Unix milliseconds fit a JavaScript number exactly, and SQLite's date
functions read them with `datetime(created_at / 1000, 'unixepoch')`.

## See also

- [Expiry](../kv/expiry.md) and [Schedules](../jobs/schedules.md).
- [Testing](../running/testing.md#control-time): move the clock in tests.
