# TinyStore

**A small storage runtime for applications.** SQL, key-value state, durable
jobs, files, search, logs and metrics in one directory, with one lifecycle,
one memory budget and one backup. SQLite is underneath, and a Rust program
compiles all of it in: the program is the store.

> **Version 0.1, the first release.** Until `1.0`, a minor release may still
> change the API, and its notes say what changed.

```sh
cargo add tinystore
cargo add serde --features derive
```

TinyStore builds SQLite itself, with flags of its own. Put them in your
project's `.cargo/config.toml`, since a dependency cannot set them for you:

```toml
[env]
LIBSQLITE3_FLAGS = "SQLITE_DQS=0 -USQLITE_ENABLE_MEMORY_MANAGEMENT SQLITE_ENABLE_DBPAGE_VTAB -DHAVE_FDATASYNC=1"
```

Without them readers wait on one another, point reads from 16 threads 7-9x
slower (research rust-slice-2026-10-10), which the store warns of once, in the
first lines of its logger when nothing listened before; a synced commit on
Linux takes 1.5x as long, since SQLite calls `fsync` for `fdatasync`; and a
node of a replicated store cannot apply its home's pages. The same flags serve
every platform.

```rust
use tinystore::{Options, Store};

let store = Store::open("data", Options::default())?;

let db = store.database("app").migrations("migrations").open()?;
db.exec(tinystore::sql!("insert into users (id, name) values (?, ?)", 42, "Ada"))?;

let sessions = store.bucket::<Session>("sessions").idle(Duration::from_hours(24 * 30)).open()?;
sessions.set(digest(&token), &Session { user_id: 42 })?;

let emails = store.queue::<Email>("emails").open()?;
emails.add(&Email { to: "ada@example.com".into() })?;
```

Each engine is a cargo feature, all of them on by default: `kv`, `config`,
`locks`, `channels`, `jobs`, `sql`, `blobs`, `records`, `metrics`, `system`,
`search`, `backup`, `replication` and `wire`, with `replication-tls` and
`console` beside them. A program that turns the defaults off links only the
engines it names.

- [Your first store](https://tinyshed.org/tinystore/docs/handbook/start/first-store): a first program, step by step
- [The guides](https://tinyshed.org/tinystore/docs): every engine, page by page, in Rust, TypeScript, Python and Go
- [The `tinystore` command](https://crates.io/crates/tinystore-cli): a store's directory served to other processes, and read while it runs

Licensed under Apache-2.0.
