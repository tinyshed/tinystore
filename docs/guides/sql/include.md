# Rows that include rows

A page often shows rows with rows of their own: each customer with their last
three orders, each post with its first comments. `include` gives each row a
list of another query's rows, in one statement, so the page costs one read
instead of one per row.

```ts
const latest = db
	.table('orders as o')
	.select<{ id: number; total: number }>('o.id, o.total')
	.where('o.user_id = u.id')
	.orderBy('o.id', 'desc')
	.limit(3)

const customers = await db.table<User & { orders: { id: number; total: number }[] }>('users as u')
	.include('orders', latest)
	.where({ 'u.plan': 'team' })
	.all()
// [{ id: 1, email: 'ada@example.com', plan: 'team', orders: [{ id: 2, total: 300 }, { id: 1, total: 1200 }] }]
```

```python
latest = (
    db.table("orders as o")
    .select("o.id, o.total")
    .where("o.user_id = u.id")
    .order_by("o.id", "desc")
    .limit(3)
)

customers = await db.table("users as u").include("orders", latest).where({"u.plan": "team"}).all()
# [{'id': 1, 'email': 'ada@example.com', 'plan': 'team', 'orders': [{'id': 2, 'total': 300}, {'id': 1, 'total': 1200}]}]
```

```go
type OrderLine struct {
	ID    int64
	Total int64
}

type Customer struct {
	User
	Orders []OrderLine
}

latest := sqldb.Select[OrderLine](sqldb.OpenTable[Order](db, "orders as o"), "o.id, o.total").
	Where(sqldb.SQL("o.user_id = u.id")).OrderBy("o.id", sqldb.Desc).Limit(3)

customers, err := sqldb.OpenTable[Customer](db, "users as u").
	Include("orders", latest).Where(sqldb.Eq{"u.plan": "team"}).All(ctx)
```

```rust
#[derive(Serialize, Deserialize)]
struct OrderLine {
    id: i64,
    total: i64,
}

#[derive(Deserialize)]
struct Customer {
    id: i64,
    email: String,
    orders: Vec<OrderLine>,
}

let latest = db
    .table::<Order>("orders as o")
    .select::<OrderLine>("o.id, o.total")
    .filter("o.user_id = u.id")
    .order_by("o.id", Direction::Desc)
    .limit(3);

let customers = db.table::<Customer>("users as u").include("orders", &latest).filter(sql::eq("u.plan", "team")).all()?;
```

## The rules of an included query

| It must                                                                                   | Because                                                             |
|-------------------------------------------------------------------------------------------|---------------------------------------------------------------------|
| name the row it belongs to in its `where`, by the outer table's alias: `o.user_id = u.id` | that is what ties each list to its row                              |
| name its columns with `select`                                                            | the columns become the fields of each included row                  |
| have a `limit`                                                                            | it counts for each row: three orders per customer, not three in all |

The outer table has an alias, `users as u`, so the inner query can name it.
The list comes back under the name `include` gives it, a field of that name
in Go and Rust.

## What SQL it writes

Each include is a correlated subquery in the select list:

```sql
select "u".*, (select group_concat(…) from (
    select o.id, o.total from "orders" as "o" where o.user_id = u.id order by "o"."id" desc limit ?
)) as "orders"
from "users" as "u"
```

SQLite answers it from the index on `orders (user_id, id)`: one statement,
one snapshot, one round trip. Without that index, each row scans the orders
table; `explain()` shows which. `toSQL()` prints the whole statement.

## Values come back as they are kept

The subquery writes each included value as SQLite's own literal of it,
through `quote()`, not as JSON. So an integer past 2⁵³ stays whole, bytes
stay bytes, and text stays text, exactly as in a row read on its own.

## Several includes, and includes inside includes

`include` can be called more than once, a list each:

```ts
const posts = await db.table<Post>('posts as p')
	.include('comments', firstComments)
	.include('tags', tagsOfPost)
	.orderBy('p.id', 'desc')
	.list({ limit: 20 })
```

```python
posts = await (
    db.table("posts as p", Post).include("comments", first_comments).include("tags", tags_of_post).order_by("p.id", "desc").list(limit=20)
)
```

```go
posts, err := sqldb.OpenTable[Post](db, "posts as p").
	Include("comments", firstComments).Include("tags", tagsOfPost).
	OrderBy("p.id", sqldb.Desc).List(ctx, sqldb.Limit(20))
```

```rust
let posts = db
    .table::<Post>("posts as p")
    .include("comments", &first_comments)
    .include("tags", &tags_of_post)
    .order_by("p.id", Direction::Desc)
    .list(20, None)?;
```

A page of `list` includes its rows' lists too: twenty posts with their
comments are still one read.

## When to write the SQL instead

An include reads a few rows for each row of a page. For a total per row, a
count of comments or a sum of orders, `select` it as a subquery, or join and
`groupBy`: one number per row needs no list.

## See also

- [SQL](README.md): the engine's overview.
- [Tables and queries](tables.md): the builder an include is made of.
