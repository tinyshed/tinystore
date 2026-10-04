# Memory and limits

TinyStore bounds the memory it uses. Every call has limits, such as the size of
a value or the samples a query may decode, and a store can also have one
memory budget that all engines share. When a call would go past a limit, it
fails with an error that says which limit it hit, instead of using more memory
or returning less than you asked for.

## One budget for the whole store

```go
store, err := tinystore.Open(ctx, dir, tinystore.Options{Memory: 256 << 20}) // 256 MiB
```

```sh
tinystore serve /srv/data --listen tls://0.0.0.0:7443 --memory 268435456 …
```

With a budget, every engine reserves memory before it does its work: a query
before it decodes, a write before it encodes its value, an upload for its
buffer. If the budget is used up, the call waits for memory in the order the
calls arrived, so a large query isn't passed over by many small ones. A call
that waits can still be cancelled. A call that needs more than the whole
budget fails at once.

A remote server uses a budget of 1 GiB unless you set `--memory`. A local
sidecar has no budget by default, because its clients are your own programs.

The budget covers the work of the engines. It is not a limit on the process's
whole memory: the Go runtime, SQLite's caches and the values your program
keeps are outside it.

## Memory while idle

Each engine's file keeps one writer connection and one reader connection open.
A burst of reads opens more readers, up to eight for an SQL database or KV,
and each one closes after a minute without use, until one is left. In Go,
`tinystore.Options{Readers: 2}` sets the most readers that any engine of the
store opens, and `sqldb.Readers(n)` sets one database's.

## Set the budget from the container

```go
store, err := tinystore.Open(ctx, dir, tinystore.Options{Memory: tinystore.FromCgroup(0.5)}) // half the container's limit
```

`FromCgroup` reads the memory limit of the container the program runs in, from
cgroup v2 or v1, and returns that fraction of it. It returns 0, which means no
budget, outside Linux and in a container without a limit. This is Go only,
because Bun and Python reach the store through a server, which takes
`--memory`.

## Run your own work under the budget

```go
reservation, err := store.Reserve(ctx, 19<<20) // an Argon2 hash holds 19 MiB
if err != nil {
	return err
}
defer reservation.Release()
hash := argon2.IDKey(password, salt, 2, 19*1024, 1, 32)
```

Your program can reserve memory from the store's budget for heavy work of its
own, such as hashing a password. The work then waits while the engines use the
budget, and the engines wait while it runs, so the whole process stays within
one bound. `ReserveNow` takes the memory only if it is free right away.

## Limits of every call

Without a budget, each engine still limits how much one call can take, and
how many calls run at once:

| Engine  | Examples of limits                                            |
|---------|---------------------------------------------------------------|
| KV      | a value 1 MiB; a `scan` page 1,000 keys or 4 MiB              |
| jobs    | a value 1 MiB; a progress report 4 KiB                        |
| blobs   | 16 KiB of memory per upload, whatever the file's size         |
| SQL     | `all` holds at most 64 MiB of rows                            |
| records | an `append` 4 MiB; a page 10,000 records                      |
| metrics | a query decodes at most 1,048,576 samples and returns 100,000 |

The [limits and defaults](../reference/limits.md) page lists every limit.

## A limit error says which limit

```ts
try {
	await store.metrics.read({ name: 'cpu', since: '30d' })
} catch (err) {
	if (err instanceof LimitError) {
		console.log(err.limit, err.wanted, err.bound) // "decoded samples", 4800000, 1048576
	}
}
```

```python
try:
    await store.metrics.read(name="cpu", since="30d")
except LimitError as err:
    print(err.limit, err.wanted, err.bound)  # decoded samples 4800000 1048576
```

```go
_, err := stats.Read(ctx, metrics.Range{Name: "cpu", Since: 30 * 24 * time.Hour})
if limit, ok := errors.AsType[*tinystore.LimitError](err); ok {
	fmt.Println(limit.Name, limit.Wanted, limit.Bound) // decoded samples 4800000 1048576
}
```

A limit error tells you which limit was reached, how much the call wanted and
what the limit is, so you know whether to raise the limit or ask for less. A
metrics query never returns fewer samples than you asked for without telling
you.

Compare the limit's name with a constant instead of its text:

```ts
import { LimitError, limits } from '@tinyshed/tinystore'

if (err instanceof LimitError && err.limit === limits.decodedSamples) { /* ask for a shorter range */ }
```

```python
if isinstance(err, tinystore.LimitError) and err.limit == tinystore.limits.DECODED_SAMPLES:
    ...  # ask for a shorter range
```

```go
if limit, ok := errors.AsType[*tinystore.LimitError](err); ok && limit.Name == metrics.LimitDecodedSamples {
	// ask for a shorter range
}
```

| Limit                        | Bun              | Python             | Go                            |
|------------------------------|------------------|--------------------|-------------------------------|
| the store's memory, one call | `storeMemory`    | `STORE_MEMORY`     | `tinystore.LimitMemory`       |
| the store's memory, now      | `storeMemoryNow` | `STORE_MEMORY_NOW` | `tinystore.LimitMemoryNow`    |
| series a query matches       | `matchedSeries`  | `MATCHED_SERIES`   | `metrics.LimitSeries`         |
| blocks a query decodes       | `decodedBlocks`  | `DECODED_BLOCKS`   | `metrics.LimitBlocks`         |
| bytes a query fetches        | `fetchedBytes`   | `FETCHED_BYTES`    | `metrics.LimitPayloadBytes`   |
| samples a query decodes      | `decodedSamples` | `DECODED_SAMPLES`  | `metrics.LimitDecodedSamples` |
| samples a query returns      | `outputSamples`  | `OUTPUT_SAMPLES`   | `metrics.LimitOutputSamples`  |
| buckets an aggregate returns | `outputBuckets`  | `OUTPUT_BUCKETS`   | `metrics.LimitOutputBuckets`  |
| a record's bytes             | `recordBytes`    | `RECORD_BYTES`     | `records.LimitRecordBytes`    |
| bytes of one `append`        | `appendBytes`    | `APPEND_BYTES`     | `records.LimitAppendBytes`    |
| a job's value                | `jobValueBytes`  | `JOB_VALUE_BYTES`  | `jobs.LimitValueBytes`        |
| a step's answer              | `stepBytes`      | `STEP_BYTES`       | `jobs.LimitStepBytes`         |
| an object past `maxSize`     | `objectBytes`    | `OBJECT_BYTES`     | `blobs.LimitObjectBytes`      |

## Why limits instead of best effort

A store that quietly uses as much memory as a query needs can take a whole
machine down with one large request, and a store that quietly returns less
gives you a wrong answer. TinyStore makes the cost visible: a call either does
all of its work within its limits, or fails and says why.

## See also

- [Limits and defaults](../reference/limits.md): every limit in one table.
- [Errors](errors.md): every kind of error and what to do about it.
