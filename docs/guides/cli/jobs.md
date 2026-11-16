# jobs

`tinystore jobs` adds, lists, counts and cancels a store's jobs from a
shell, with no SDK. `add` and `cancel` write through a server, as
[every write](README.md#who-writes) of the command line does. `list` and
`counts` read a queue beside the program or server that holds the directory.
No worker runs in the command: a job runs in the program whose worker takes
its queue.

```sh
tinystore jobs ./data add emails '{"to": "ada@example.com"}' --id welcome-42 --delay 1m
tinystore jobs ./data list emails --state failed
tinystore jobs ./data counts emails
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
  it there, and `add` says so on stderr. A program cancels it by the job its
  list gave, as [Cancel a job without an id](../jobs/ids.md#cancel-a-job-without-an-id)
  shows.
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

- It lists the jobs that have ids, in the order of their ids, then those
  without, `-` in the id's column, in the order they run, then those without
  that failed. A second word narrows them to the ids that start with it.
- `--state` lists one state alone: `scheduled`, `waiting`, `running`, `done`
  or `failed`.
- A page holds 100 jobs unless `--limit` says, 1000 at most, and `--after`
  reads the next, as [Pages](README.md#pages) says.
- `--json` gives a line per job, its time whole and UTC.

## Count a queue's jobs

```sh
tinystore jobs ./data counts emails
```

```text
queue emails: 120 scheduled, 8 waiting, 2 running, 3 failed
```

- Every state is read from one snapshot. `scheduled` is due later, `waiting`
  is due and no handler has it yet, `running` is in a handler's hands, and
  `failed` waits for the queue's `keep`. A job done has left the queue and is
  not counted.
- `--json` gives one line, `{"scheduled": 120, "waiting": 8, "running": 2,
  "failed": 3}`, and `--database app` counts a queue a program opened from a
  database.

## Cancel a job

```sh
tinystore jobs ./data cancel emails welcome-42
```

```text
cancelled welcome-42 in queue emails
```

A job that is gone, or never was, says `queue emails holds no job welcome-42`
and exits 0.

## See also

- [jobs](../jobs/README.md): queues, workers, retries and schedules from a
  program.
