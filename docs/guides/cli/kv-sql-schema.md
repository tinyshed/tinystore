# kv, sql and schema

Three commands look inside a store's data while its program runs: `kv` reads a
bucket, `sql` runs a query on a database, `schema` shows a database's tables.
`migrate` applies a database's migrations from a shell. Each reads beside the
program or server that holds the directory, without writing a byte, unless
told to write.

## kv

```sh
tinystore kv ./data list sessions        # a bucket's keys and values
tinystore kv ./data list sessions 42     # one branch's
tinystore kv ./data get sessions 42      # one value
tinystore kv ./data get sessions 42 --json
tinystore kv ./data list drafts --database app   # a bucket kept in sql/app.db
```

```text
42  {"user":42}
```

```text
{"expires":1795021159722,"key":"42","value":{"user":42}}
```

`--json` gives each value with its key and expiry. An encrypted bucket's
values are read with the key the directory holds. A bucket the program opened
from a database, `db.bucket("drafts")`, lives in that database's file, and
`--database` names it.

## sql

```sh
tinystore sql ./data shop "select id, email, plan from users"
tinystore sql ./data shop "select count(*) as n from users" --json
tinystore sql ./data shop "delete from users where id = 7" --write
```

```text
id  email            plan
1   ada@example.com  team
2   bob@example.com  pro
3   eve@example.com  free
```

- Without `--write`, a statement runs on a read-only connection: a write is
  refused, so a typo cannot change data.
- `--write` lets it write, and needs the directory that no program holds: a
  running program's writer is its own. Stop the program, or write through it.
- `--json` gives one document, the rows by their columns' names.

## schema

```sh
tinystore schema ./data          # the store's databases
tinystore schema ./data shop     # a database's tables, columns and indexes
```

```text
CREATE TABLE users (id integer primary key, email text not null unique, plan text not null default 'free') strict;

CREATE TABLE orders (id integer primary key, user_id integer not null references users (id), total integer not null, …) strict;

CREATE INDEX orders_by_user on orders (user_id, id);
```

## migrate

```sh
tinystore migrate ./data shop ./migrations
```

It applies the migrations the database has not applied, in one transaction,
and refuses a database that applied an edited, renamed or missing file, as
opening it from a program does. No program may hold the directory meanwhile:
run it in a deploy before the program starts. See
[Migrations](../sql/migrations.md).

## search

```sh
tinystore search list ./data
tinystore search drop ./data posts
```

```text
● ./data  1 index
  products  3 documents · never opened
```

An index no program opens any more is kept until it is dropped, since another
version of the program may open it again.

## See also

- [The command line](README.md).
- [AI agents](mcp.md): the same reads for an agent.
