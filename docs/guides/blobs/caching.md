# Caching

A set of files can be a cache of derived files, such as converted PDFs or
thumbnails: `idle` counts a file's term from its last read, and `maxSize`
bounds what the set holds together, removing the files used least recently
when a write would go past it. The Blobs engine keeps the bound in the set
itself, so every program that opens the set follows the same one.

```ts
const cache = await store.database('cache', { backup: false })
const pages = cache.files('pages', { idle: '30d', maxSize: '512MiB' })

await pages.put(`${hash}.pdf`, pdf, { contentType: 'application/pdf' })
const page = await pages.get(`${hash}.pdf`) // renews its 30 days and moves it up the order
```

```python
cache = await store.database("cache", backup=False)
pages = cache.files("pages", idle="30d", max_size="512MiB")

await pages.put(f"{hash}.pdf", pdf, content_type="application/pdf")
page = await pages.get(f"{hash}.pdf")  # renews its 30 days and moves it up the order
```

```go
cache, err := sqldb.Open(ctx, store, "cache", sqldb.Backup(false))
pages, err := blobs.OpenFiles(cache, "pages", blobs.Idle(30*24*time.Hour), blobs.MaxSize(512<<20))

_, err = pages.Put(ctx, hash+".pdf", bytes.NewReader(pdf), blobs.ContentType("application/pdf"))
page, found, err := pages.Get(ctx, hash+".pdf") // renews its 30 days and moves it up the order
```

```rust
let cache = store.database("cache").backup(false).open()?;
let pages = cache.files("pages").idle(Duration::from_hours(24 * 30)).max_size(512 << 20).open()?;

pages.key(&format!("{hash}.pdf")).content_type("application/pdf").put(&pdf)?;
let page = pages.get(&format!("{hash}.pdf"))?; // renews its 30 days and moves it up the order
```

A database opened with `backup: false` keeps its files out of every backup,
which suits files the program can make again. A key names what made the
file: after a new converter, write `${hash}.v2.pdf`, or the cache serves the
old pages until their term ends.

## A term from the last use

`idle` works as it does for a bucket ([Expiry](../kv/expiry.md)):

- `get` and `head` renew a file's term. `head` counts because a conditional
  request answered `304` used the file. `list`, `usage` and the scrub renew
  nothing, so a page that lists the cache does not keep it alive.
- A renewal is written once a thirtieth of the term has passed since the
  last one, about once a day for 30 days, together with other writes. A read
  that finds less than a minute left renews before it answers.
- `ttl` and `idle` exclude each other on one set; a write's own `ttl` gives
  that file a fixed term.
- A connection with a `read` token cannot open a set with `idle`: the open
  fails `permission`. Its reads renew nothing.

## A ceiling that evicts

A write that takes the set past `maxSize` removes files in its own commit
until the set fits, so a write never fails because the set is full:

```ts
const thumbs = store.files('thumbs', { maxSize: '1GiB' })

await thumbs.put(`${id}.webp`, webp) // may remove the thumbnails used least recently
```

```python
thumbs = store.files("thumbs", max_size="1GiB")

await thumbs.put(f"{id}.webp", webp)  # may remove the thumbnails used least recently
```

```go
thumbs, err := blobs.OpenFiles(store, "thumbs", blobs.MaxSize(1<<30))

_, err = thumbs.Put(ctx, id+".webp", bytes.NewReader(webp)) // may remove the thumbnails used least recently
```

```rust
let thumbs = store.files("thumbs").max_size(1 << 30).open()?;

thumbs.put(&format!("{id}.webp"), &webp)?; // may remove the thumbnails used least recently
```

Files written and never read since go first, then the files read least
recently, to within an hour: a file's read gives it a day's head start over
files only written. So a burst of misses, each written once and never asked
for again, does not push out the pages people read every day. The head start
is a day and not for good: a file read a year ago goes before one written
this morning.

- A file larger than `maxSize` cannot fit: its write fails `limit`, naming
  the ceiling. `maxFileSize` still bounds one file below it.
- The size counted is what `usage` counts, the files' listed sizes. A copy
  shares its bytes with its source and still counts twice, and removing one
  copy frees no disk until the other goes too.
- One write removes at most 10,000 files. A write that 10,000 removals
  cannot fit fails `limit` and removes nothing. That happens only when a set
  holds a great many small files over a ceiling just lowered; the store's
  maintenance then removes the rest a batch at a time.
- An upload counts at its commit, not while its pieces arrive.
- A use is written at most once an hour, or once a thirtieth of `idle`.
  Reads through a `read` token, the command line and the MCP tools record no
  use, so looking at a cache from outside does not change what it keeps.

## A quota that refuses

`whenFull: 'refuse'` turns the ceiling into a quota for files that must not
go, such as a user's uploads:

```ts
const uploads = db.files('uploads', { maxSize: '500MiB', whenFull: 'refuse' })

await uploads.put(path, data) // past 500 MiB: LimitError, and nothing is removed
```

```python
uploads = db.files("uploads", max_size="500MiB", when_full="refuse")

await uploads.put(path, data)  # past 500 MiB: LimitError, and nothing is removed
```

```go
uploads, err := blobs.OpenFiles(db, "uploads", blobs.MaxSize(500<<20), blobs.WhenFull(blobs.Refuse))

_, err = uploads.Put(ctx, path, r.Body) // past 500 MiB: tinystore.ErrLimit, and nothing is removed
```

```rust
let uploads = db.files("uploads").max_size(500 << 20).when_full(WhenFull::Refuse).open()?;

uploads.put(&path, &data)?; // past 500 MiB: ErrorKind::Limit, and nothing is removed
```

A replace counts the difference, so a file smaller than the one it replaces
always lands; a copy counts its size; each write of a transaction counts on
its own. Expiry and deletes free room as usual. A ceiling lowered below the
set's size refuses what grows the set and takes what shrinks it.

## The ceiling is the set's

The set keeps `maxSize` and `whenFull` in its file, so one program with
512 MiB and another with none never share a set without a bound:

- The first open with `maxSize` writes it, and from then on it holds for
  every handle, in this process and every other.
- An open without `maxSize` takes the one the set keeps.
- An open with another value fails `conflict`, naming both. Opening a handle
  never changes what data already stored lives by, so a rolling deploy with
  old and new values does not move the ceiling with every open.

A ceiling changes by one call, which checks what it replaces:

```ts
await pages.setMaxSize('1GiB', { ifCurrent: '512MiB' }) // 'none' removes it
```

```python
await pages.set_max_size("1GiB", if_current="512MiB")  # "none" removes it
```

```go
err = pages.SetMaxSize(ctx, 1<<30, blobs.IfCurrent(512<<20)) // blobs.NoMaxSize removes it
```

```rust
pages.set_max_size(1 << 30).if_current(512 << 20).apply()?; // remove_max_size() removes it
```

A ceiling the set keeps already is a success that writes nothing, so every
instance of a deploy may call it at its start, opening the set without
`maxSize`. A set that keeps another than `ifCurrent` fails `conflict`, so two
deploys never undo each other. A lower ceiling removes files at once, a batch
at a time, unless the set refuses. `whenFull` can be given beside it; left
out, the set keeps what it does. A `read` token cannot change a ceiling, nor
open a set with one it does not keep: both fail `permission`.

| What                  | Where it lives           | Default                    |
|-----------------------|--------------------------|----------------------------|
| `ttl`, `idle`         | the handle that opens it | none: files never expire   |
| `maxFileSize`         | the handle that opens it | none                       |
| `maxSize`, `whenFull` | the set, in its file     | none; `'evict'` once given |

## Free room now

`trim` removes files in the same order until the set holds at most `to`,
and leaves the ceiling where it was:

```ts
const { evictions, freed } = await pages.trim({ to: '300MiB' }) // { evictions: 184, freed: 220_000_000 }
```

```python
trimmed = await pages.trim(to="300MiB")  # Trim(evictions=184, freed=220000000)
```

```go
trimmed, err := pages.Trim(ctx, 300<<20) // blobs.Trim{Evictions, Freed}
```

```rust
let trimmed = pages.trim(300 << 20)?; // Trim { evictions, freed }
```

It runs in batches of 10,000 files, each its own commit, so a trim of a
million files never holds the writer long; `freed` is the files' listed
sizes. A set without a ceiling trims too. A set whose ceiling refuses fails
`invalid`: those files are a user's. From a shell, `tinystore files ./data
trim pages --to 300MiB` does the same ([Files](../cli/files.md)).

## Seeing it work

```ts
const usage = await pages.usage() // { count: 12418, size: 522_000_000, maxSize: 536_870_912, evictions: 37 }
```

```python
usage = await pages.usage()  # Usage(count=12418, size=522000000, max_size=536870912, evictions=37)
```

```go
usage, err := pages.Usage(ctx) // blobs.Usage{Count: 12418, Size: 522000000, MaxSize: 536870912, Evictions: 37}
```

```rust
let usage = pages.usage()?; // Usage { count: 12418, size: 522000000, max_size: Some(536870912), evictions: 37 }
```

`evictions` counts the files the ceiling removed since the store opened, in
this process; a `trim` counts in its own answer. A full set with few
evictions has a ceiling that fits; a count that climbs fast says it is low.
The store's own metrics have `tinystore_files_evicted_total` and
`tinystore_files_bytes`, by `set` ([Self-metrics](../metrics/self-metrics.md)),
and `tinystore status --names` prints each set's files, size and ceiling:

```text
    pages   12418 files  498 MiB / 512 MiB
```

`tinystore verify ./data` checks that each set's count and size, which its
writes keep, match its files.

## Limits and defaults

| What                              | Value                                   |
|-----------------------------------|-----------------------------------------|
| files one write evicts at most    | 10,000                                  |
| files one trim's batch removes    | 10,000, each batch its own commit       |
| a use written at most             | once an hour, or a thirtieth of `idle`  |
| an idle file renewed before reads | with less than a minute left            |
| a file read ranks ahead by        | a day                                   |
| `maxSize`, `whenFull`             | none; `'evict'` once a ceiling is given |

## See also

- [Copies and expiry](copies-and-expiry.md): `ttl`, `expire`, and copies that
  share their bytes.
- [Files in a database](../sql/buckets-and-queues.md#files-in-a-database):
  `db.files` and a database left out of backups.
- [Expiry](../kv/expiry.md): `idle` as a bucket has it.
