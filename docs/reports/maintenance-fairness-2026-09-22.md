# Fair selection of ready series

The second change addresses D2 in
[the execution review](execution-review-2026-09-22.md). `Maintain` now starts
its indexed ready-series scan after the last selected `series_id` and wraps to
the start when needed. The cursor belongs to the open store handle. It does not
change the stored format, frontier rules or `MaintenanceSeries` bound.

## Evidence and environment

Windows 11, Ryzen 7 7700, Go 1.27.1 windows/amd64. The in-package fixture
uses two ready series with 241 samples each and `MaintenanceSeries=1`. After
each pass the first series can receive another 240 samples, so it remains ready.
The pre-change diagnostic probe is in
[engine-audit-2026-09-22-probes](engine-audit-2026-09-22-probes/), and was
observed again before this edit. The new passing gate is
`TestMaintenanceRotatesReadySeries`.

| state | pass at which series 2 first sealed |
|---|---:|
| old smallest-id selection | later than pass 5; still unsealed after five passes |
| rotating selection | pass 2 |

This is an operational fairness result, not a throughput result. The wrapped
case executes a second bounded index scan, whose cost has not yet been isolated
under a large ready queue. Expiry still uses its due-time ordering. This change
does not isolate corrupt or over-limit series, provide quarantine recovery or
remove the `clearReady` churn identified in D1 and the audit.

## Reproduce

On the pre-change revision, the archived diagnostic is runnable with:

```powershell
pwsh -NoProfile -File <repo>/docs/reports/engine-audit-2026-09-22-probes/run.ps1
```

On the current branch, run the new gate:

```sh
go test ./metrics -run TestMaintenanceRotatesReadySeries -count=1 -v
```

The archived script is tied to the audited revision: its separate
`internal/sqlite` probe names the old `readers` field and does not compile after
the reader-pool change. Its metrics starvation probe did pass before this edit.

## Checks

`go test -shuffle=on ./...`, `task fmt:check` and `task lint` passed after this
change; `go test -race ./metrics` passed in the Go 1.27.1 Linux container.
The earlier `task check` attempt reached `task size` but could not run its
`wc` command on this Windows PATH; see
[the reader-reuse report](reader-reuse-2026-09-22.md).
