# Your first store

This chapter installs TinyStore and runs a first program: it keeps a value,
adds a background job, runs it and logs a line. Then it looks inside the
store with the `tinystore` command. It takes about ten minutes, in
TypeScript, Python, Go or Rust.

> [!NOTE]
> **Version 0.1**
> TinyStore's first release. Until `1.0`, a minor release may still change
> the API, and its notes say what changed.

## Install

```sh for=bun
bun add @tinyshed/tinystore
```

```sh for=python
pip install tinyshed-tinystore
```

```sh for=go
go get tinyshed.org/tinystore
go install tinyshed.org/tinystore/cmd/tinystore   # the tinystore command, of the same release
```

```sh for=rust
cargo add tinystore
cargo add serde --features derive
cargo add tracing tracing-subscriber
```

```toml title=".cargo/config.toml" for=rust
# the SQLite build TinyStore expects, which a dependency cannot set for you
[env]
LIBSQLITE3_FLAGS = "SQLITE_DQS=0 -USQLITE_ENABLE_MEMORY_MANAGEMENT SQLITE_ENABLE_DBPAGE_VTAB -DHAVE_FDATASYNC=1"
```

| Language     | Needs                     | The core comes as                                         |
|--------------|---------------------------|-----------------------------------------------------------|
| Bun and Node | Bun 1.4 or Node 22        | the `tinystore` program and libraries, in the npm package |
| Python       | Python 3.12               | the `tinystore` program and a module, in the wheel        |
| Go           | Go 1.25                   | the release's program and library, fetched at first open  |
| Rust         | the Rust toolchain itself | the crate, compiled into your program                     |

Rust programs compile the core in, so a Rust program is the store. It builds
SQLite too, with the flags in `.cargo/config.toml`: without them many readers
at once wait on one another, which the store's logger warns of in its first
lines, and a synced commit on Linux takes 1.5x as long. The other
languages talk to the same core in a separate process by default, as
[How the store runs](#how-the-store-runs) explains below.

## The whole program

```ts title="hello.ts"
import { open } from '@tinyshed/tinystore'

interface User {
	name: string
	visits: number
}

interface Email {
	to: string
	subject: string
}

await using store = await open('./data')

const users = store.bucket<User>('users')
await users.set('42', { name: 'Ada', visits: 1 })
console.log(await users.get('42')) // { name: 'Ada', visits: 1 }

const emails = store.queue<Email>('emails')
await emails.add({ to: 'ada@example.com', subject: 'Welcome' })

const log = store.logger('hello')
await emails.runDue(async email => {
	log.info('email sent', { to: email.to })
})
```

```python title="hello.py"
import asyncio
from dataclasses import dataclass

import tinystore


@dataclass
class User:
    name: str
    visits: int


@dataclass
class Email:
    to: str
    subject: str


async def main() -> None:
    async with await tinystore.open("data") as store:
        users = store.bucket("users", User)
        await users.set("42", User(name="Ada", visits=1))
        print(await users.get("42"))  # User(name='Ada', visits=1)

        emails = store.queue("emails", Email)
        await emails.add(Email(to="ada@example.com", subject="Welcome"))

        log = store.logger("hello")

        async def send(email: Email, run: tinystore.Run) -> None:
            log.info("email sent", to=email.to)

        await emails.run_due(send)


asyncio.run(main())
```

```go title="main.go"
package main

import (
	"context"
	"fmt"
	"log/slog"

	"tinyshed.org/tinystore"
	"tinyshed.org/tinystore/jobs"
	"tinyshed.org/tinystore/kv"
	"tinyshed.org/tinystore/records"
)

type User struct {
	Name   string `json:"name"`
	Visits int    `json:"visits"`
}

type Email struct {
	To      string `json:"to"`
	Subject string `json:"subject"`
}

func main() {
	ctx := context.Background()
	store, err := tinystore.Open(ctx, "data")
	if err != nil {
		panic(err)
	}
	defer store.Close()

	// errors left out from here on
	users, err := kv.OpenBucket[User](store, "users")
	err = users.Set(ctx, "42", User{Name: "Ada", Visits: 1})
	user, found, err := users.Get(ctx, "42")
	fmt.Println(user, found) // {Ada 1} true

	emails, err := jobs.OpenQueue[Email](store, "emails")
	_, err = emails.Add(ctx, Email{To: "ada@example.com", Subject: "Welcome"})

	log := slog.New(records.NewHandler(store, "hello"))
	err = emails.RunDue(ctx, func(ctx context.Context, email Email, run *jobs.Run) error {
		log.Info("email sent", "to", email.To)
		return nil
	})
}
```

```rust title="main.rs"
use serde::{Deserialize, Serialize};
use tinystore::{Options, Store};
use tracing_subscriber::prelude::*;

#[derive(Serialize, Deserialize, Debug)]
struct User {
    name: String,
    visits: u32,
}

#[derive(Serialize, Deserialize)]
struct Email {
    to: String,
    subject: String,
}

fn main() -> tinystore::Result<()> {
    let store = Store::open("data", Options::default())?;

    let users = store.bucket::<User>("users").open()?;
    users.set("42", &User { name: "Ada".into(), visits: 1 })?;
    println!("{:?}", users.get("42")?); // Some(User { name: "Ada", visits: 1 })

    let emails = store.queue::<Email>("emails").open()?;
    emails.add(&Email { to: "ada@example.com".into(), subject: "Welcome".into() })?;

    let log = store.logger("hello").layer()?;
    tracing_subscriber::registry().with(log).init();
    emails.run_due(|email: Email, _run| {
        tracing::info!(to = %email.to, "email sent");
        Ok::<_, std::convert::Infallible>(())
    })?;
    store.close()
}
```

Run it, and it prints the user and one log line:

```log
15:39:52.006 INFO  hello  email sent  to=ada@example.com
```

The rest of this chapter goes through the program a piece at a time.

## Open the store

`open` takes a directory and creates it if it is missing. Everything the
program keeps goes into that directory, and nothing goes anywhere else. When
the program ends, `await using` in TypeScript, `async with` in Python,
`defer store.Close()` in Go and `store.close()` in Rust close the store. A
store that is closed has written everything it was given.

One process holds a directory at a time. If a second program opens the same
directory, it does not get a second copy of the data: it joins the first
store, as [How the store runs](#how-the-store-runs) shows.

## Keep a value

A bucket is a named collection of values of one type, by key. `set` writes a
value and returns after it is on the disk. `get` reads it back, or returns
nothing (`undefined`, `None`, `found` false, `None`) when the key is not
there.

The type is the one you give the bucket: an interface in TypeScript, a
dataclass in Python, a struct in Go and Rust. The value is stored as JSON, so
a Python program reads what a Go program wrote. Buckets do much more: keys
that expire, versions for safe updates, branches such as one user's
sessions. Chapter 6, Sessions and limits, covers them.

## Do work later

A queue holds jobs: work to do now, after a delay, or at a given time. `add`
puts a job in the queue and returns after the job is on the disk. A worker
runs a handler on each job. If the handler returns, the job is done. If it
throws or returns an error, the job runs again after a pause.

This program uses `runDue / run_due / RunDue / run_due`, which runs the jobs that
are due and then returns. That suits a script or a test. A server calls
`work` instead, which keeps running jobs as they come due until the store
closes:

```ts
emails.work(async email => {
	await smtp.send(email)
})
```

```python
async def deliver(email: Email, run: tinystore.Run) -> None:
    await smtp.send(email)


emails.work(deliver)
```

```go
err = emails.Work(ctx, func(ctx context.Context, email Email, run *jobs.Run) error {
	return smtp.Send(ctx, email)
})
```

```rust
let worker = emails.work(|email: Email, _run| smtp.send(&email))?;
```

## What a crash does

A job is on the disk before `add` returns. So if the program dies right
after it, the job is still there when the program starts again, and a worker
runs it then:

```scene
code ts
  await emails.add({ to: 'ada@example.com', subject: 'Welcome' })

  emails.work(async email => {
    await smtp.send(email)
  })
end
actor app   "Your program" at 10,28 icon=terminal
actor again "Restarted"    at 10,78 icon=terminal
actor disk  "jobs.db"      at 50,50 icon=database
actor smtp  "Mail server"  at 90,50 icon=mail
link app disk
set disk "waiting: 0"

step "Your program adds a job. add returns only after the job is written to jobs.db and synced to the disk." line=1
  send app -> disk "add"
  set disk "waiting: 1"
  send disk -> app "saved" color=ok

step "The program crashes before any worker has run the job. Everything it held in memory is gone." line=1
  mark app down
  cut app disk

step "You start the program again. The job is still in jobs.db, because it was on the disk before add returned." line=3
  link again disk
  send again -> disk "work"
  send disk -> again "the job"

step "The worker sends the email and the job is done. Nothing was lost, and you wrote no code for it." line=4
  send again -> smtp "Welcome" color=ok
  set disk "waiting: 0"
```

A write that returned success survives the process dying (contract DUR-01).
The other side of this is that a job runs at least once, not exactly once.
If the program dies after the email was sent but before the job was marked
done, the email is sent again after the restart. Chapter 7, Replies in the
background, shows how a handler makes a second run harmless.

## Log what happened

`store.logger('hello')` is a logger whose lines go to two places. It prints
each line to the console, as any logger does. It also keeps the line in the
store, in a stream named `hello`, so that you can read your logs back later
by time, level, fields or text.

Go uses the standard `slog` package with a handler from TinyStore, and Rust
uses `tracing` with a layer from TinyStore, so the rest of your program logs
as it always did. A log call never waits for the disk. The logger keeps its
lines in memory and writes them to the store within a second.

## Look inside

The `tinystore` command reads a store while your program runs. It never
changes anything and never stops the program:

```sh
tinystore status ./data
```

```text
● ./data  served by tinystore 0.1.1 · pid 15865 · up 4s · durability not set: full, os for records and metrics

  kv        76 KiB
  jobs     125 KiB
  records   42 KiB
  ────────────────
  total    243 KiB
```

```sh
tinystore kv ./data list users    # 42  {"name":"Ada","visits":1}
tinystore logs ./data             # 15:39:52.006 INFO  hello  email sent  to=ada@example.com
tinystore logs ./data -f          # every new line as it arrives, until Ctrl+C
```

`tinystore` alone lists every command. Chapter 28, Running it, covers them
all, along with `tinystore mcp`, which lets an AI agent read the store.

## How the store runs

The directory now contains a file for each engine the program used:

```text
data/
├── LOCK               held by the process that serves the store
├── kv.db              the bucket
├── jobs.db            the queue
├── records.db         the log line
└── server/SERVE       how other programs find the sidecar
```

The engines open lazily: `kv.db` appears when the first bucket opens, and a
program that never logs has no `records.db`. Next to each file you may see
`-wal` and `-shm` files. Those are SQLite's write-ahead log, and they belong
to the file beside them.

A Rust program runs the store inside its own process. A Bun, Node, Python or
Go program does not, by default. Its SDK starts a small `tinystore` process
next to it, called the sidecar, and talks to it over a local socket or a
named pipe on Windows. The sidecar writes its address to `server/SERVE`, so a
second program that opens the same directory finds it and joins it. When the
last program has disconnected, the sidecar waits 30 seconds and exits.

## Why this design?

**Why a sidecar by default.** A directory can have only one owner at a time,
because two processes writing the same SQLite file through separate caches
would wait on each other's locks, and their background work would collide.
But a web application often runs several processes: four workers behind a
load balancer, and a cron script beside them. The sidecar lets all of them
share one store with one owner. If your program is the only one that ever
opens the directory, open it with `embedded: true` (`embedded=True`,
`tinystore.Embedded()`), and the core runs inside your process with no
sidecar at all.

**Why a write waits for the disk.** A program that was told "saved" must
find the value after a crash or a power loss, so `set` and `add` return only
after the disk has synced. A sync takes time, so TinyStore groups writes:
while one sync runs, the writes that arrive wait together and share the next
one. A hundred concurrent requests pay for a few syncs, not a hundred.
Chapter 15, The journey of a write, follows one write all the way down.

**Why logs do not wait.** A log line is worth less than the request that
writes it, so the logger never makes a request wait for the disk. Lines wait
in a bounded buffer and are written every second. If the buffer is full, a
line is dropped and counted, never blocking the program. Logs and metrics
also default to a lighter durability than the other engines: they survive the
program crashing, but a power loss may take the last second of them.

## See also

- [One directory, many engines](directory.md): every file in the directory,
  and every way to reach it.
- [Why TinyStore](why.md): what TinyStore replaces, and what it is not.
