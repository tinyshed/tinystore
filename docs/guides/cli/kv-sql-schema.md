# kv, sql and schema

These commands look inside a store's data while its program runs: `kv` reads
a bucket, `sql` runs a query on a database, `schema` shows a database's
tables, and `search` lists the store's indexes. They read as
[every read](README.md#how-it-behaves) of the command line does, and write
only when told: `kv set`, `kv delete`, `sql --write` and `search drop`.
`migrate` applies a database's migrations from a shell.

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
`--database` names it, for `kv set` and `kv delete` too. A value written to an
encrypted bucket is written encrypted.

### Write a key

```sh
tinystore kv ./data set flags beta on --ttl 1h
tinystore kv ./data set counts hits 42 --as int
echo '{"theme": "dark"}' | tinystore kv ./data set settings 42 - --as json
tinystore kv ./data delete sessions 42
```

```text
set beta in bucket flags, expiring in 1h
deleted 42 from bucket sessions
```

- A value is text unless `--as int` or `--as json` says otherwise. Nothing is
  guessed from how it looks, so `02134` stays text.
- `-` reads the value from stdin, without the line end `echo` adds. Use it for
  JSON in Windows PowerShell 5.1, which drops the quotes inside an argument.
- `--ttl 1h` expires the key an hour from now.
- An encrypted bucket is written encrypted.
- A set or a delete sent twice leaves the same as once.

They write through a server, as [every write](README.md#who-writes) of the
command line does.

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
- `--write` lets it write. When no program holds the directory, it opens the
  store itself. When `tinystore serve` or an SDK's sidecar holds it, the
  statement goes through that server, once, since a statement sent again may
  write twice; there, a statement that gives rows back, a read or a
  `returning`, is refused. A program that opened the store `embedded` keeps
  its writer to itself: stop it, or write from it.
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
and checks the applied ones as opening the database from a program does: see
[Migrations](../sql/migrations.md). No program may hold the directory
meanwhile, so run it in a deploy before the program starts.

## search

```sh
tinystore search list ./data
tinystore search drop ./data posts
```

```text
● ./data  1 index
  products  3 documents · never opened
```

`list` reads beside the program or server that holds the directory. `drop`
needs a directory that no program or server holds, as `migrate` does. While
one holds it, drop the index through it with `dropIndex`. An index no program
opens any more is kept until it is dropped, as
[Listing and dropping](../search/rebuilds.md#listing-and-dropping) explains.

## See also

- [The command line](README.md).
- [AI agents](mcp.md): the same reads for an agent.
