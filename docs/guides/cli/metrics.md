# metrics

`tinystore metrics ingest` writes metrics from a shell: the Prometheus text
that an exporter serves, or that a script prints, read from stdin. It writes
through the server that holds the store's directory, or starts one, so it
works while a program uses the store, and needs no SDK.

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
  line.
- A sample that the text gives no time gets the time the text was read.
- A name's kind comes from the text's `# TYPE` line. A name with no
  `# TYPE` is a gauge.
- `--label instance=web-1` adds a label to every series. Give it again for
  more labels.
- `--keep 30d` sets how long each name of the text keeps its samples, as the
  SDKs' `keep` does.

A script can print the text itself. Each line is a name, its labels, a value
and, if you have one, a time in milliseconds:

```sh
printf 'ping_rtt_ms{target="poland"} 23.1 1793014320000\n' | tinystore metrics ./data ingest
```

## Who writes

| Who holds `./data`                    | What `ingest` does                                                          |
|---------------------------------------|-----------------------------------------------------------------------------|
| `tinystore serve` or an SDK's sidecar | writes through it                                                           |
| nothing                               | starts a sidecar, which stops 30 seconds after its last client, and says so |
| a program that opened it `embedded`   | refuses and exits 1                                                         |

```text
tinystore metrics ingest: started a server for ./data; it stops 30 s after the last client
```

Several commands can write at once: the first one starts the sidecar, and the
others write through it. A program that opened the store `embedded` keeps it
inside its own process, so no other process can write to it:

```text
tinystore metrics ingest: a program holds ./data inside its own process, so no other process can write to it
  open it in that program with a sidecar, or let `tinystore serve ./data` hold it
```

A directory without a store is refused, and nothing is created in it. Run the
program or `tinystore serve` there once to make the store.

## Limits and defaults

| What                           | Value                                       |
|--------------------------------|---------------------------------------------|
| a name's keep, when none given | 30 days                                     |
| the largest text               | 2 MiB, the largest message the server takes |
| a sidecar's idle time          | 30 seconds after its last client            |
| the wait for a server          | 15 seconds to start, 10 for an answer       |

## See also

- [Ingesting](../metrics/ingesting.md): the same writes from a program, and what
  the engine keeps.
- [status and logs](status-and-logs.md): see which server holds a directory.
