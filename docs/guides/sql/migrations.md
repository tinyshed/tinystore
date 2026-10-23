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

The migrations that run at one open run in one transaction: the file has the
old schema or the whole new one, never half (contract SQL-03). A statement
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
the database's file and starting again is the fastest undo.

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

`tinystore migrate <dir> <name> <folder>` applies a folder beside a running
server, for a deploy that migrates before it starts the program.

## Limits

| What                    | Limit                                                |
|-------------------------|------------------------------------------------------|
| a migration's name      | `0001_name.sql`: its number, then a name             |
| a database's name       | `[a-z0-9][a-z0-9_-]{0,63}`, its file `sql/<name>.db` |
| the engine's own tables | names that start with `_tinystore_`, beside yours    |

## See also

- [SQL](README.md): the engine's overview.
- [Writes and transactions](writes.md): what runs against the schema.
- [Messages](../../handbook/chat/messages.md#the-schema) in the Handbook
  writes a chat's schema.
