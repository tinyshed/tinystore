# Getting started

In this guide you install TinyStore, open a store, and write a short program
that uses three of its engines. The program creates a one-time sign-in code
that expires automatically, sends an email in the background, and writes a log
line that you can read after the program exits.

## Install

```sh for=bun
bun add @tinyshed/tinystore
```

```sh for=python
pip install tinyshed-tinystore
```

```sh for=go
go get github.com/tinyshed/tinystore
```

The Bun and Python packages include the `tinystore` binary for your
platform, which runs the store for them. TinyStore needs Bun 1.4+, Node.js
22+, Python 3.12+ or Go 1.27+.

## Open a store

```ts
import { open } from '@tinyshed/tinystore'

await using store = await open('./data')
```

```python
import asyncio

import tinystore


async def main() -> None:
    async with tinystore.open("./data") as store:
        ...


asyncio.run(main())
```

```go
store, err := tinystore.Open(ctx, "./data", tinystore.Options{})
if err != nil {
	return err
}
defer store.Close(ctx)
```

TinyStore creates `./data` if it doesn't exist.

- In Go, the store runs inside your process. If another process tries to
  open the same directory, it gets an `ErrInUse` error, so two processes can
  never corrupt the data.
- In Bun and Python, `open` connects to the directory's sidecar, or starts one
  if none is running. All processes that open the same directory share it.

When you close the store, it writes out any log lines that are still waiting
in memory. The rest of the code on this page goes inside this block.

## Create a one-time code

A sign-in link contains a one-time code. Store the code in a KV bucket that
deletes its keys after 15 minutes. Then read it with `take`, which returns the
value and deletes the key in one step:

```ts
const codes = store.kv.bucket('sign-in-codes', 'int', { defaultTtl: '15m' })

await codes.set('K7Q2', 42)           // the code for user 42
console.log(await codes.take('K7Q2')) // 42, and the code is gone
console.log(await codes.take('K7Q2')) // undefined: a second click finds nothing
```

```python
codes = store.kv.bucket("sign-in-codes", int, default_ttl="15m")

await codes.set("K7Q2", 42)          # the code for user 42
print(await codes.take("K7Q2"))      # 42, and the code is gone
print(await codes.take("K7Q2"))      # None: a second click finds nothing
```

```go
// errors left out
state, err := kv.Open(ctx, store, kv.Options{})
codes, err := kv.OpenBucket[int64](ctx, state, "sign-in-codes", kv.DefaultTTL(15*time.Minute))

err = codes.Set(ctx, "K7Q2", 42)               // the code for user 42
userID, found, err := codes.Take(ctx, "K7Q2") // 42 true, and the code is gone
_, found, err = codes.Take(ctx, "K7Q2")       // false: a second click finds nothing
```

If a user clicks the link twice at the same moment, only one `take` gets the
code, so only one sign-in succeeds. Unused codes are deleted after 15 minutes.
You don't need an `expires_at` column or a cleanup job.

Each bucket stores values of one type, which you choose when you open it.
Here the values are integers: `'int'` in Bun, `int` in Python and `int64` in
Go.

## Send an email in the background

Sending an email shouldn't slow down the request. Put it in a queue instead:

```ts
const emails = store.jobs.queue<{ to: string; code: string }>('emails')
await emails.enqueue({ to: 'ada@example.com', code: 'K7Q2' })

await emails.work(
	async job => {
		console.log(`sending ${job.value.code} to ${job.value.to}`)
	},
	{ untilIdle: true },
)
```

```python
async def send(job: tinystore.Job[dict[str, str]]) -> None:
    print(f"sending {job.value['code']} to {job.value['to']}")


emails = store.jobs.queue("emails", dict[str, str])
await emails.enqueue({"to": "ada@example.com", "code": "K7Q2"})
await emails.work(send, until_idle=True)
```

```go
type Email struct {
	To, Code string
}

queues, err := jobs.Open(ctx, store, jobs.Options{})
emails, err := jobs.OpenQueue[Email](ctx, queues, "emails")
err = emails.Enqueue(ctx, Email{To: "ada@example.com", Code: "K7Q2"})

err = emails.Work(ctx, func(ctx context.Context, job jobs.Job[Email]) error {
	fmt.Printf("sending %s to %s\n", job.Value.Code, job.Value.To)
	return nil
}, jobs.UntilIdle())
```

`enqueue` returns after the job is saved to disk, so the job survives even if
the process crashes right after. When the handler finishes, the job is done.
When the handler throws or returns an error, TinyStore retries the job later,
with a longer delay before each new attempt.

With `untilIdle`, `work` returns as soon as no jobs are due, which is useful in
scripts and tests. A server runs `work` for as long as the server runs.

## Write a log line

```ts
const log = store.records.logger('app')
log.info('code sent', { userId: 42 })
```

```python
logging.basicConfig(handlers=[store.records.handler("app")], level=logging.INFO)
logging.info("code sent", extra={"user_id": 42})
```

```go
logs, err := records.Open(ctx, store, records.Options{})
logger := slog.New(logs.Handler("app"))
logger.Info("code sent", "userId", 42)
```

The logger prints each line to stderr as soon as you log it, and also saves
it in the store. In a terminal, lines are formatted for people:

```log
09:08:47.058 INFO  app  code sent  userId=42
```

Everywhere else, each line is a JSON object, which is the format that log
collectors in containers expect. Logging never waits for the disk.

## Look inside the store

Run the program, then use the `tinystore` command to see what the directory
contains. The Bun and Python packages install the command. In Go, install it
with `go install`:

```sh for=bun
bunx @tinyshed/tinystore status ./data
```

```sh for=python
tinystore status ./data
```

```sh for=go
go install github.com/tinyshed/tinystore/cmd/tinystore@latest
tinystore status ./data
```

```text
○ ./data  served by none

  jobs         60.0 KiB
  kv           36.0 KiB
  records      24.0 KiB
  ──────────────────────
  total       120.0 KiB
```

Each engine you used has its own file, and engines you didn't use don't
create any files. Run `tinystore logs ./data` to print your application's
logs, including the line from the program. Add `-f` to follow new lines while
the program runs.

## The complete program

```ts title="app.ts"
import { open } from '@tinyshed/tinystore'

await using store = await open('./data')

const codes = store.kv.bucket('sign-in-codes', 'int', { defaultTtl: '15m' })
await codes.set('K7Q2', 42)
console.log(await codes.take('K7Q2')) // 42
console.log(await codes.take('K7Q2')) // undefined

const emails = store.jobs.queue<{ to: string; code: string }>('emails')
await emails.enqueue({ to: 'ada@example.com', code: 'K7Q2' })
await emails.work(
	async job => {
		console.log(`sending ${job.value.code} to ${job.value.to}`)
	},
	{ untilIdle: true },
)

const log = store.records.logger('app')
log.info('code sent', { userId: 42 })
```

```python title="app.py"
import asyncio
import logging

import tinystore


async def send(job: tinystore.Job[dict[str, str]]) -> None:
    print(f"sending {job.value['code']} to {job.value['to']}")


async def main() -> None:
    async with tinystore.open("./data") as store:
        codes = store.kv.bucket("sign-in-codes", int, default_ttl="15m")
        await codes.set("K7Q2", 42)
        print(await codes.take("K7Q2"))  # 42
        print(await codes.take("K7Q2"))  # None

        emails = store.jobs.queue("emails", dict[str, str])
        await emails.enqueue({"to": "ada@example.com", "code": "K7Q2"})
        await emails.work(send, until_idle=True)

        logging.basicConfig(handlers=[store.records.handler("app")], level=logging.INFO)
        logging.info("code sent", extra={"user_id": 42})


asyncio.run(main())
```

```go title="main.go"
package main

import (
	"context"
	"fmt"
	"log"
	"log/slog"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/jobs"
	"github.com/tinyshed/tinystore/kv"
	"github.com/tinyshed/tinystore/records"
)

type Email struct {
	To, Code string
}

func main() {
	if err := run(context.Background()); err != nil {
		log.Fatal(err)
	}
}

func run(ctx context.Context) error {
	store, err := tinystore.Open(ctx, "./data", tinystore.Options{})
	if err != nil {
		return err
	}
	defer store.Close(ctx)

	state, err := kv.Open(ctx, store, kv.Options{})
	if err != nil {
		return err
	}
	codes, err := kv.OpenBucket[int64](ctx, state, "sign-in-codes", kv.DefaultTTL(15*time.Minute))
	if err != nil {
		return err
	}
	if err := codes.Set(ctx, "K7Q2", 42); err != nil {
		return err
	}
	userID, found, err := codes.Take(ctx, "K7Q2")
	if err != nil {
		return err
	}
	fmt.Println(userID, found) // 42 true
	_, found, err = codes.Take(ctx, "K7Q2")
	if err != nil {
		return err
	}
	fmt.Println(found) // false

	queues, err := jobs.Open(ctx, store, jobs.Options{})
	if err != nil {
		return err
	}
	emails, err := jobs.OpenQueue[Email](ctx, queues, "emails")
	if err != nil {
		return err
	}
	if err := emails.Enqueue(ctx, Email{To: "ada@example.com", Code: "K7Q2"}); err != nil {
		return err
	}
	err = emails.Work(ctx, func(ctx context.Context, job jobs.Job[Email]) error {
		fmt.Printf("sending %s to %s\n", job.Value.Code, job.Value.To)
		return nil
	}, jobs.UntilIdle())
	if err != nil {
		return err
	}

	logs, err := records.Open(ctx, store, records.Options{})
	if err != nil {
		return err
	}
	logger := slog.New(logs.Handler("app"))
	logger.Info("code sent", "userId", userID)
	return nil
}
```

When you run it, the program prints:

```text
42
undefined
sending K7Q2 to ada@example.com
```

Python prints `None` instead of `undefined`, and Go prints `42 true` and
`false`. The log line goes to stderr.

## Next steps

Pick an engine that your application needs. Each engine's section starts with
an overview, followed by one page per feature, for example
[Quotas](kv/quotas.md).
