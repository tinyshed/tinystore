# Migrations

A database's schema is a folder of `.sql` files, one change each. Opening the
database applies the ones its file has not applied yet and checks the ones it
has, so a program that starts has the schema its code expects, or it does not
start.

```text
migrations/
  0001_users.sql
  0002_orders.sql
  0003_orders_by_user.sql
```

```ts
const db = await store.database('shop', { migrations: `${import.meta.dirname}/migrations` })
```

```python
db = await store.database("shop", migrations=Path(__file__).parent / "migrations")
```

```go
db, err := sqldb.Open(ctx, store, "shop", sqldb.Migrations(os.DirFS("migrations")))
```

```rust
let db = store.database("shop").migrations("migrations").open()?;
```

## A migration file

```sql title="migrations/0002_orders.sql"
create table orders (
    id         integer primary key,
    user_id    integer not null references users (id),
    total      integer not null,
    status     text not null check (status in ('pending', 'paid', 'refunded')),
    created_at integer not null
) strict;

create index orders_by_user on orders (user_id, id);
```

A file is plain SQLite: any number of statements, run in order. The name
starts with its number, and the number decides the order: `0002_orders.sql`
runs after `0001_users.sql`, whatever the rest of the name says. The numbers
count from `0001` without a gap, and a name without one, `0001.sql`, is
refused.

## What opening does

| The file has                                          | Opening                                              |
|-------------------------------------------------------|------------------------------------------------------|
| no migrations applied                                 | applies them all                                     |
| `0001` and `0002`, the folder has `0001`–`0003`       | applies `0003`                                       |
| every one applied, unchanged                          | checks them and opens                                |
| `0001` applied, the file since edited                 | fails: `0001_users.sql changed after it was applied` |
| an applied one renamed, deleted or put before another | fails, naming it                                     |
| more migrations than the folder                       | fails: an older program on a newer file              |
| a draft applied, the file since edited                | fails, naming the command that applies it again      |
| a draft applied, renamed without `.draft`             | records the new name and removes the copy            |

The file keeps the name and the text of each migration it applied, in
`_tinystore_migrations`, and each open compares them with the folder's. Line
endings do not count as an edit. A folder checked out on Windows with
`\r\n` at the end of each line, for example by Git's `core.autocrlf`, opens a
file whose migrations were applied from the same files with `\n`.

The migrations that run at one open run in one transaction: the file has the
old schema or the whole new one, never half (contract SQL-03). Drafts, below,
run in a second one once the database is copied. A statement
that fails names its file and stops the open:

```text
sql shop: …/sql/shop.db: migrations: 0004_refunds.sql: its statements: invalid: no such table: refund
```

## Change a schema

Add the next file. Never edit one that has run:

```sql title="migrations/0003_orders_note.sql"
alter table orders add column note text;
```

There are no "down" migrations. To undo a change, write the next migration
that undoes it. During development, before anything is deployed, deleting
the database's file and starting again is the fastest undo, or write the
migration as a draft.

## A migration still being written

A migration you are still writing is a draft: add `.draft` to its name.

```sql title="migrations/0003_orders_note.draft.sql"
alter table orders add column note text;
```

A draft applies like any other migration, with two differences. Before the
first draft applies, the store copies the database to
`sql/<name>.before-drafts`. And if a draft changes after it ran, the open
fails and names the command that applies it again:

```text
sql shop: …/sql/shop.db: migrations: the draft 0003_orders_note.draft.sql changed after it was applied: `tinystore migrate ./data shop ./migrations --redo-drafts` takes the database back to its copy from before its drafts and applies them again, losing what was written since
```

Stop the program and run it:

```sh
tinystore migrate ./data shop ./migrations --redo-drafts
```

```text
● shop  took back 0003_orders_note.draft.sql, applied 0003_orders_note.draft.sql
```

The database is back to what it held before its first draft, with every
draft applied again as it reads now. Rows written after the drafts are
gone, and so is anything else the database's file holds, such as a bucket,
a queue or an index opened from it. The bytes of its files, `db.files`, stay
as they are. A program that gives its migrations as files rather than a
folder sees `<migrations>` in the command: give the folder they come from.

To ship a draft, drop `.draft` from its name and leave its text as it is.
The next open records the new name and removes the copy, and from then on
the migration is checked like any other:

```text
● shop  shipped 0003_orders_note.sql
```

- Drafts come after every migration that shipped: `0004_refunds.sql` after
  `0003_orders_note.draft.sql` is refused, since taking the draft back would
  take it back too.
- The store never takes a draft back on its own: an edited draft only stops
  the open (contract SQL-12). A draft that reaches production by mistake
  costs a refused open, never the data written since.
- A database that a node keeps for [replication](../../handbook/nodes/copies.md)
  takes no draft, since its copies could not follow it back.

## Rebuilding a table

SQLite's `alter table` adds, renames and drops columns. Anything else, a new
`check`, another type or another key, is a new table, copied:

```sql title="migrations/0004_orders_currency.sql"
create table orders_new (
    id         integer primary key,
    user_id    integer not null references users (id),
    total      integer not null,
    currency   text not null default 'EUR',
    status     text not null check (status in ('pending', 'paid', 'refunded')),
    created_at integer not null
) strict;

insert into orders_new (id, user_id, total, status, created_at)
    select id, user_id, total, status, created_at from orders;
drop table orders;
alter table orders_new rename to orders;
create index orders_by_user on orders (user_id, id);
```

Foreign keys are off while migrations run, so that dropping `orders` does not
cascade into the rows that point at it. They are checked once the migrations
end: a migration that leaves a row without its parent is refused, and
nothing it did is kept (contract SQL-04). Outside migrations, foreign keys are
always on.

## What a migration may not do

A migration runs on the database's one writer, so it may not change that
connection for everyone after it (contract SQL-02):

| Refused                                 | Why                                                                                |
|-----------------------------------------|------------------------------------------------------------------------------------|
| `pragma foreign_keys = on`, any setting | it would stay set on the writer every write shares                                 |
| `begin`, `commit`, `savepoint`          | the migrations already run in one transaction                                      |
| `attach database`                       | a database is one file; two engines never share a commit by accident               |
| `create temp table`, a temporary object | it would stay on the writer, and later writes to its name would never reach a file |

A pragma that only reads, `pragma table_info(orders)`, is allowed.

## Migrations in the binary

A program shipped as one file carries its migrations inside it:

```ts
import users from './migrations/0001_users.sql' with { type: 'text' }
import orders from './migrations/0002_orders.sql' with { type: 'text' }

const db = await store.database('shop', {
	migrations: { '0001_users.sql': users, '0002_orders.sql': orders },
})
```

```python
from importlib.resources import files

folder = files("shop") / "migrations"
db = await store.database("shop", migrations={f.name: f.read_text() for f in folder.iterdir() if f.name.endswith(".sql")})
```

```go
//go:embed migrations/*.sql
var migrations embed.FS

dir, err := fs.Sub(migrations, "migrations")
db, err := sqldb.Open(ctx, store, "shop", sqldb.Migrations(dir))
```

```rust
let db = store
    .database("shop")
    .migrations([
        ("0001_users.sql", include_str!("../migrations/0001_users.sql")),
        ("0002_orders.sql", include_str!("../migrations/0002_orders.sql")),
    ])
    .open()?;
```

## Several programs, one database

Every program that opens a database passes its migrations, and each open
checks them, so two versions of a program cannot disagree about a schema in
silence. During a rolling deploy:

- The new version opens first, applies `0005`, and works.
- An old version that opens the file later fails at start: the file has a
  migration it does not know. Stop the old versions before the new one
  starts, or make `0005` a change the old code does not notice, a new
  table or a new nullable column, and deploy it a version ahead.

`tinystore migrate <dir> <db> <migrations>` applies a folder from a deploy,
before the program starts: no program may hold the directory meanwhile. See
[migrate](../cli/kv-sql-schema.md#migrate).

## Limits and defaults

| What                    | Limit                                                |
|-------------------------|------------------------------------------------------|
| a database's name       | `[a-z0-9][a-z0-9_-]{0,63}`, its file `sql/<name>.db` |
| the engine's own tables | names that start with `_tinystore_`, beside yours    |

## See also

- [SQL](README.md): the engine's overview.
- [Writes and transactions](writes.md): what runs against the schema.
- [Messages](../../handbook/chat/messages.md#the-schema) in the Handbook
  writes a chat's schema.
