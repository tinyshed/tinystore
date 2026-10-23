# Store API for Rust

Every public type, trait and function of the store in `tinystore`, generated from its source. The [Store guide](../../handbook/start/directory.md) explains how to use them, and the [Bun and Node](../bun/store.md), [Python](../python/store.md) and [Go](../go/store.md) pages list the same API.

## CancellationToken

```rust
pub struct CancellationToken { /* private fields */ }
```

Stops the reads of a call from another thread, at once or after a
timeout: a read under way stops, gives its reader back and fails
`Cancelled`, and one that has not started fails before it takes one.

The token stops what [`run`](CancellationToken::run) runs, on its thread:

```rust
use tinystore::CancellationToken;

let token = CancellationToken::new();
let counting = std::thread::spawn({
    let (db, token) = (db.clone(), token.clone());
    move || token.run(|| db.scalar::<i64>("select count(*) from events"))
});
token.cancel(); // the visitor left: the count stops with `Cancelled`, its reader back

let timed = CancellationToken::timeout(Duration::from_secs(2));
let total: i64 = timed.run(|| db.scalar("select count(*) from events"))?;
```

Only reads stop: a write runs to its end, and so does a read inside a
transaction, which its own bound stops. Clones share the token.

### CancellationToken.new

```rust
pub fn new() -> Self
```

### CancellationToken.timeout

```rust
pub fn timeout(after: Duration) -> Self
```

A token that stops its reads once `after` has passed since it was
made, or earlier when it is cancelled.

A timeout is read as a statement runs and as a read waits; a statement
SQLite runs as one step, as a `count(*)` of a whole table, finishes that
step first, where `cancel` stops it at once.

### CancellationToken.cancel

```rust
pub fn cancel(&self)
```

Stops the reads under the token now and from now on.

### CancellationToken.is_cancelled

```rust
pub fn is_cancelled(&self) -> bool
```

Whether the token was cancelled or its timeout passed.

### CancellationToken.run

```rust
pub fn run<T>(&self, call: impl FnOnce() -> T) -> T
```

Runs `call` on this thread with the token over every read it makes:
once the token is cancelled, each read under way fails `Cancelled` and
each one after it fails before it starts. Tokens nest: a read stops
when any token around it does.

## Clock

```rust
pub trait Clock
```

Where a store reads the time. Every engine reads it through the store, so a
test that moves one clock moves them all.

### Clock.now

```rust
fn now(&self) -> SystemTime
```

## SystemClock

```rust
pub struct SystemClock
```

The system's clock.

## TestClock

```rust
pub struct TestClock { /* private fields */ }
```

A clock a test moves by hand. It never moves backwards: engines keep
watermarks and leases by it, and a step back would make them lie.

### TestClock.new

```rust
pub fn new(start: SystemTime) -> Self
```

### TestClock.advance

```rust
pub fn advance(&self, by: Duration)
```

### TestClock.set

```rust
pub fn set(&self, to: SystemTime) -> Result<()>
```

## unix_millis

```rust
pub fn unix_millis(time: SystemTime) -> i64
```

Milliseconds since the Unix epoch; negative before it.

## unix_nanos

```rust
pub fn unix_nanos(time: SystemTime) -> i64
```

Nanoseconds since the Unix epoch; negative before it.

## Constraint

```rust
pub struct Constraint {
    pub kind: ConstraintKind,
    pub table: Option<String>,
    pub columns: Vec<String>,
    pub name: Option<String>,
}
```

A constraint a write broke, as SQLite names it: its kind always, and the
table and the columns, or the constraint's own name, when SQLite says them.

## ConstraintKind

```rust
pub enum ConstraintKind {
    Unique,
    PrimaryKey,
    ForeignKey,
    Check,
    NotNull,
}
```

A unique or a primary key already held is `conflict`; any other is
`invalid`.

### ConstraintKind.as_str

```rust
pub fn as_str(self) -> &'static str
```

The kind as the wire and every SDK say it: `primaryKey`.

## Error

```rust
pub struct Error { /* private fields */ }
```

An error that names what failed: `kv bucket sessions: key "a": not found`.

### Error.new

```rust
pub fn new(kind: ErrorKind, what: impl Into<String>) -> Self
```

### Error.invalid

```rust
pub fn invalid(what: impl Into<String>) -> Self
```

### Error.not_found

```rust
pub fn not_found(what: impl Into<String>) -> Self
```

### Error.closed

```rust
pub fn closed(what: impl Into<String>) -> Self
```

### Error.limit

```rust
pub fn limit(what: impl Into<String>) -> Self
```

### Error.corrupt

```rust
pub fn corrupt(what: impl Into<String>) -> Self
```

### Error.internal

```rust
pub fn internal(what: impl Into<String>) -> Self
```

### Error.io

```rust
pub fn io(what: impl Into<String>, source: Error) -> Self
```

### Error.with_source

```rust
pub fn with_source(self, source: impl Into<Box<dyn StdError + Send + Sync>>) -> Self
```

### Error.within

```rust
pub fn within(self, outer: impl Display) -> Self
```

The same error, said of what contains it: `kv bucket sessions` around
`key "a"` reads `kv bucket sessions: key "a"`.

### Error.duplicate

```rust
pub fn duplicate(&self) -> Self
```

A copy for each of several callers one failure answers, its cause kept
as text.

### Error.fact

```rust
pub fn fact(&self, name: &str) -> Option<&str>
```

A fact the error carries by name: a limit's `limit`, `wanted` and
`bound`; a broken constraint's `constraint`, `table`, `columns` and
`name`, which `Error::constraint` reads as one.

### Error.constraint

```rust
pub fn constraint(&self) -> Option<Constraint>
```

The constraint a write broke, as SQLite names it:

```rust
match db.exec(sql!("insert into users (email) values (?)", "ann@example.com")) {
    Err(error) if error.constraint().is_some_and(|broken| broken.columns == ["email"]) => {
        println!("that email is taken")
    }
    done => drop(done?),
}
```

### Error.kind

```rust
pub fn kind(&self) -> ErrorKind
```

### Error.what

```rust
pub fn what(&self) -> &str
```

## ErrorKind

```rust
pub enum ErrorKind {
    Invalid,
    NotFound,
    Conflict,
    TooOld,
    TooNew,
    Limit,
    Closed,
    Corrupt,
    InUse,
    Suspended,
    Unavailable,
    Cancelled,
    OutcomeUnknown,
    CommittedLocally,
    Lost,
    Io,
    Internal,
}
```

What went wrong, with the same meaning in every engine, on the wire and in
every SDK.

### ErrorKind.as_str

```rust
pub fn as_str(self) -> &'static str
```

## Result

```rust
pub type Result<T, E> = Result<T, E>
```

## Key

```rust
pub trait Key
```

What names a key, an owner or a folder: text, or an integer spelled in
decimal, so that `42` and `"42"` are one key.

### Key.text

```rust
fn text(&self) -> Cow<'_, str>
```

## Memory

```rust
pub struct Memory { /* private fields */ }
```

### Memory.new

```rust
pub fn new(limit: u64) -> Arc<Self>
```

### Memory.limit

```rust
pub fn limit(&self) -> u64
```

### Memory.used

```rust
pub fn used(&self) -> u64
```

### Memory.bounded

```rust
pub fn bounded(&self) -> bool
```

Whether the store was given a budget; without one, work reserves
nothing and pays nothing for it.

### Memory.try_reserve

```rust
pub fn try_reserve(self: &Arc<Self>, bytes: u64, what: &str) -> Result<Reservation>
```

Takes `bytes` now, or fails with `Limit`: so does a call that would
take the room a waiting reservation waits for.

### Memory.nothing

```rust
pub fn nothing(self: &Arc<Self>) -> Reservation
```

A reservation of nothing yet, which grows as its work holds more.

### Memory.reserve

```rust
pub fn reserve(self: &Arc<Self>, bytes: u64, what: &str, patience: Duration) -> Result<Reservation>
```

Takes `bytes`, waiting up to `patience` for other work to give them
back; waiters are served in the order they came, and a call that
takes nothing waits for none of them.

## Reservation

```rust
pub struct Reservation { /* private fields */ }
```

Bytes taken from a store's memory, given back when dropped.

### Reservation.bytes

```rust
pub fn bytes(&self) -> u64
```

### Reservation.grow

```rust
pub fn grow(&mut self, bytes: u64, what: &str) -> Result<()>
```

Takes `bytes` more now, or fails with `Limit` and keeps what it held.

### Reservation.shrink

```rust
pub fn shrink(&mut self, to: u64)
```

Gives back what the work turned out not to need.

## Durability

```rust
pub enum Durability {
    Full,
    Os,
}
```

How far a commit goes before it returns: a setting of a store's files,
`kv.db`, `jobs.db` and each database's, which [`Options`](crate::Options)
gives for them all and a database may say for itself.

## Options

```rust
pub struct Options {
    pub memory: Option<u64>,
    pub clock: Option<Arc<dyn Clock>>,
    pub background: bool,
    pub durability: Option<Durability>,
    pub keep_free: u64,
    pub encryption_key_file: Option<PathBuf>,
    pub node: Option<Node>,
    pub self_metrics: Option<SelfMetrics>,
    pub max_series: u64,
}
```

How a store opens.

## Store

```rust
pub struct Store { /* private fields */ }
```

One directory of engine files, held by one store at a time through its
`LOCK`. Engines open against a store and close with it, the last opened
first: at `close`, or when the last clone of the store is dropped.

### Store.open

```rust
pub fn open(dir: impl AsRef<Path>, options: Options) -> Result<Store>
```

Opens the store in `dir`, creating the directory when it is missing.
Another store holding the directory, in this process or another, makes
it an `InUse` error.

### Store.open_beside

```rust
pub fn open_beside(dir: impl AsRef<Path>) -> Result<Store>
```

Opens the store in `dir` to read beside the process that holds it, a
server or a program: no lock taken, no background work, each file read
through readers alone, none written and none checkpointed. A write is
refused, `Invalid`, and a file the store does not hold is `NotFound`
rather than made. What the holder commits meanwhile, a read sees.

### Store.dir

```rust
pub fn dir(&self) -> &Path
```

### Store.is_beside

```rust
pub fn is_beside(&self) -> bool
```

Whether the store was opened beside its holder, to read only.

### Store.keep_free

```rust
pub fn keep_free(&self) -> u64
```

The bytes of the disk the store keeps free, as its options said.

### Store.max_series

```rust
pub fn max_series(&self) -> u64
```

The metrics series the store holds at most, as its options said.

### Store.now

```rust
pub fn now(&self) -> SystemTime
```

### Store.clock_moved

```rust
pub fn clock_moved(&self)
```

Tells every engine that a test moved the store's clock by hand, so
that what sleeps until a time reads it again: a worker waiting for a
job due in an hour runs it once the clock is an hour on.

### Store.memory

```rust
pub fn memory(&self) -> &Arc<Memory>
```

### Store.close

```rust
pub fn close(&self) -> Result<()>
```

Stops background work, waiting for the task under way, then every
handler the engines run, a queue's or a channel's, waiting for those
running; then closes every engine, the last opened first, and lets go
of the directory. The first engine that fails to close is the error;
the others still close. A second call does nothing, and neither does
the last clone's drop after.

### Store.tx

```rust
pub fn tx<T, E: From<Error>>(&self, work: impl FnOnce(&Tx<'_>) -> Result<T, E>) -> Result<T, E>
```

Runs `work` in one transaction of one of the store's own files, kv.db
or jobs.db: the file of the first handle `tx.with` takes in, whose
writer it then holds alone, so that what it reads cannot change before
it writes. Two orders cannot both take the last item, and a hundred
jobs are added together or not at all. It commits when `work` returns
`Ok` and rolls back when it returns an error or panics.

Inside, every call on the store's buckets and queues goes through the
transaction, `tx.with(&bucket)`: one made around it would not see what
it wrote, or would wait for the writer it holds, and fails `Invalid`
instead. A bucket and a queue of the store are two files, and no write
is atomic across two: a handle kept in another file than the first is
`Invalid`, and to commit keys and jobs together a program opens both
from a database. Writes that need no reads need no transaction, since
separate calls already share commits.

## Tx

```rust
pub struct Tx<'a> { /* private fields */ }
```

A transaction of the store, given to the function `Store::tx` runs.

### Tx.with

```rust
pub fn with<'h, H: TxHandle>(&self, handle: &'h H) -> <H as >::InTx
```

The calls of `handle` inside this transaction: what they read sees what
the transaction wrote, and what they write commits with it or not at
all. A handle kept in another file is refused at its first call.

## Transaction

```rust
pub struct Transaction<'t> { /* private fields */ }
```

A transaction as the handles it takes in see it: the writer it holds, the
file a handle must be kept in, the bound it ends within, and what runs
once it ends.

## TxHandle

```rust
pub trait TxHandle
```

A handle a transaction of its file takes in with `tx.with(&handle)`: a
bucket or counters, and a queue, from the store or from a database.

## sql

```rust
macro_rules! sql
```

A statement and the values of its `?`s, in order:
`sql!("select * from notes where author_id = ? and done = ?", user_id, false)`.

<!-- Generated by task reference from crates/tinystore through rustdoc. Edit the doc comments there, not this file. -->
