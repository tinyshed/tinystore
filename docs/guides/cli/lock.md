# lock

`tinystore lock hold` runs a command while it holds a key of a lock, so that
the command never runs twice at once: a cron job on two machines that share a
store's server, or a deploy step two shells may start together.

```sh
tinystore lock ./data hold cron nightly-backup -- ./backup.sh
```

## Run a command under a lock

```sh
tinystore lock ./data hold cron nightly-backup -- ./backup.sh
tinystore lock ./data hold cron nightly-backup --wait 0 -- ./backup.sh
```

- The words are the lock, the key, and after `--` the command with its own
  arguments. `--` keeps the command's flags from being read as `lock`'s.
- `--wait 30s` stands in line for the key that long, ten seconds when not
  given, as the SDKs' `hold` does. `--wait 0` takes the key only if it is free.
  A key held by another past the wait exits 1, and the command never runs:

```text
tinystore lock hold: lock cron: key nightly-backup is held by another; --wait stands in line longer
```

- The command gets the lease's fence in `TINYSTORE_FENCE`: a number larger
  than every earlier holder's, which a write can compare to refuse a holder
  that stalled. See [Fences](../locks/fences.md).
- `lock hold` ends with the command's exit code, once it has let the key go.

## The lease

The key is held through the server that holds the directory, or a sidecar
`lock hold` starts, with a lease that lasts `--ttl`, 30 seconds when not
given. `lock hold` renews it every third of `--ttl` while the command runs.

- A lease that is lost, revoked at `--max-hold` or not renewed in time
  because the server went away, stops the command, since another may hold the
  key now, and exits 1. On Linux and macOS the command gets `SIGTERM`, then
  `SIGKILL` five seconds later; a script that starts other programs should
  `exec` the last one, or trap `TERM` and stop them. On Windows it is ended at
  once.

```text
tinystore lock hold: the lease was lost, so the command was stopped: another may hold the key now
```

- `--max-hold 1h` lets the key go an hour after it was given, however it was
  renewed.
- If `lock hold` itself is killed, the key is free once the lease runs out,
  `--ttl` after its last renewal.
- Locks live in the memory of the process that holds the store, so every
  holder must reach the same server (NOT-03).

## See also

- [Locks](../locks/README.md): locks and semaphores from a program, waiting,
  leases and fences.
