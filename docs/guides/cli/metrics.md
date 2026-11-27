# metrics

`tinystore metrics` reads and writes a store's metrics from a shell, with no
SDK. `names`, `read`, `aggregate` and `export` read beside the program or
server that holds the directory. `ingest` writes the Prometheus text that an
exporter serves, or that a script keeps printing. `replace` corrects a
gauge's samples over a range, and `delete` removes a series. All three write
through a server, as [every write](README.md#who-writes) of the command line
does.

```sh
tinystore metrics ./data names
tinystore metrics ./data read ping_rtt_ms --where target=poland --since 1h
tinystore metrics ./data aggregate ping_rtt_ms --since 1h --every 10m --op max --by target
tinystore metrics ./data export --only-prefix ping_ > ping.prom
curl -s localhost:9100/metrics | tinystore metrics ./data ingest --label instance=web-1
ping … | awk '…' | tinystore metrics ./data ingest -f
printf '2026-11-02T00:00Z 900\n' | tinystore metrics ./data replace revenue shop=msk --from 2026-11-02T00:00Z --to 2026-11-03T00:00Z
tinystore metrics ./data delete ping_rtt_ms target=poland
```

## See what a store holds

```sh
tinystore metrics ./data names
tinystore metrics ./data names http_      # names that start with http_
```

```text
ping_lost_total  counter      6 series  keep 30d    newest 2s ago
ping_rtt_ms      gauge        6 series  keep 30d    newest 2s ago
```

`--json` gives one object per name, with its unit, help and times in UTC. A
page holds 1000 names, and `--after` reads the next, as [Pages](README.md#pages)
says.

## Read samples

```sh
tinystore metrics ./data read ping_rtt_ms --where target=poland --from 14:32 --to 14:33 --gaps 5s
```

```text
ping_rtt_ms{target="poland"}   26 Oct 14:32–14:33 Europe/Moscow · 46 samples
  14:32:00   23.1
  14:32:01   22.9
  14:32:02   23.4
             · 15s without samples
  14:32:17   24.0
```

- The first line of each series shows its name and the range that was read.
  The range is in the command's [zone](README.md#how-it-behaves), which is
  the `--tz` zone or this machine's zone.
  Without `--since`, `--from` or `--to`, `read` reads the last hour.
- `--from` and `--to` take `14:30` for today, `2026-10-25T03:00` for a date,
  both in that zone, or a time with its offset, such as
  `2026-10-25T00:00:00Z`.
- `--where target=poland` keeps the series whose label matches. Use
  `status=500,502` for either value, `env!=dev` for neither, and `host=api-*`
  for a prefix. Give `--where` again for more labels.
- `--gaps 5s` marks every place where a series had no sample for longer than
  five seconds. Without it, `read` guesses nothing about a series' step.
- `read` prints a page of 1000 samples at most, and `--limit` takes up to
  100,000. The next page, read with `--after` as [Pages](README.md#pages)
  says, covers the same range as the first, even with `--since`.

`--json` prints one object per sample, its time in UTC:

```json
{"labels":{"target":"poland"},"name":"ping_rtt_ms","time":"2026-10-26T11:32:00Z","value":23.1}
```

A value that JSON can't hold, `NaN` or an infinity, comes as text: `"NaN"`,
`"+Inf"`, `"-Inf"`.

## Aggregate

```sh
tinystore metrics ./data aggregate ping_rtt_ms --since 1h --every 10m --op max --by target
```

```text
ping_rtt_ms max every 10m   26 Oct 13:40–14:40 Europe/Moscow
             finland   poland   sweden
  13:40:00      31.2     24.0     40.1
  13:50:00      30.8   1012.5     39.7
```

- `--op` takes any of the [ops](../metrics/aggregates.md#ops), such as `max`,
  `increase` or `p95`.
- `--every 10m` makes a bucket every ten minutes. Without it, the range is
  one bucket.
- `--by target` makes a group, a column, per value of the label.
- A terminal gets a table. When the columns don't fit, each bucket gets a line
  of its own. `--json` gives one object per bucket.
- A page holds 1000 buckets unless `--limit` says, and always ends between
  two whole buckets.

## Export for Prometheus

```sh
tinystore metrics ./data export
tinystore metrics ./data export --only-prefix shop_ --since 5m -o shop.prom
```

```text
# HELP shop_orders_total Orders placed.
# TYPE shop_orders_total counter
shop_orders_total{shop="msk"} 1027 1795737044534
```

- `export` writes the newest sample of every series in the store, in
  Prometheus' text format. It writes one family per name and one line per
  series. Each line has the newest value and that sample's time in
  milliseconds. [To Prometheus](../metrics/exposition.md) explains what each
  kind of metric becomes.
- `--only-prefix shop_` writes only the names that start with `shop_`.
  `--since 5m` leaves out a series if its newest sample is older than that.
  Without `--since`, the command writes every series that the store still
  keeps, even one that has had no samples for a long time.
- `-o shop.prom` writes to that file instead of stdout. The command first
  writes `shop.prom.part` next to it and renames it when it is complete, so
  a collector never reads half a file. It reports what it wrote on stderr.
- `--no-timestamps` writes each line without its sample's time.
- The text format allows only letters, digits, `_` and `:` in names, and
  the same without `:` in labels, so `export` writes any other character as
  `_`. If a series then
  has the same spelling as another one, `export` leaves it out, and stderr
  says how many series it left out.
- A running program's instruments write their totals every 15 seconds, so
  the last few seconds may not be in the text yet.

node_exporter's textfile collector and the Pushgateway refuse a sample that
has its own time. So a cron line for them leaves the times out with
`--no-timestamps`, and uses `-o` so that the file appears only when it is
complete:

```sh
* * * * * tinystore metrics /srv/shop/data export --no-timestamps -o /var/lib/node_exporter/shop.prom
```

If Prometheus scrapes a program's own handler instead, it keeps the times,
as [To Prometheus](../metrics/exposition.md#give-the-metrics-to-a-prometheus)
shows.

## Write a scrape or a file

```sh
curl -s localhost:9100/metrics | tinystore metrics ./data ingest --label instance=web-1
tinystore metrics ./data ingest --keep 30d < scrape.txt
```

```text
wrote 1834 samples to ./data
```

- `ingest` reads stdin to its end and writes every series of the text, or
  none. A line it cannot read fails the whole text, and the error names the
  line. It reads Prometheus' text and OpenMetrics', whose last line is
  `# EOF`.
- A sample that the text gives no time gets the time the text was read. A
  write whose answer was lost is sent again with the same times, so it
  [changes nothing](../metrics/ingesting.md#an-ingest-sent-again) the second
  time.
- If stdin is still open after 5 seconds, `ingest` says that a stream that
  does not end needs `-f`.
- A name's kind comes from its `# TYPE` line, as
  [From Prometheus](../metrics/ingesting.md#from-prometheus) lists.
- `--label instance=web-1` adds a label to every series. Give it again for
  more labels.
- `--keep 30d` sets how long each name of the text keeps its samples, as the
  SDKs' `keep` does.

A script can print the text itself. Each line is a name, its labels, a value
and, if you have one, a time in milliseconds:

```sh
printf 'ping_rtt_ms{target="poland"} 23.1 1793014320000\n' | tinystore metrics ./data ingest
```

## Write a stream that does not end

`-f` follows a stream, as `tail -f` follows a file: a ping, a loop that
scrapes, any script that prints a line whenever it has a number. It writes
what came every second, or every 10,000 lines, until stdin ends.

```sh
ping -i 1 poland.example.com \
  | awk -F'time=' '/time=/ { split($2, ms, " "); print "ping_rtt_ms{target=\"poland\"}", ms[1]; fflush() }' \
  | tinystore metrics ./data ingest -f --keep 30d
```

```text
tinystore metrics ingest: started a server for ./data; it stops 30 s after the last client
tinystore metrics ingest: ping_rtt_ms came with no # TYPE, so it is a gauge; --kind ping_rtt_ms=counter says otherwise
```

- A line gets the time it was read, unless it gives its own.
- A `# TYPE`, `# HELP` or `# UNIT` line holds for the lines after it. A loop
  that scrapes the same exporter repeats them, and that is fine.
- `--kind ping_lost_total=counter` gives a name its type when the stream has
  no `# TYPE` line for it. A name with neither is a gauge, and `-f` says so
  once.
- A line it cannot read, or one the store refuses, is skipped, and stderr
  names its number. The rest is written. When stdin ends, `-f` prints how
  many samples it wrote and exits 1 if it skipped any line.
- `-f` reads Prometheus' text only. OpenMetrics' times are seconds and its
  text ends with `# EOF`, so it has to be read whole, without `-f`.
- Restarting the same stream is safe only when its lines carry their own
  times. A line without one gets a new time when it is read again.

## Correct a gauge's range

```sh
printf '2026-11-01T00:00Z 1000\n2026-11-02T00:00Z 900\n' | tinystore metrics ./data replace revenue shop=msk --from 2026-11-01T00:00Z --to 2026-11-03T00:00Z
```

```text
replaced revenue{shop="msk"} from 2026-11-01T00:00:00Z to 2026-11-03T00:00:00Z with 2 samples
```

`replace` reads one sample per line on stdin: its time, as `--from` takes
one, then its value. When stdin ends, the range from `--from` up to `--to`,
now unless given, holds exactly those samples, as
[Correcting a gauge after the fact](../metrics/ingesting.md#correcting-a-gauge-after-the-fact)
explains. Only a gauge can be replaced. A line that can't be read fails the
command before anything is written.

## Delete a series

```sh
tinystore metrics ./data delete ping_rtt_ms target=poland
```

```text
deleted ping_rtt_ms{target="poland"}
```

`delete` removes the one series whose labels are exactly the ones given, as
[Removing a series](../metrics/reading.md#removing-a-series) says. A name
alone names its series without labels, so `delete ping_rtt_ms` removes
nothing from a metric whose series all have a `target`.

## Limits and defaults

| What                           | Value                                                |
|--------------------------------|------------------------------------------------------|
| a name's keep, when none given | 30 days                                              |
| the largest text               | 32 MiB or 100,000 samples, sent in pieces past 2 MiB |
| a batch of `-f`                | a second of lines, or 10,000 lines                   |

## See also

- [Ingesting](../metrics/ingesting.md): the same writes from a program, and what
  the engine keeps.
- [status and logs](status-and-logs.md): see which server holds a directory.
