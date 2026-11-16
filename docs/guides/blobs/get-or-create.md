# Get or create

`getOrCreate` returns the file at a path, and on a miss it runs your
function, writes what it returns and returns that. If many requests miss the
same path at once, the function runs once and every request gets the same
file. It is part of the Blobs engine, and it works on any set of files: a
cache of converted pages, thumbnails, or a report that many readers ask for.

```ts
const pages = store.files('pages')

const page = await pages.getOrCreate(`${hash}.pdf`, async () => ({ data: await convert(doc), contentType: 'application/pdf' }))
return new Response(page.stream(), { headers: { etag: page.etag } })
```

```python
from tinystore import Fill

pages = store.files("pages")

page = await pages.get_or_create(f"{hash}.pdf", lambda: Fill(convert(doc), content_type="application/pdf"))
```

```go
pages, err := blobs.OpenFiles(store, "pages")

page, err := pages.GetOrCreate(ctx, hash+".pdf", func(ctx context.Context) (blobs.Fill, error) {
	pdf, err := convert(ctx, doc)
	return blobs.Fill{Data: pdf, ContentType: "application/pdf"}, err
})
```

```rust
let pages = store.files("pages").open()?;

let page = pages.key(&format!("{hash}.pdf")).content_type("application/pdf").get_or_create(|| convert(&doc))?;
```

The answer is the same file that `get` returns, so ranges, ETags and serving
work as they do for any stored file.

## What `getOrCreate` does

| When `getOrCreate(path, fn)` is called | It                                                                        |
|----------------------------------------|---------------------------------------------------------------------------|
| a file is at the path                  | returns it, and runs nothing                                              |
| another call is running `fn`           | waits for it, in this process or through the server, and returns its file |
| the path is free                       | runs `fn`, writes what it returns, and returns that file                  |
| `fn` throws                            | throws, and every call that waited fails too; the next call runs `fn`     |

Ten requests that miss the same page in the same second start one
conversion. The other nine wait and then read the file it wrote (contract
BLOB-21).

Put a version in the key when the function changes, such as
`${hash}.v2.pdf` after a new converter. Otherwise the set keeps serving the
old files until they expire or you delete them.

## What the function returns

The function returns what `put` takes: bytes, a stream, or the path of a file
on the store's machine. A stream or a file is written a piece at a time, so a
500 MiB conversion never sits in memory whole.

```ts
await pages.getOrCreate(`${hash}.pdf`, async () => ({ path: await renderToFile(doc) }), { contentType: 'application/pdf' })
```

```python
await pages.get_or_create(f"{hash}.pdf", lambda: Fill(path=render_to_file(doc)), content_type="application/pdf")
```

```go
_, err = pages.GetOrCreate(ctx, hash+".pdf", func(ctx context.Context) (blobs.Fill, error) {
	file, err := renderToFile(ctx, doc)
	return blobs.Fill{Path: file}, err
}, blobs.ContentType("application/pdf"))
```

```rust
use tinystore::blobs::Fill;

pages.key(&format!("{hash}.pdf")).content_type("application/pdf").get_or_create(|| Ok::<_, tinystore::Error>(Fill::file(render_to_file(&doc)?)))?;
```

The content type and the meta of the call apply unless the function's result
gives its own. A result that the set cannot store, such as one past
`maxFileSize`, fails with a limit error for the caller and for every call that
waited, and nothing is written.

## When something else writes the path

While the function runs, the call holds a claim on the path. The file lands
only if, when it is written, the claim still belongs to this run and the path
is still empty (contract BLOB-22). So the file at the path is always the
result of the run that held the path when it was written.

| What happens while `fn` runs                 | Result                                                                    |
|----------------------------------------------|---------------------------------------------------------------------------|
| a `put` of the path                          | the `put` wins, and every waiting call gets the `put`'s file              |
| a `delete` of the path                       | the run's result is dropped, and one of the waiting calls runs `fn` again |
| `fn` throws                                  | every waiting call fails with that error                                  |
| a waiting call times out or is cancelled     | it stops waiting, and the run goes on                                     |
| the run's claim ran out, and another took it | the old run's result is dropped, and its callers wait for the new run     |

A delete means that what was being made is stale, so `getOrCreate` does not
promise that `fn` runs exactly once. It promises that `fn` runs once while
its claim holds, and that the file at the path comes from the run that held
the claim.

When `fn` throws, the caller gets its own error. A call that waited gets the
store's error as it is when `fn` threw one, and otherwise a conflict
(`ConflictError` in Bun and Python, `ErrConflict` in Go,
`ErrorKind::Conflict` in Rust) whose message contains the original one.
The next call runs `fn` again, so one bad document costs one conversion and
not one per waiting request.

## How long a run may take

A claim lasts 30 seconds, and the store renews it every 10 seconds while the
function runs, for up to 10 minutes. Pass `timeout` for a function that takes
longer.

```ts
await pages.getOrCreate(`${id}.mp4`, () => transcode(id), { timeout: '30m' })
```

```python
await pages.get_or_create(f"{id}.mp4", lambda: transcode(id), timeout="30m")
```

```go
_, err = pages.GetOrCreate(ctx, id+".mp4", transcode, blobs.Timeout(30*time.Minute))
```

```rust
pages.key(&format!("{id}.mp4")).timeout(Duration::from_secs(30 * 60)).get_or_create(|| transcode(&id))?;
```

After `timeout`, the claim runs out within 30 seconds, and the next call
runs its own function. The first function goes on, because the store cannot
stop your code, but its result is dropped. A function that runs forever
therefore never holds its path for good.

A claim belongs to the process that runs the function:

- In your program's own process, the store renews it. If the process dies,
  the next open of the store forgets the claim.
- Through a server, the claim belongs to the connection. If the connection
  closes, the claim goes at once and the next call runs the function. The SDK
  asks again on its next connection, so your function may run again.

## Where it runs

- Inside a database's transaction, `getOrCreate` fails with an invalid error,
  because the function would hold the transaction's writer.
- On a node that copies `blobs.db` from its home, it fails with an invalid
  error. Call it on the home.
- A call of the same path inside its own function fails with an invalid
  error, because it would wait for itself.

With the `async` feature, `aio`'s `get_or_create` takes an async function and
waits on no thread. If you drop its future while it waits, it stops waiting.
If you drop it while the function runs, the claim is let go and nothing is
written. See [Rust with async](../../handbook/start/rust-async.md).

## Seeing it work

The store's own metrics count every call in
`tinystore_files_get_or_create_total{set, outcome}`, where `outcome` is
`hit` (the file was there), `run` (this call ran the function), `wait` (it
waited for another call's run) or `failed`. `hit` plus `wait`, divided by
`run`, is what the cache saves. See
[the store's own metrics](../metrics/self-metrics.md).

## Limits and defaults

| What      | Value                        |
|-----------|------------------------------|
| a claim   | 30 seconds, renewed every 10 |
| `timeout` | 10 minutes unless given      |

## See also

- [Blobs](README.md): the engine's overview.
- [Once](../kv/once.md): a function run once per key whose answer is a value,
  not a file.
- [Serving files](serving.md): ranges, ETags and conditional requests.
