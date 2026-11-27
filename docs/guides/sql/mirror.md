# Mirrors

A mirror keeps a table of the SQL engine as a copy of another system's rows,
a course's works or a shop's orders, and says what changed. You give it the
list the source answered; it compares the list with what the table holds by
the source's key, writes only the difference in one commit, and answers what
was added, changed and gone.

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

The table is yours, made by a migration; `key` names its columns that name a
row at the source, and a unique index or the primary key must cover them, or
the mirror is `invalid`. The rows are the program's type, as a table's
inserts take them.

## What replace answers

`replace(scope, rows)` makes the scope, the rows whose columns hold the
scope's values, what the list lists. `{}` is the whole table. Every given row
must carry the scope's columns and agree with them: a row of another course in
course 1's list is `invalid`, naming its key, and so is a key given twice.

| A given row's key | Held as                      | Written                                  | Answered  |
|-------------------|------------------------------|------------------------------------------|-----------|
| not held          |                              | an insert                                | `added`   |
| held              | its `changeOn` columns apart | the given columns that differ            | `changed` |
| held              | the same                     | nothing                                  | `same`    |
| held              | marked gone                  | the columns that differ, the mark nulled | `added`   |
| not in the list   | live                         | the mark set to now, or a delete         | `gone`    |
| not in the list   | marked gone                  | nothing                                  |           |

A row answers by the columns the list's rows carry: `added` and a changed
row's `after` as the list gave them, a changed row's `before` and a gone row
as the table held them. The whole call is one write: if any row fails, a
constraint or a trigger, nothing of it is written (contracts SQL-17, SQL-18).

A row the list gives as it is held is not written at all, so no page of it
changes (contract SQL-19). That is the point against deleting every row and
inserting the list, or upserting it: on 5,000 rows of which 1% changed, a
`replace` wrote 50 rows in 9.2 ms, a delete and an `insert_all` 5,000 in
15.0 ms, and an `upsert_all` in five chunks 5,000 in 17.6 ms, the medians of
nine passes on a 4 vCPU Linux container (`cargo test --release -p tinystore
--lib --features async,testing mirror_against_the_loops -- --ignored
--nocapture`, in `crates/tinystore/src/sql/mirror/mirror_tests.rs`).

Values compare as SQLite's `is` does, with no column's affinity between: an
integer and a real by their numbers, `4` and `4.0` alike, text and blobs by
their bytes. `'4'` is not `4`, so give each column values of its type.

## Marking instead of deleting

Without `gone`, a row the list leaves out is deleted. With it, the row stays
and the column is set to the store's time in Unix milliseconds. A marked row
is not marked or answered again; one the source names again loses its mark
and is answered in `added`, as a program announces it as new. The column
belongs to the mirror: rows that carry it are `invalid`.

## What makes a row changed

`changeOn` names the columns whose change makes a row `changed`; without it,
every column the rows carry but the key's. A row whose `changeOn` columns are
as held is `same` and is not written, even where its other columns differ:
a `fetched_at` the source rewrites at every read changes nothing. A changed
row has every given column that differs written, named in `changeOn` or
not.

## A delta

`upsert(rows)` takes a source's change of some rows rather than its whole
list: it writes what they add and change, and marks nothing gone. Its `gone`
is always empty.

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

A source that answers an empty page during an outage would have a `replace`
mark the whole course gone. `minKept` guards against it: when the scope holds
live rows and the list would leave fewer than that fraction of them live, an
empty list among them, nothing is written, and the answer is the diff that
would have been, with `suspect: true` (contract SQL-20). A fraction outside
(0, 1] is `invalid`.

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

What a suspect answer means is the program's to decide. To believe a
shorter list the source keeps answering, count it with kv's counters and send the
list again without the guard after the third time:

```ts
const doubts = store.counters('mirror-doubts', { ttl: '1d' })
let diff = await works.replace({ course_id: 1 }, fetched, { minKept: 0.5 })
if (diff.suspect && (await doubts.add('works/1')) >= 3) {
	diff = await works.replace({ course_id: 1 }, fetched)
}
if (!diff.suspect) await doubts.delete('works/1')
```

## In a transaction

In Rust, a mirror joins its database's transaction, `tx.with(&works)`, and
its write commits with the transaction's or not at all; `min_kept` is not
offered there. The SDKs have no mirror inside a transaction yet: in
TypeScript and Python, a mirror's call inside `db.tx` is `invalid`, as any
call made around the transaction from inside it, and in Go, make it outside
`Tx`, since inside it would wait for the writer the transaction holds.

```rust
db.tx(|tx| -> tinystore::Result<()> {
    let diff = tx.with(&works).replace(&[("course_id", 1)], &fetched)?;
    tx.exec(sql!("update courses set synced_at = ? where id = ?", now, 1))?;
    Ok(())
})?;
```

## Limits and defaults

| What                                | Bound                                             |
|-------------------------------------|---------------------------------------------------|
| rows of one list                    | 100,000, `mirror rows`; past it `limit`           |
| rows a scope holds                  | 100,000, `mirror rows`                            |
| the rows a scope reads into memory  | within the store's memory and 64 MiB a call       |
| a list and its answer over the wire | one message each, 2 MiB: mirror a scope at a time |

Over the wire, an answer past one message is refused `limit` before anything
is written.

## What is not built

A kept state of a sync per scope, a cursor the next call resumes from, is
not yet built, nor a hash column that would compare a row by one value, nor
the diff published to a channel or appended to a stream in the same commit.

## See also

- [Tables and queries](tables.md): inserts and upserts of rows the program
  writes itself.
- [Writes and transactions](writes.md): `db.tx`, and what a write that fails
  leaves.
