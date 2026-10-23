# Tables and queries

`db.table` is a typed handle on one table: its inserts and upserts, and a
query builder for the queries that depend on a request, filters a user turns
on and off, a sort they choose, pages. The builder writes the SQL a person
would, and shows it.

```ts
interface Order {
	id: number
	user_id: number
	total: number
	status: string
	created_at: number
}

const orders = db.table<Order>('orders')
const open = await orders.where({ user_id: userId, status: ['pending', 'paid'] }).orderBy('id', 'desc').limit(20).all()
```

```python
@dataclass
class Order:
    id: int
    user_id: int
    total: int
    status: str
    created_at: int


orders = db.table("orders", Order)
open_orders = await orders.where(user_id=user_id, status=["pending", "paid"]).order_by("id", "desc").limit(20).all()
```

```go
type Order struct {
	ID        int64
	UserID    int64
	Total     int64
	Status    string
	CreatedAt int64
}

orders := sqldb.OpenTable[Order](db, "orders")
open, err := orders.Where(sqldb.Eq{"user_id": userID, "status": []string{"pending", "paid"}}).
	OrderBy("id", sqldb.Desc).Limit(20).All(ctx)
```

```rust
#[derive(Serialize, Deserialize)]
struct Order {
    id: i64,
    user_id: i64,
    total: i64,
    status: String,
    created_at: i64,
}

let orders = db.table::<Order>("orders");
let open = orders
    .filter(sql::eq("user_id", &user_id))
    .filter(sql::is_in("status", &["pending", "paid"]))
    .order_by("id", Direction::Desc)
    .limit(20)
    .all()?;
```

A row is read into your type by the columns' names. In Go a field `UserID`
is the column `user_id`, or the name a `db:"…"` or `json:"…"` tag gives it.
A table's handle costs nothing until its first call.

## Insert and upsert

```ts
const user = await users.insert({ email: 'ada@example.com', plan: 'pro' }) // as the file keeps it: id, defaults
await users.insert([{ email: 'bob@example.com' }, { email: 'eve@example.com' }]) // one write, all or none

await users.upsert({ id: 1, email: 'ada@example.com', plan: 'team' }, { conflict: 'id', update: ['plan'] })
```

```python
user = await users.insert({"email": "ada@example.com", "plan": "pro"})  # as the file keeps it: id, defaults
await users.insert([{"email": "bob@example.com"}, {"email": "eve@example.com"}])  # one write, all or none

await users.upsert(User(id=1, email="ada@example.com", plan="team"), conflict="id", update=["plan"])
```

```go
user, err := users.Insert(ctx, User{Email: "ada@example.com", Plan: "pro"}) // as the file keeps it
_, err = users.InsertMany(ctx, []User{{Email: "bob@example.com"}, {Email: "eve@example.com"}})

_, err = users.Upsert(ctx, User{ID: 1, Email: "ada@example.com", Plan: "team"}, sqldb.Conflict("id"), sqldb.Update("plan"))
```

```rust
let user = users.insert(&User { id: 1, email: "ada@example.com".into(), plan: "pro".into() })?;
users.insert_all(&[bob, eve])?; // one write, all or none

users.upsert(&User { id: 1, email: "ada@example.com".into(), plan: "team".into() }, Upsert::on(["id"]).update(["plan"]))?;
```

`insert` gives the row back as the file keeps it, with the id SQLite chose
and the defaults it filled. An upsert inserts the row, or, when the key in
`conflict` is taken, sets only the columns `update` names on the row that
holds it.

An upsert can also say whose row it may change, so that an id from a request
never overwrites another user's row:

```ts
const saved = await notes.upsert(note, { conflict: 'id', update: ['title', 'body'], where: { author_id: userId } })
if (saved === undefined) return forbidden() // the id is someone else's
```

```python
saved = await notes.upsert(note, conflict="id", update=["title", "body"], where={"author_id": user_id})
if saved is None:
    return forbidden()  # the id is someone else's
```

```go
done, err := notes.Upsert(ctx, note, sqldb.Conflict("id"), sqldb.Update("title", "body"),
	sqldb.Where(sqldb.Eq{"author_id": userID}))
if err == nil && done.Changes == 0 {
	return forbidden() // the id is someone else's
}
```

```rust
let saved = notes.upsert(&note, Upsert::on(["id"]).update(["title", "body"]).only_if(sql::eq("author_id", &user_id)))?;
if saved.is_none() {
    return Ok(forbidden()); // the id is someone else's
}
```

## Conditions

`where` (`filter` in Rust) adds a condition with `and`. Each call returns a
new query, so a base query can be shared and narrowed:

| Condition                   | TypeScript                        | SQL it writes                                  |
|-----------------------------|-----------------------------------|------------------------------------------------|
| equal                       | `{ status: 'paid' }`              | `"status" = ?`                                 |
| one of a list               | `{ status: ['paid', 'pending'] }` | `"status" in (select value from json_each(?))` |
| null                        | `{ deleted_at: null }`            | `"deleted_at" is null`                         |
| any SQL                     | `` sql`total >= ${1000}` ``       | `total >= ?`                                   |
| or                          | `sql.or(sql`…`, { user_id: 1 })`  | `(… or "user_id" = ?)`                         |
| a JSON list holding a value | `sql.has('tags', 'urgent')`       | `exists (select 1 from json_each(…))`          |

```ts
const q = orders
	.where({ status: ['paid', 'pending'] })
	.where(sql.or(sql`total >= ${1000}`, { user_id: userId }))
	.orderBy('id', 'desc')
	.limit(50)
q.toSQL() // { text: 'select * from "orders" where "status" in (…) and (total >= ? or "user_id" = ?) order by …', values: […] }
```

```python
q = (
    orders.where(status=["paid", "pending"])
    .where(sql.or_(sql("total >= ?", 1000), {"user_id": user_id}))
    .order_by("id", "desc")
    .limit(50)
)
q.to_sql()  # ('select * from "orders" where "status" in (…) and (total >= ? or "user_id" = ?) order by …', […])
```

```go
q := orders.Where(sqldb.Eq{"status": []string{"paid", "pending"}}).
	Where(sqldb.Or(sqldb.SQL("total >= ?", 1000), sqldb.Eq{"user_id": userID})).
	OrderBy("id", sqldb.Desc).Limit(50)
text, values, err := q.SQL()
```

```rust
let q = orders
    .filter(sql::is_in("status", &["paid", "pending"]))
    .filter(sql::or([sql!("total >= ?", 1000), sql::eq("user_id", &user_id)]))
    .order_by("id", Direction::Desc)
    .limit(50);
let statement = q.to_sql()?;
```

A list of any length is one value that SQLite reads with `json_each`, so a
list of 3 ids and one of 3,000 are the same statement, prepared once.

A condition that is `undefined` in TypeScript is an error, not "any": a
request that forgot an id would otherwise read every user's rows. A filter
that applies only sometimes is an `if` around a `where`.

## Reading

| Call                   | Gives                                                |
|------------------------|------------------------------------------------------|
| `all()`                | every row                                            |
| `one()`                | the one row, or none; two rows are `invalid`         |
| `count()`              | how many rows match, without the order and the limit |
| `each()`               | the rows one at a time, for more than fits in memory |
| `list({limit, after})` | a page, and the cursor of the next                   |
| `explain()`            | SQLite's plan, a line a step                         |

## Pages

```ts
const newest = orders.where({ user_id: userId }).orderBy('id', 'desc')
const page = await newest.list({ limit: 50 })
const next = await newest.list({ limit: 50, after: page.next }) // page.next is undefined after the last page
```

```python
newest = orders.where(user_id=user_id).order_by("id", "desc")
page = await newest.list(limit=50)
nxt = await newest.list(limit=50, after=page.next)  # page.next is None after the last page
```

```go
newest := orders.Where(sqldb.Eq{"user_id": userID}).OrderBy("id", sqldb.Desc)
page, err := newest.List(ctx, sqldb.Limit(50))
next, err := newest.List(ctx, sqldb.Limit(50), sqldb.After(page.Next)) // page.Next is "" after the last page
```

```rust
let newest = orders.filter(sql::eq("user_id", &user_id)).order_by("id", Direction::Desc);
let page = newest.list(50, None)?;
let next = newest.list(50, page.next.as_deref())?; // page.next is None after the last page
```

The next page starts after the last row of this one, by the query's order,
so a row inserted while the user reads never appears twice and never pushes
another out of view, as `offset` would. The order must end in a unique
column: sort by `created_at`, then `id`. A column that holds nulls pages in
SQLite's order, so its nulls come first in ascending order and last in
descending order.

## Columns, groups and joins

```ts
const totals = await orders
	.select<{ user_id: number; total: number }>('user_id, sum(total) as total')
	.groupBy('user_id')
	.having(sql`sum(total) > ${1000}`)
	.all()

const withEmail = await db
	.table('orders as o')
	.join('users as u', 'u.id = o.user_id')
	.select<{ id: number; email: string }>('o.id, u.email')
	.where({ 'o.status': 'paid' })
	.all()
```

```python
totals = await (
    orders.select("user_id, sum(total) as total").group_by("user_id").having(sql("sum(total) > ?", 1000)).all()
)

with_email = await (
    db.table("orders as o").join("users as u", "u.id = o.user_id").select("o.id, u.email").where({"o.status": "paid"}).all()
)
```

```go
type Total struct {
	UserID int64
	Total  int64
}

totals, err := sqldb.Select[Total](orders, "user_id, sum(total) as total").
	GroupBy("user_id").Having(sqldb.SQL("sum(total) > ?", 1000)).All(ctx)
```

```rust
#[derive(Deserialize)]
struct Total {
    user_id: i64,
    total: i64,
}

let totals = orders
    .select::<Total>("user_id, sum(total) as total")
    .group_by(["user_id"])
    .having(sql!("sum(total) > ?", 1000))
    .all()?;
```

`select` names the columns as SQL text and the type of the row they make.
`orderBy` and `groupBy` quote their column as a name, so a sort a request
chooses can never be SQL. To order by an expression, pass a piece of SQL:
`orderBy(sql`lower(email)`)` (`order_by_sql` in Rust).

## Update and delete

```ts
await orders.where({ id: orderId, user_id: userId }).update({ status: 'paid' })
await orders.where(sql`created_at < ${cutoff}`).delete()
```

```python
await orders.where(id=order_id, user_id=user_id).update(status="paid")
await orders.where("created_at < ?", cutoff).delete()
```

```go
_, err = orders.Where(sqldb.Eq{"id": orderID, "user_id": userID}).Update(ctx, sqldb.Set{"status": "paid"})
_, err = orders.Where(sqldb.SQL("created_at < ?", cutoff)).Delete(ctx)
```

```rust
orders
    .filter(sql::eq("id", &order_id))
    .filter(sql::eq("user_id", &user_id))
    .update([("status", sql::value("paid"))])?;
orders.filter(sql!("created_at < ?", cutoff)).delete()?;
```

Both return how many rows changed. An update or a delete without a condition
is refused (contract SQL-09): `delete from orders` is written as SQL, on
purpose.

## Builder or SQL?

Use the builder when the query depends on the request. Use SQL, `db.all`,
`db.one`, `db.scalar`, when the query is fixed, or has subqueries and window
functions the builder would only spell worse. Both send a statement with
values as parameters, and every SDK's builder writes the same statement from
the same steps (contract SQL-10).

## See also

- [SQL](README.md): the engine's overview.
- [Rows that include rows](include.md): each row with the rows of another
  query.
- [Read a chat, a page at a time](../../handbook/chat/messages.md#read-a-chat-a-page-at-a-time)
  in the Handbook.
