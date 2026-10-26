# jobs

`tinystore jobs` adds, lists and cancels a store's jobs from a shell, with
no SDK. `add` and `cancel` write through the server that holds the directory,
or start one. `list` reads a queue beside the program or server that holds the
directory. No worker runs in the command: a job runs in the program whose
worker takes its queue.

```sh
tinystore jobs ./data add emails '{"to": "ada@example.com"}' --id welcome-42 --delay 1m
tinystore jobs ./data list emails --state failed
tinystore jobs ./data cancel emails welcome-42
```

## Add a job

```sh
tinystore jobs ./data add emails '{"to": "ada@example.com"}' --id welcome-42 --delay 1m
```

```text
added welcome-42 to queue emails, due in 1m
```

- A job is JSON. `-` reads it from stdin, which Windows PowerShell 5.1 needs,
  since it drops the quotes inside an argument:

```sh
echo '{"to": "ada@example.com"}' | tinystore jobs ./data add emails -
```

- `--id welcome-42` names the job. A queue that holds a job with that id adds
  nothing, so the same command sent again is harmless:

```text
queue emails holds a job welcome-42 already; nothing added
```

- A job without `--id` can't be cancelled from a shell, since nothing names
  it, and `add` says so on stderr.
- `--delay 1m` runs it a minute from now, and `--at 14:30` at a time. Without
  either it runs as soon as a worker takes it.
- How many times it is tried, and for how long, is the worker's: the queue's
  options in the program that runs it.
- `--database app` adds to a queue the program opened from a database,
  `db.queue("reminders")`, kept in that database's file.

## List a queue

```sh
tinystore jobs ./data list emails --state failed
tinystore jobs ./data list emails welcome-
```

```text
welcome-42  failed     26 Oct 14:32:07  attempt 10  smtp: connection refused
welcome-43  scheduled  26 Oct 14:40:00  attempt 0
```

- It lists the jobs that have ids, in the order of their ids. A second word
  narrows them to the ids that start with it.
- `--state` lists one state alone: `scheduled`, `waiting`, `running`, `done`
  or `failed`.
- `--limit` sets how many a page holds, 100 when not given. When there are
  more, stderr says what `--after` reads on.
- `--json` gives a line per job, its time whole and UTC, and a last line
  `{"next": "…"}` when there are more.

## Cancel a job

```sh
tinystore jobs ./data cancel emails welcome-42
```

```text
cancelled welcome-42 in queue emails
```

A job that is gone, or never was, says `queue emails holds no job welcome-42`
and exits 0.

## Who writes

`add` and `cancel` write as every write of the command line does: through
`tinystore serve` or an SDK's sidecar when one holds the directory, or a
sidecar they start, which stops 30 seconds after its last client. A directory
that a program opened `embedded` is refused, since no other process can write
to it. See [metrics: who writes](metrics.md#who-writes).

## See also

- [jobs](../jobs/README.md): queues, workers, retries and schedules from a
  program.
