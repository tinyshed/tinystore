# Mirrors

A mirror keeps a table of the SQL engine as a copy of another system's rows,
for example a course's works or a shop's orders, and tells you what changed.
You give it the list that the source returned. The mirror compares the list
with the table's rows by the source's key, writes only the difference in one
commit, and returns what was added, changed and gone.

```ts
const works = db.mirror<Work>('works', { key: ['course_id', 'id'], gone: 'gone_at', changeOn: ['grade'] })

const diff = await works.replace({ course_id: 1 }, fetched)
for (const { before, after } of diff.changed) notify(after, before.grade, after.grade)
// diff: { added: Work[], changed: { before, after }[], gone: Work[], same: 41, suspect: false }
```

```python
works = db.mirror("works", Work, key=["course_id", "id"], gone="gone_at", change_on=["grade"])

diff = await works.replace({"course_id": 1}, fetched)
for changed in diff.changed:
    notify(changed.after, changed.before.grade, changed.after.grade)
```

```go
works := sqldb.OpenMirror[Work](db, "works", sqldb.MirrorOptions{
	Key: []string{"course_id", "id"}, Gone: "gone_at", ChangeOn: []string{"grade"},
})

diff, err := works.Replace(ctx, map[string]any{"course_id": 1}, fetched)
for _, changed := range diff.Changed {
	notify(changed.After, changed.Before.Grade, changed.After.Grade)
}
```

```rust
let works = db.mirror::<Work>("works").key(["course_id", "id"]).gone("gone_at").change_on(["grade"]).open()?;

let diff = works.replace(&[("course_id", 1)], &fetched)?;
for changed in &diff.changed {
    notify(&changed.after, changed.before.grade, changed.after.grade);
}
```

You create the table with a migration. `key` lists the columns that identify
a row at the source. A unique index or the primary key must cover them,
otherwise the mirror fails with `invalid`. Rows use your program's type, the
same type that the table's inserts accept.

## What replace answers

`replace(scope, rows)` makes the scope match the list. The scope is the set
of rows whose columns have the scope's values, and `{}` is the whole table.
Every given row must contain the scope's columns with the same values. For
example, a row of another course in course 1's list is `invalid`, and the
error names its key. A key that appears twice in the list is `invalid` too.

| The row's key     | In the table                  | What is written                               | Reported in |
|-------------------|-------------------------------|-----------------------------------------------|-------------|
| only in the list  |                               | an insert                                     | `added`     |
| in both           | its `changeOn` columns differ | the given columns that differ                 | `changed`   |
| in both           | the same                      | nothing                                       | `same`      |
| in both           | marked gone                   | the columns that differ, and the mark cleared | `added`     |
| only in the table | live                          | the mark set to now, or a delete              | `gone`      |
| only in the table | marked gone                   | nothing                                       |             |

Rows in the answer contain only the columns that the list's rows contain.
`added` rows and a changed row's `after` are as the list gave them. A changed
row's `before` and the `gone` rows are as the table had them. In TypeScript,
a mirror accepts `types` like `db.table` does,
`{ types: { done: 'bool', due: 'time', tags: 'json' } }`, and writes and
reads those columns the way a table's handle does. Python and Go read them by
the row's type. The whole call is one write. If any row fails, for example on
a constraint or a trigger, nothing is written (contracts SQL-17, SQL-18).

If the list gives a row exactly as the table has it, the mirror doesn't write
the row at all, so none of its pages change (contract SQL-19). This is the
advantage over deleting every row and inserting the list, or over upserting
the list. On 5,000 rows where 1% changed, `replace` wrote 50 rows in 9.2 ms.
A delete plus an `insert_all` wrote 5,000 rows in 15.0 ms, and an
`upsert_all` in five chunks wrote 5,000 rows in 17.6 ms. These are the
medians of nine passes on a 4 vCPU Linux container (`cargo test --release -p
tinystore --lib --features async,testing mirror_against_the_loops --
--ignored --nocapture`, in `crates/tinystore/src/sql/mirror/mirror_tests.rs`).

Values are compared as the table stores them. A column converts a given value
by its declared type, the same way SQLite does when it stores a value. For
example, `4` given to a `text` column becomes `'4'`, and `'12'` given to an
`integer` column becomes `12`, so neither is reported as changed on every
call. An integer and a real compare by their numbers, so `4` and `4.0` are
equal. Text and blobs compare by their bytes. The exception is a key's text,
which compares by the collation of the index that makes the key unique. For
example, with `email text primary key collate nocase`, `'Ann@example.org'`
and `'ann@example.org'` are the same row, and a list that contains both is
`invalid`.

A given row whose key contains a null is `invalid`. A unique index allows any
number of rows with a null in their key, so such a key can't identify one
row.

A mirror writes through its database's writer, and a copy of a node has no
writer. So on a copy, every mirror call fails with `unavailable`, even though
`exec` and `batch` pass their statements on to the file's home. Run the
mirror on the home node.

## Marking instead of deleting

Without `gone`, the mirror deletes a row that the list leaves out. With
`gone`, the row stays, and the mirror sets the column to the store's time in
Unix milliseconds. The mirror doesn't mark a marked row again or report it
again. If the source lists the row again, the mirror clears its mark and
reports it in `added`, so your program can announce it as new.

If your schema keeps times in seconds, give the column with its unit, as a
table's [deleted column](deleted-and-versions.md#seconds-or-milliseconds)
takes it:

```ts
const works = db.mirror<Work>('works', { key: ['course_id', 'id'], gone: { column: 'gone_at', unit: 's' } })
```

```python
works = db.mirror("works", Work, key=["course_id", "id"], gone=sql.Mark("gone_at", unit="s"))
```

```go
works := sqldb.OpenMirror[Work](db, "works", sqldb.MirrorOptions{
	Key: []string{"course_id", "id"}, Gone: "gone_at", GoneUnit: sqldb.Seconds,
})
```

```rust
let works = db.mirror::<Work>("works").key(["course_id", "id"]).gone(sql::Mark::seconds("gone_at")).open()?;
```

The time comes from the store's clock, so a store on a test clock marks rows
with the time the test set (contract SQL-25).

The column belongs to the mirror. Your row type may include it to read the
mark back, for example a `gone_at` field in a dataclass or a struct. The
mirror ignores the field while it is null, and a given row that sets it is
`invalid`. Rows in the answer don't include the column, so the field is null
there.

## What makes a row changed

`changeOn` lists the columns whose change makes a row `changed`. Without it,
every column that the rows contain counts, except the key's columns. If a
row's `changeOn` columns match the table, the row is `same` and is not
written, even if its other columns differ. For example, a `fetched_at` column
that the source rewrites on every read changes nothing. When a row is
changed, the mirror writes every given column that differs, whether
`changeOn` lists it or not.

## A delta

`upsert(rows)` accepts the rows that changed at the source instead of the
whole list. It writes what the rows add and change, and marks nothing gone,
so its `gone` is always empty.

```ts
const diff = await works.upsert(changedSinceYesterday)
```

```python
diff = await works.upsert(changed_since_yesterday)
```

```go
diff, err := works.Upsert(ctx, changedSinceYesterday)
```

```rust
let diff = works.upsert(&changed_since_yesterday)?;
```

## A list that cannot be right

If a source returns an empty page during an outage, `replace` would mark the
whole course gone. `minKept` guards against this. If the scope has live rows
and the list would leave fewer than that fraction of them live, `replace`
writes nothing. An empty list is one such case. The answer is then the diff
that `replace` would have written, with `suspect: true` (contract SQL-20). A
fraction outside (0, 1] is `invalid`.

```ts
const diff = await works.replace({ course_id: 1 }, fetched, { minKept: 0.5 })
```

```python
diff = await works.replace({"course_id": 1}, fetched, min_kept=0.5)
```

```go
diff, err := works.Replace(ctx, map[string]any{"course_id": 1}, fetched, sqldb.MinKept(0.5))
```

```rust
let diff = works.min_kept(0.5).replace(&[("course_id", 1)], &fetched)?;
```

Your program decides what a suspect answer means. For example, to accept a
shorter list once the source keeps returning it, count the suspect answers
with kv's counters. On the third one, send the list again without the guard:

```ts
const doubts = store.counters('mirror-doubts', { ttl: '1d' })
let diff = await works.replace({ course_id: 1 }, fetched, { minKept: 0.5 })
if (diff.suspect && (await doubts.add('works/1')) >= 3) {
	diff = await works.replace({ course_id: 1 }, fetched)
}
if (!diff.suspect) await doubts.delete('works/1')
```

## In a transaction

In Rust, a mirror can join its database's transaction with `tx.with(&works)`.
Its write then commits with the transaction, or not at all. `min_kept` is not
available there.

The SDKs don't support a mirror inside a transaction yet. In TypeScript and
Python, a mirror call inside `db.tx` fails with `invalid`, like any call
made on `db` rather than on `tx` inside a transaction's function. In Go, call the mirror outside `Tx`,
because inside it the call would wait for the writer that the transaction
holds.

```rust
db.tx(|tx| -> tinystore::Result<()> {
    let diff = tx.with(&works).replace(&[("course_id", 1)], &fetched)?;
    tx.exec(sql!("update courses set synced_at = ? where id = ?", now, 1))?;
    Ok(())
})?;
```

## Limits and defaults

| What                                | Bound                                                  |
|-------------------------------------|--------------------------------------------------------|
| rows in one list                    | 100,000 (`mirror rows`), past it fails with `limit`    |
| rows in a scope                     | 100,000 (`mirror rows`)                                |
| the rows a scope reads into memory  | within the store's memory, and 64 MiB per call         |
| a list and its answer over the wire | one message of 2 MiB each, so mirror a scope at a time |

Over the wire, if an answer doesn't fit in one message, the call fails with
`limit` before anything is written.

## What is not built

These are not built yet:

- A saved sync state per scope, such as a cursor that the next call resumes
  from.
- A hash column that would compare a row by one value.
- Publishing the diff to a channel, or appending it to a stream, in the same
  commit.

## See also

- [Tables and queries](tables.md): inserts and upserts of rows the program
  writes itself.
- [Writes and transactions](writes.md): `db.tx`, and what a write that fails
  leaves.
