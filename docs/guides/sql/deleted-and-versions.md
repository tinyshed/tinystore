# Deleted rows and versions

A table handle in the SQL engine can manage two columns for you. With
`deleted`, a delete marks the row instead of removing it, and every query and
write of the handle skips marked rows. With `version`, every update raises the
row's version, and an edit made from an old version fails with a conflict
instead of silently replacing someone else's work. Both are features of the
handle. A statement that you write by hand, such as `db.all('select …')`,
runs exactly as you wrote it and sees every row.

The table needs the two columns. `deleted_at` is a nullable integer that
stores the time of the delete in Unix milliseconds. `version` is an integer
that starts at 0:

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
await tasks.where({ id: 1 }).update({ title: 'from Ann' }, { ifVersion: seen!.version }) // version 1
await tasks.where({ id: 1 }).update({ title: 'from Ben' }, { ifVersion: seen!.version })
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
await tasks.where(id=1).update({"title": "from Ann"}, if_version=seen.version)  # version 1
await tasks.where(id=1).update({"title": "from Ben"}, if_version=seen.version)
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
_, err = tasks.Where(sqldb.Eq{"id": 1}).Update(ctx, sqldb.Set{"title": "from Ann"}, sqldb.IfVersion(seen.Version))
_, err = tasks.Where(sqldb.Eq{"id": 1}).Update(ctx, sqldb.Set{"title": "from Ben"}, sqldb.IfVersion(seen.Version))
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
tasks.filter(sql::eq("id", &1)).if_version(seen.version).update([("title", sql::value("from Ann"))])?; // version 1
let ben = tasks.filter(sql::eq("id", &1)).if_version(seen.version).update([("title", sql::value("from Ben"))]);
if ben.is_err_and(|error| error.kind() == ErrorKind::Conflict) {
    // sql app: table tasks: the row is at version 1, not 0: conflict
}

tasks.filter(sql::eq("id", &1)).delete()?; // sets deleted_at, and the row stays in the file
tasks.count()?; // 0
```

The options are `deleted` and `version` in every language:
`deleted / deleted / sqldb.Deleted / .deleted()` and
`version / version / sqldb.Version / .version()`. A table without these
options runs exactly the same statements as before.

## Delete, restore and purge

`delete` sets the deleted column to the current time. After that, every call
of the handle skips the row: `where`, `all`, `one`, `count`, `list`, `each`,
`explain` and `update`. Two other handles show the marked rows:

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

- `restore` clears the mark on the marked rows that match the conditions,
  whichever handle you call it on.
- `purge` deletes rows from the file. On `deletedOnly()` it deletes the
  marked rows that match. On `withDeleted()` it deletes every row that
  matches, marked or not. On the plain handle it fails with an invalid error,
  so a purge that forgot `deletedOnly()` never removes live rows
  (contract SQL-22).
- `delete`, `restore` and `purge` need a condition, like `update` does.
- `insert` writes the deleted column as the row gives it.
- A marked row is still in the file and still uses its key, so an `insert`
  with the same key fails with a conflict error. An upsert whose key belongs
  to a marked row updates that row and clears its mark, so the task is back.
  To bring back a row by its key, upsert it.
- In Rust, the time of the mark comes from the store's clock. In Bun, Python
  and Go, it comes from your process's clock.

A handle with a deleted column never shows, counts or changes a marked row
unless you ask with `withDeleted()` or `deletedOnly()` (contract SQL-21).

## Edits made from an old version

With a version column, every `update`, `delete`, `restore` and upsert
through the handle adds one to the row's version (contract SQL-24). You never
set the version yourself. An update or an upsert that sets the version column
fails with an invalid error. An `insert`, or the insert part of an upsert,
starts the row at version 0, whatever value the row gives for the column. So
your row type can include the version field to read it back.

`ifVersion` makes an update or a delete depend on the version you read:

- If every row that matches is at that version, the edit writes all of them.
- If any row that matches is at another version, the edit writes nothing and
  fails with a conflict error (`ConflictError` in Bun and Python,
  `tinystore.ErrConflict` in Go, `ErrorKind::Conflict` in Rust). The message
  says the version the row is at.
- If no row matches, the edit changes nothing and returns 0 rows changed. It
  is not a conflict.

The version check and the write are one statement, so an edit of several
rows writes all of them or none (contract SQL-23). When the edit changes
nothing, a second read finds out why, in the same transaction as the
statement. A handle from a transaction, `tx.table`, runs both in that
transaction. `ifVersion` on a table without a version column fails with an
invalid error. So does `ifVersion` on a delete when the table has no deleted
column.

A versioned edit still costs one statement. The second read runs only when
the edit changed no row. Outside a transaction, an edit with `ifVersion` runs
in its own transaction, which holds the writer for that moment. A copy of a
node in Rust has no writer, so it passes the statement on to the file's home,
like any other write. The read that tells a conflict apart from a missing row
then runs on the copy, once the copy has the home's commit.

## Clean up deleted rows on a schedule

The store does not purge old marked rows by itself, because it cannot know
your table's options while your program is not running. A schedule from the
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

A unique index ignores the mark. If `slug` is unique, a marked task still
uses its slug, and a new task with the same slug fails. Make the index
partial, so that it covers live rows only:

```sql
create unique index tasks_slug on tasks (slug) where deleted_at is null;
```

Then a restore can fail with a conflict if a live row took the slug in the
meantime.

## Keep a history of every version

To keep past versions, add a table and a trigger in a migration. This trigger
saves the old row, with the time of the change, each time the version goes
up. It also trims each task's history to the last 50 versions, counting the
current row:

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
edit, delete and restore adds one entry. An update written by hand adds none
unless it raises the version too. `changed_at` comes from SQLite's clock,
which is the system clock, not the store's clock.

To merge a burst of edits by one person into one entry, add `edited_by` and
`edited_at` columns that your program sets on each edit. Then skip the entry
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

Read the history with SQL: `select * from tasks_history where id = ?
order by version desc`. There is no `tasks.history(id)` call.

## See also

- [Tables and queries](tables.md): the builder, its conditions, updates and
  deletes.
- [Migrations](migrations.md): where the table, the index and the trigger are
  declared.
- [Schedules and cron](../jobs/schedules.md): the schedule that purges.
