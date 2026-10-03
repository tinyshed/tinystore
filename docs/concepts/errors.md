# Errors

Every TinyStore error has one of a few kinds, and the kinds are the same in
every engine and every language. An error about one item, such as a key, a
job or a series, also names that item. This page lists the kinds and what to
do about each.

## Check the kind

```ts
import { ConflictError, LimitError } from 'tinystore'

try {
	await drafts.set(id, draft, { ifVersion })
} catch (err) {
	if (err instanceof ConflictError) { /* reload and try again */ }
	else if (err instanceof LimitError) { /* ask for less */ }
	else throw err
}
```

```python
from tinystore import ConflictError, LimitError

try:
    await drafts.set(note_id, draft, if_version=version)
except ConflictError:
    ...  # reload and try again
except LimitError:
    ...  # ask for less
```

```go
err := drafts.Set(ctx, id, draft, kv.IfVersion(version))
switch {
case errors.Is(err, tinystore.ErrConflict):
	// reload and try again
case errors.Is(err, tinystore.ErrLimit):
	// ask for less
}
```

In Go, every engine wraps the root package's errors, so `errors.Is` works the
same way everywhere. In Bun and Python, every error is a subclass of
`TinystoreError`.

## Kinds

| Kind | Go | Bun and Python | What to do |
|---|---|---|---|
| invalid | `ErrInvalid` | `InvalidError` | fix the request: the value, key, name or SQL is wrong |
| limit | `ErrLimit` | `LimitError` | ask for less, or raise the limit; the error says which |
| conflict | `ErrConflict` | `ConflictError` | read again, then decide whether to retry |
| closed | `ErrClosed` | `ClosedError` | the store or handle was closed; open it again |
| in use | `ErrInUse` | `InUseError` | another process holds the directory, or the name is taken |
| corrupt | `ErrCorrupt` | `CorruptError` | stored data failed its checksum; restore from a backup |
| too old, too new | `ErrTooOld`, `ErrTooNew` | `TooOldError`, `TooNewError` | the time is outside the engine's [window](time.md) |
| suspended | `ErrSuspended` | `SuspendedError` | a metrics series failed maintenance; repair or drop it |
| outcome unknown | each engine's `ErrOutcomeUnknown` | `OutcomeUnknownError` | read what you wrote before you retry |
| permission | | `PermissionDeniedError` | the token's capability doesn't allow the call |
| unimplemented | | `UnimplementedError` | the server is older than your SDK; upgrade the server |
| unavailable | | `UnavailableError` | the server is shutting down; try again on a new connection |

## Errors that name their item

| Error | Names |
|---|---|
| `*kv.KeyError` | the bucket and the key |
| `*jobs.JobError` | the queue and the key |
| `*blobs.KeyError` | the bucket and the full path |
| `*metrics.SeriesError` | the series' name and labels |
| `*records.RecordError` | the record that was rejected |
| `*sqldb.ConstraintError` | the kind of constraint, its table and name |
| `*tinystore.LimitError` | the limit, what the call wanted and the bound |

In Go, use `errors.As` to read them. In Bun and Python, the error's `what`
field carries the same names. A rejected `ingest` or `append` names the one
series or record that was refused, so you can send the call again without it.

## Damaged data stays contained

A file that fails its checksum fails only its own engine's `open`. Inside an
engine, damage is contained too:

- **Records**: a damaged block is reported once and skipped by sealing. A read
  over it fails and names it, and `drop` removes it, but only once it is shown
  not to read.
- **Metrics**: a series that fails maintenance is suspended and logged, and
  the other series keep working. `drop` removes it.
- **Blobs**: the scrub marks a changed file, and its next read fails with a
  corrupt error. Writing new bytes to the key works as usual.

## See also

- [Memory and limits](memory.md): limit errors in detail.
- [Durability](durability.md): when the outcome is unknown.
