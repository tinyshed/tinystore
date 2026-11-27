# Deleted rows and versions

A table's handle in the SQL engine can keep two columns for you. With
`deleted`, a delete marks the row instead of removing it, and every query and
write of the handle skips marked rows. With `version`, every update raises the
row's version, and an edit made from an old version fails with a conflict
instead of silently replacing someone else's work. Both belong to the handle:
a statement you write by hand, `db.all('select …')`, is sent as you wrote it
and sees every row.

The table keeps the two columns. `deleted_at` is a nullable integer, the time
of the delete in unix milliseconds, and `version` is an integer that starts at
0:

```sql
-- 0001_tasks.sql
create table tasks (
    id         integer primary key,
    title      text not null,
    deleted_at integer,
    version    integer not null default 0
) strict;
```

Two people open the same task. Ann saves first. Ben saves from the version he
saw, and his save fails, so Ann's title stays:

```ts
interface Task {
	id: number
	title: string
	deleted_at: number | null
	version: number
}

const tasks = db.table<Task>('tasks', { deleted: 'deleted_at', version: 'version' })

const seen = await tasks.where({ id: 1 }).one() // { id: 1, title: 'Draft', deleted_at: null, version: 0 }
await tasks.where({ id: 1 }).update({ title: 'from Ann' }, { expect: seen!.version }) // version 1
await tasks.where({ id: 1 }).update({ title: 'from Ben' }, { expect: seen!.version })
// ConflictError: sql app: table tasks: the row is at version 1, not 0

await tasks.where({ id: 1 }).delete() // sets deleted_at, and the row stays in the file
await tasks.count() // 0
```

```python
@dataclass
class Task:
    id: int
    title: str
    deleted_at: int | None = None
    version: int = 0


tasks = db.table("tasks", Task, deleted="deleted_at", version="version")

seen = await tasks.where(id=1).one()  # Task(id=1, title='Draft', deleted_at=None, version=0)
await tasks.where(id=1).update({"title": "from Ann"}, expect=seen.version)  # version 1
await tasks.where(id=1).update({"title": "from Ben"}, expect=seen.version)
# tinystore.ConflictError: sql app: table tasks: the row is at version 1, not 0

await tasks.where(id=1).delete()  # sets deleted_at, and the row stays in the file
await tasks.count()  # 0
```

```go
type Task struct {
	ID        int64
	Title     string
	DeletedAt *int64
	Version   int64
}

tasks := sqldb.OpenTable[Task](db, "tasks", sqldb.Deleted("deleted_at"), sqldb.Version("version"))

seen, _, err := tasks.Where(sqldb.Eq{"id": 1}).One(ctx) // {ID:1 Title:Draft DeletedAt:<nil> Version:0}
_, err = tasks.Where(sqldb.Eq{"id": 1}).Update(ctx, sqldb.Set{"title": "from Ann"}, sqldb.Expect(seen.Version))
_, err = tasks.Where(sqldb.Eq{"id": 1}).Update(ctx, sqldb.Set{"title": "from Ben"}, sqldb.Expect(seen.Version))
if errors.Is(err, tinystore.ErrConflict) {
	// sql app: table tasks: the row is at version 1, not 0
}

_, err = tasks.Where(sqldb.Eq{"id": 1}).Delete(ctx) // sets deleted_at, and the row stays in the file
live, err := tasks.Count(ctx)                       // 0
```

```rust
#[derive(Serialize, Deserialize)]
struct Task {
    id: i64,
    title: String,
    deleted_at: Option<i64>,
    version: i64,
}

let tasks = db.table::<Task>("tasks").deleted("deleted_at").version("version");

let seen = tasks.filter(sql::eq("id", &1)).one()?.unwrap(); // Task { id: 1, title: "Draft", deleted_at: None, version: 0 }
tasks.filter(sql::eq("id", &1)).expect(seen.version).update([("title", sql::value("from Ann"))])?; // version 1
let ben = tasks.filter(sql::eq("id", &1)).expect(seen.version).update([("title", sql::value("from Ben"))]);
if ben.is_err_and(|error| error.kind() == ErrorKind::Conflict) {
    // sql app: table tasks: the row is at version 1, not 0: conflict
}

tasks.filter(sql::eq("id", &1)).delete()?; // sets deleted_at, and the row stays in the file
tasks.count()?; // 0
```

The options are `deleted` and `version` in every language:
`deleted / deleted / sqldb.Deleted / .deleted()` and
`version / version / sqldb.Version / .version()`. A table without them
writes exactly the statements it wrote before.

## Delete, restore and purge

`delete` sets the deleted column to the current time. After that, every call
of the handle passes the row by: `where`, `all`, `one`, `count`, `list`,
`each`, `explain` and `update`. Two other handles show the marked rows:

```ts
await tasks.withDeleted().where({ id: 1 }).one() // the row, marked or not
await tasks.deletedOnly().all() // only the marked rows
await tasks.where({ id: 1 }).restore() // clears deleted_at: the row is back
await tasks.deletedOnly().where('deleted_at < ?', monthAgo).purge() // deletes for good
```

```python
await tasks.with_deleted().where(id=1).one()  # the row, marked or not
await tasks.deleted_only().all()  # only the marked rows
await tasks.where(id=1).restore()  # clears deleted_at: the row is back
await tasks.deleted_only().where("deleted_at < ?", month_ago).purge()  # deletes for good
```

```go
row, found, err := tasks.WithDeleted().Where(sqldb.Eq{"id": 1}).One(ctx) // the row, marked or not
marked, err := tasks.DeletedOnly().All(ctx)                              // only the marked rows
_, err = tasks.Where(sqldb.Eq{"id": 1}).Restore(ctx)                     // clears deleted_at: the row is back
_, err = tasks.DeletedOnly().Where(sqldb.SQL("deleted_at < ?", monthAgo)).Purge(ctx) // deletes for good
```

```rust
tasks.with_deleted().filter(sql::eq("id", &1)).one()?; // the row, marked or not
tasks.deleted_only().all()?; // only the marked rows
tasks.filter(sql::eq("id", &1)).restore()?; // clears deleted_at: the row is back
tasks.deleted_only().filter(sql!("deleted_at < ?", month_ago)).purge()?; // deletes for good
```

- `restore` clears the mark of the marked rows that match the conditions,
  whichever handle you call it on.
- `purge` deletes rows from the file. On `deletedOnly()` it deletes the
  marked rows that match. On `withDeleted()` it deletes every row that
  matches, marked or not. On the plain handle it fails with an invalid error,
  so that a purge that forgot `deletedOnly()` never removes live rows
  (contract SQL-22).
- `delete`, `restore` and `purge` need a condition, as `update` does.
- `insert` leaves the deleted column as the row gives it.
- A marked row is still in the file and still holds its key, so an `insert`
  of the same key fails with a conflict error. An upsert whose key belongs to
  a marked row updates that row and clears its mark: the task came back. To
  bring back a row by its key, upsert it.
- In Rust, the mark is the store's clock. In Bun, Python and Go, it is the
  clock of your process.

A handle with a deleted column never shows, counts or changes a marked row
unless you ask with `withDeleted()` or `deletedOnly()` (contract SQL-21).

## Edits made from an old version

With a version column, every `update`, `delete`, `restore` and upsert of the
handle adds one to the row's version (contract SQL-24). You never set the
version yourself: an update or an upsert that names the version column fails
with an invalid error, and an `insert`, or the insert of an upsert, starts the
row at version 0 whatever the row gives for the column, so a type that reads
the version back can carry it.

`expect` makes an update or a delete depend on the version you read:

- If every row that matches is at that version, the edit writes all of them.
- If any row that matches is at another version, the edit writes nothing and
  fails with a conflict error (`ConflictError` in Bun and Python,
  `tinystore.ErrConflict` in Go, `ErrorKind::Conflict` in Rust). The message
  says the version the row is at.
- If no row matches, the edit changes nothing and returns 0 rows changed. It
  is not a conflict.

One statement decides, so an edit of several rows writes all of them or none
(contract SQL-23). When it changes nothing, a second read finds out why, in
the same transaction as the statement. A handle from a transaction,
`tx.table`, runs both in that transaction. `expect` on a table without a
version column is an invalid error, and so is `expect` on a delete without a
deleted column.

A versioned edit costs one statement, as before. The second read runs only
when the edit changed no row. Outside a transaction, an edit with `ifVersion`
runs in a transaction of its own, which holds the writer for that moment. On
a copy of a node in Rust, which has no writer, the statement is passed on to
the file's home as any other write is, and the read that tells a conflict from
no row runs on the copy once it holds the home's commit.

## Clean up deleted rows on a schedule

The store does not purge old marked rows by itself. It cannot know your
table's options while your program is not running. A schedule from the
[jobs engine](../jobs/schedules.md) does it in three lines:

```ts
store.schedule('purge-tasks', { cron: '10 3 * * *', timeZone: 'UTC' }, async run => {
	const monthAgo = run.at.getTime() - 30 * 24 * 60 * 60 * 1000
	await tasks.deletedOnly().where('deleted_at < ?', monthAgo).purge()
})
```

```python
async def purge_tasks(run: tinystore.Run) -> None:
    month_ago = int((run.at - timedelta(days=30)).timestamp() * 1000)
    await tasks.deleted_only().where("deleted_at < ?", month_ago).purge()


store.schedule("purge-tasks", purge_tasks, cron="10 3 * * *", time_zone="UTC")
```

```go
purging, err := jobs.OpenSchedule(store, "purge-tasks", jobs.Cron("10 3 * * *", "UTC"))
err = purging.Work(ctx, func(ctx context.Context, run *jobs.Run) error {
	monthAgo := run.At.AddDate(0, 0, -30).UnixMilli()
	_, err := tasks.DeletedOnly().Where(sqldb.SQL("deleted_at < ?", monthAgo)).Purge(ctx)
	return err
})
```

```rust
let app = db.clone();
let purging = store.schedule("purge-tasks").cron("10 3 * * *", "UTC").work(move |run| {
    let month_ago = run.at() - Duration::from_hours(30 * 24);
    let millis = month_ago.duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as i64;
    let tasks = app.table::<Task>("tasks").deleted("deleted_at");
    tasks.deleted_only().filter(sql!("deleted_at < ?", millis)).purge().map(drop)
})?;
```

## Unique keys and deleted rows

A unique index does not know about the mark. If `slug` is unique, a marked
task still holds its slug, and a new task with the same slug fails. Make the
index partial, so that it covers live rows only:

```sql
create unique index tasks_slug on tasks (slug) where deleted_at is null;
```

A restore can then fail with a conflict, if a live row took the slug in the
meantime.

## Keep a history of every version

A table of past versions is a trigger, and a migration declares it. This one
keeps the old row each time the version goes up, with the time of the change,
and trims a task's history so that it holds the last 50 versions, the current
row among them:

```sql
-- 0002_tasks_history.sql
create table tasks_history (
    id         integer not null,
    version    integer not null,
    title      text not null,
    changed_at integer not null,
    primary key (id, version)
) strict;

create trigger tasks_history after update on tasks
when new.version > old.version
begin
    insert into tasks_history (id, version, title, changed_at)
    values (old.id, old.version, old.title, cast(unixepoch('subsec') * 1000 as integer));
    delete from tasks_history where id = old.id and version <= new.version - 50;
end;
```

Every update through a handle with `version` raises the version, so every
edit, delete and restore leaves one entry. An update written by hand leaves
none unless it raises the version too. `changed_at` is SQLite's clock, the
system's, not the store's.

To merge a burst of edits by one person into one entry, add `edited_by` and
`edited_at` columns that your program sets on each edit, and skip the entry
when the same person edited less than ten minutes before:

```sql
-- 0003_tasks_history_merge.sql
alter table tasks add column edited_by text;
alter table tasks add column edited_at integer;

drop trigger tasks_history;
create trigger tasks_history after update on tasks
when new.version > old.version
    and not (new.edited_by is old.edited_by and new.edited_at - old.edited_at < 600000)
begin
    insert into tasks_history (id, version, title, changed_at)
    values (old.id, old.version, old.title, cast(unixepoch('subsec') * 1000 as integer));
    delete from tasks_history where id = old.id and version <= new.version - 50;
end;
```

You read the history with SQL: `select * from tasks_history where id = ?
order by version desc`. There is no `tasks.history(id)` call.

## See also

- [Tables and queries](tables.md): the builder, its conditions, updates and
  deletes.
- [Migrations](migrations.md): where the table, the index and the trigger are
  declared.
- [Schedules and cron](../jobs/schedules.md): the schedule that purges.
