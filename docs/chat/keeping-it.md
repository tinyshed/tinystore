# Keeping it

The chat's whole state is one directory, so keeping it safe is one job: back
it up every night while it runs, restore it on another machine when you need
to, and test the parts that depend on time without waiting for time to pass.
This chapter uses the store's backup, a schedule from the Jobs engine, and a
store that runs on a clock your test moves.

## Back up while it runs

```ts
const backup = await store.backup('backups/chat-2026-10-19.zip')
// { files: 7, bytes: 1834221, keyLeftOut: false }
```

```python
backup = await store.backup("backups/chat-2026-10-19.zip")
# Backup(files=7, bytes=1834221, key_left_out=False)
```

```go
backup, err := store.Backup(ctx, "backups/chat-2026-10-19.zip")
// tinystore.Backup{Files: 7, Bytes: 1834221, KeyLeftOut: false}
```

```rust
let backup = store.backup().write("backups/chat-2026-10-19.zip")?;
// Backup { files: 7, bytes: 1834221, key_left_out: false }
```

```sh
tinystore backup ./data backups/chat-2026-10-19.zip
```

The backup is one zip of every engine's files, with a `manifest.json` that
lists each file's size and SHA-256. The chat keeps answering while it is
made: each engine copies its files as one moment of itself, without stopping
its writer or its readers. The zip appears under its name only once it is
whole, so a backup that fails leaves no half zip behind. The folder it goes
into must exist.

Any tool that opens a zip opens it. It is not encrypted: what the store keeps
in the clear is in the clear in the zip, so keep backups as safely as the
store's directory.

## Every night

A schedule is a repeating job that the code owns. The chat backs up at three
in the morning, Berlin time:

```ts
store.schedule('backup', { cron: '0 3 * * *', timeZone: 'Europe/Berlin' }, async run => {
	const day = run.at.toISOString().slice(0, 10)
	await store.backup(`backups/chat-${day}.zip`)
})
```

```python
async def nightly_backup(run: tinystore.Run) -> None:
    await store.backup(f"backups/chat-{run.at.date()}.zip")


store.schedule("backup", nightly_backup, cron="0 3 * * *", time_zone="Europe/Berlin")
```

```go
nightly, err := jobs.OpenSchedule(store, "backup", jobs.Cron("0 3 * * *", "Europe/Berlin"))
err = nightly.Work(ctx, func(ctx context.Context, run *jobs.Run) error {
	_, err := store.Backup(ctx, fmt.Sprintf("backups/chat-%s.zip", run.At.Format(time.DateOnly)))
	return err
})
```

```rust
let backups = store.clone();
let nightly = store.schedule("backup").cron("0 3 * * *", "Europe/Berlin").work(move |run| -> Result<(), Box<dyn Error>> {
    let day = jiff::Timestamp::try_from(run.at())?.strftime("%Y-%m-%d"); // the jiff crate
    backups.backup().write(format!("backups/chat-{day}.zip"))?;
    Ok(())
})?;
```

`run.at` is the time the run was due, not the time it started, so a backup
that starts late is still named after its night. The schedule is a job like
any other: if the chat is down at three, it runs once when the chat comes
back, not once for every night it missed. A run that fails is retried with a
backoff, and the schedule goes on.

A cron needs a time zone. "Three in the morning" means nothing without one,
and a default of UTC would be a guess.

## Restore

```sh
tinystore restore backups/chat-2026-10-19.zip ./data
```

```rust
tinystore::backup::restore("backups/chat-2026-10-19.zip", "data")?; // before Store::open
```

A restore writes into an empty or missing directory, never over a store. It
checks every file against the manifest's size and SHA-256 before it names
it, so it either restores everything or leaves the directory empty. Then the
chat opens the directory as it opens any store.

A program in TypeScript, Python or Go restores through the `tinystore`
command, because a restore happens before any store is open.

> [!WARNING]
> **The encryption key is not in a backup**
> If the chat keeps encrypted buckets, the store's encryption key is left out
> of the zip unless you ask for it, and the backup says `keyLeftOut: true`.
> Without the key, the restored store cannot read those values. Keep the key
> apart, or back it up on purpose with `includeEncryptionKey`
> (`include_encryption_key`, `tinystore.IncludeEncryptionKey()`).

## Test on a clock you move

Sign-in codes expire after 15 minutes, a reminder runs in an hour, sessions
end after 30 idle days. A test should not wait for any of that. A private
store can run on a clock that the test moves:

```ts
const store = await open(dir, { private: true, clock: new Date('2026-10-09T09:00:00Z') })
const codes = store.bucket<number>('sign-in-codes', { ttl: '15m' })
const reminders = store.queue<Reminder>('reminders')

await codes.set('k7q2', 42)
await reminders.add({ to: 'ada@example.com' }, { delay: '1h' })
await reminders.runDue(remind) // 0: nothing is due yet

await store.clock.advance('1h')
await reminders.runDue(remind) // 1: the reminder ran
await codes.get('k7q2') // undefined: the code expired
```

```python
store = await tinystore.open(dir, private=True, clock=datetime(2026, 10, 9, 9, tzinfo=UTC))
codes = store.bucket("sign-in-codes", int, ttl="15m")
reminders = store.queue("reminders", Reminder)

await codes.set("k7q2", 42)
await reminders.add(Reminder(to="ada@example.com"), delay="1h")
await reminders.run_due(remind)  # 0: nothing is due yet

await store.clock.advance("1h")
await reminders.run_due(remind)  # 1: the reminder ran
await codes.get("k7q2")  # None: the code expired
```

```go
store, err := tinystore.Open(ctx, dir, tinystore.Private(), tinystore.Clock(time.Date(2026, 10, 9, 9, 0, 0, 0, time.UTC)))
codes, err := kv.OpenBucket[int64](store, "sign-in-codes", kv.TTL(15*time.Minute))
reminders, err := jobs.OpenQueue[Reminder](store, "reminders")

err = codes.Set(ctx, "k7q2", 42)
_, err = reminders.Add(ctx, Reminder{To: "ada@example.com"}, jobs.Delay(time.Hour))
err = reminders.RunDue(ctx, remind) // nothing is due yet

err = store.Clock().Advance(ctx, time.Hour)
err = reminders.RunDue(ctx, remind)    // the reminder ran
_, found, err := codes.Get(ctx, "k7q2") // found is false: the code expired
```

```rust
let clock = Arc::new(TestClock::new(start));
let store = Store::open(&dir, Options { clock: Some(clock.clone()), background: false, ..Options::default() })?;
let codes = store.bucket::<i64>("sign-in-codes").ttl(Duration::from_mins(15)).open()?;
let reminders = store.queue::<Reminder>("reminders").open()?;

codes.set("k7q2", &42)?;
reminders.delay(Duration::from_hours(1)).add(&Reminder { to: "ada@example.com".into() })?;
reminders.run_due(remind)?; // 0: nothing is due yet

clock.advance(Duration::from_hours(1));
reminders.run_due(remind)?; // 1: the reminder ran
codes.get("k7q2")?; // None: the code expired
```

Every engine reads the time through the store's clock: expiry, jobs, a
quota's windows, the time of a log line. Moving the clock moves all of them
together, and it never moves backwards. `runDue` runs what is due and
returns, so a test checks the result right after.

A private store is a server for the test alone, in a temporary directory,
so tests run in parallel without sharing a sidecar.

## Why this design?

**Why not copy the directory.** A file that SQLite is writing has a
write-ahead log beside it, and a copy taken mid-write can hold half a commit.
A backup instead asks each engine for one moment of its files: SQLite's
`vacuum into` for a database, a link for each stored file, so a large
attachment costs the backup a directory entry and no copy until the zip is
written. Engines that hold numbers in memory, such as counters and the
logger's buffer, write them first, so the backup has them.

**Why each engine is its own moment.** The store has no moment shared by all
its files, because no write spans two files. Each engine's copy is
consistent with itself, which is all a write could ever rely on. When two
things must be in one moment, such as a message and its reply job, they live
in one file, and that file is copied as one moment.

**Why a restore checks every byte.** A backup is the copy you use on your
worst day. A restore that finds a damaged file and stops halfway would leave
you with a store that opens and is wrong. TinyStore checks every file before
it puts it in place, and leaves an empty directory if anything does not
match, so a restore is either the backup or nothing.

**Why a clock instead of sleeping.** A test that waits 15 minutes for a code
to expire is never run, and one that waits 50 milliseconds for a job is
flaky. A clock the test moves makes time a value the test controls: the same
test passes the same way on a laptop and on a slow CI machine.

## See also

- [One directory, many engines](../start/directory.md): what is in the
  directory a backup copies.
- [Replies in the background](replies.md): schedules are queues' jobs.
- [Backup API reference](../reference/bun/backup.md): every option of a
  backup.
