//! A table's queries, built from pieces into the SQL a person would write: the
//! builder adds the ands, the parentheses, the placeholders and the quotes
//! around names, and nothing else. The Bun SDK's builder writes the same text
//! from the same steps, which testdata/sql/queries.json holds both to.

use std::marker::PhantomData;

use serde::Serialize;
use serde::de::DeserializeOwned;

use super::database::{Database, Done};
use super::included::{literal_of, names_of};
use super::pieces::{checked, joined_with, quote};
use super::rows::{Nested, Rows};
use super::run::Wanted;
use super::statement::Sql;
use super::tx::Tx;
use super::values::{Value, row_of};
use crate::{Error, Result};

/// What runs a table's statements: the database, or a transaction of it.
#[derive(Clone, Debug)]
enum Runner<'r> {
    Database(Database),
    Tx(&'r Tx<'r>),
    /// A test's runner, which keeps the last statement and answers nothing.
    #[cfg(test)]
    Recorder(std::sync::Arc<std::sync::Mutex<Option<Sql>>>),
}

impl Runner<'_> {
    fn rows_of(&self, statement: &Sql, wanted: Wanted) -> Result<Rows> {
        match self {
            Runner::Database(database) => database.rows_of(statement, wanted),
            Runner::Tx(tx) => tx.rows_of(statement, wanted),
            #[cfg(test)]
            Runner::Recorder(last) => {
                *last.lock().unwrap() = Some(statement.clone());
                Ok(Rows::answered(wanted))
            }
        }
    }

    fn exec_of(&self, statement: Sql) -> Result<Done> {
        match self {
            Runner::Database(database) => database.exec(statement),
            Runner::Tx(tx) => tx.exec(statement),
            #[cfg(test)]
            Runner::Recorder(last) => {
                *last.lock().unwrap() = Some(statement);
                Ok(Done::default())
            }
        }
    }
}

/// The way an order goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Asc,
    Desc,
}

impl Direction {
    fn text(self) -> &'static str {
        match self {
            Direction::Asc => "asc",
            Direction::Desc => "desc",
        }
    }
}

/// A page of a query's rows, and the cursor that reads the next; none after
/// the last.
#[derive(Clone, Debug, PartialEq)]
pub struct Page<T> {
    pub rows: Vec<T>,
    pub next: Option<String>,
}

/// What an upsert does when the row's key is taken:
/// `Upsert::on("id").update(["title"]).only_if(sql::eq("author_id", &user_id))`.
#[derive(Clone, Debug)]
pub struct Upsert {
    conflict: Vec<String>,
    update: Vec<String>,
    only_if: Option<Sql>,
}

impl Upsert {
    /// The columns whose value, taken, makes the insert an update: the key.
    pub fn on<I: IntoIterator<Item = S>, S: Into<String>>(conflict: I) -> Upsert {
        Upsert { conflict: conflict.into_iter().map(Into::into).collect(), update: Vec::new(), only_if: None }
    }

    /// The columns the update sets from the row; none other changes.
    #[must_use]
    pub fn update<I: IntoIterator<Item = S>, S: Into<String>>(mut self, columns: I) -> Upsert {
        self.update = columns.into_iter().map(Into::into).collect();
        self
    }

    /// The condition the row that holds the key must meet, its owner's, or
    /// it stays as it is.
    #[must_use]
    pub fn only_if(mut self, condition: impl Into<Sql>) -> Upsert {
        self.only_if = Some(condition.into());
        self
    }
}

#[derive(Clone, Debug)]
struct Order {
    piece: Sql,
    column: Option<String>,
    direction: Direction,
}

/// A query whose rows each row of another holds as a list under a name.
#[derive(Clone, Debug)]
struct Included {
    name: String,
    parts: Parts,
    /// The names of the columns its select gives, in their order.
    columns: Vec<String>,
}

#[derive(Clone, Debug)]
struct Parts {
    table: String,
    alias: Option<String>,
    select: Option<Sql>,
    joins: Vec<Sql>,
    conditions: Vec<Sql>,
    groups: Vec<String>,
    having: Vec<Sql>,
    order: Vec<Order>,
    limit: Option<u64>,
    offset: Option<u64>,
    includes: Vec<Included>,
    /// The first piece the query could not take, told when it runs.
    refused: Option<String>,
}

/// A query on a table: a value, each call returning a new one, so that the
/// first stays as it was. `all`, `one`, `scalar`, `count` and `list` read;
/// `update` and `delete` write.
///
/// ```no_run
/// # use tinystore::sql::{self, Direction};
/// # #[derive(serde::Serialize, serde::Deserialize)] struct Order { id: String }
/// # fn main() -> tinystore::Result<()> {
/// # let db = tinystore::Store::open("data", Default::default())?.database("app").open()?;
/// let urgent: Vec<Order> = db
///     .table::<Order>("orders")
///     .filter(sql::eq("status", "pending"))
///     .filter(sql::or([tinystore::sql!("amount >= ?", 1000), sql::eq("priority", "high")]))
///     .order_by("created_at", Direction::Desc)
///     .limit(50)
///     .all()?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Debug)]
pub struct Query<'r, T> {
    runner: Runner<'r>,
    parts: Parts,
    row: PhantomData<fn() -> T>,
}

/// A table: its queries, and its rows' inserts and upserts. `Deref`s to the
/// query of all its rows.
#[derive(Clone, Debug)]
pub struct Table<'r, T> {
    name: String,
    query: Query<'r, T>,
}

impl<'r, T> std::ops::Deref for Table<'r, T> {
    type Target = Query<'r, T>;

    fn deref(&self) -> &Query<'r, T> {
        &self.query
    }
}

impl Database {
    /// A table of the database, typed where it opens: its rows' inserts and
    /// upserts, and queries built without SQL text. It costs nothing until
    /// its first call.
    pub fn table<T>(&self, name: &str) -> Table<'static, T> {
        Table::new(Runner::Database(self.clone()), name)
    }
}

impl<'t> Tx<'t> {
    /// A table inside the transaction: its calls are the transaction's.
    pub fn table<T>(&'t self, name: &str) -> Table<'t, T> {
        Table::new(Runner::Tx(self), name)
    }
}

impl<'r, T> Table<'r, T> {
    fn new(runner: Runner<'r>, name: &str) -> Self {
        let (table, alias, refused) = match table_ref(name) {
            Ok((table, alias)) => (table, alias, None),
            Err(why) => (name.to_owned(), None, Some(why)),
        };
        let parts = Parts {
            table: table.clone(),
            alias,
            select: None,
            joins: Vec::new(),
            conditions: Vec::new(),
            groups: Vec::new(),
            having: Vec::new(),
            order: Vec::new(),
            limit: None,
            offset: None,
            includes: Vec::new(),
            refused,
        };
        Table { name: table, query: Query { runner, parts, row: PhantomData } }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// A table whose statements a test reads rather than runs.
    #[cfg(test)]
    pub(crate) fn recorded(name: &str, last: std::sync::Arc<std::sync::Mutex<Option<Sql>>>) -> Self {
        Table::new(Runner::Recorder(last), name)
    }
}

impl<T: Serialize + DeserializeOwned> Table<'_, T> {
    /// Inserts a row, every field it has, and gives it back as the file keeps
    /// it: the defaults and the generated columns the database filled.
    pub fn insert(&self, row: &T) -> Result<T> {
        let mut rows = self.insert_all(std::slice::from_ref(row))?;
        rows.pop().ok_or_else(|| Error::internal("an insert that gave back no row"))
    }

    /// Inserts rows of one shape as one write, all of them or none.
    pub fn insert_all(&self, rows: &[T]) -> Result<Vec<T>> {
        if rows.is_empty() {
            return Ok(Vec::new());
        }
        let insert = insert_of(&self.name, rows)?;
        let (text, values, _) = insert.into_parts();
        let returning = Sql::piece(format!("{text} returning *"), values, false);
        self.query.runner.rows_of(&returning, Wanted::All)?.decode()
    }

    /// Inserts a row, or, when its key is taken, sets the columns `update`
    /// names on the row that holds it, if that row meets the upsert's
    /// condition; `None` when the condition kept it.
    pub fn upsert(&self, row: &T, upsert: Upsert) -> Result<Option<T>> {
        if upsert.update.is_empty() {
            return Err(Error::invalid("an upsert names the columns its update sets"));
        }
        let insert = insert_of(&self.name, std::slice::from_ref(row))?;
        let (insert_text, mut values, _) = insert.into_parts();
        let conflict =
            upsert.conflict.iter().map(|name| quote(name)).collect::<Result<Vec<_>, _>>().map_err(Error::invalid)?;
        let sets = upsert
            .update
            .iter()
            .map(|name| quote(name).map(|quoted| format!("{quoted} = excluded.{quoted}")))
            .collect::<Result<Vec<_>, _>>()
            .map_err(Error::invalid)?;
        let mut text = format!("{insert_text} on conflict ({}) do update set {}", conflict.join(", "), sets.join(", "));
        if let Some(condition) = upsert.only_if {
            // in an upsert's update, a column is the row's that holds the key
            let (condition, condition_values, refused) = checked(condition).into_parts();
            if let Some(why) = refused {
                return Err(Error::invalid(why));
            }
            text.push_str(&format!(" where {condition}"));
            values.extend(condition_values);
        }
        let statement = Sql::piece(format!("{text} returning *"), values, false);
        let rows: Vec<T> = self.query.runner.rows_of(&statement, Wanted::One)?.decode()?;
        Ok(rows.into_iter().next())
    }
}

impl<'r, T> Query<'r, T> {
    /// Adds a condition with `and`: a piece from `sql!`, `sql::eq`, `sql::or`
    /// and the rest. `filter`, since `where` is Rust's own word.
    #[must_use]
    pub fn filter(&self, condition: impl Into<Sql>) -> Self {
        self.with(|parts| parts.conditions.push(checked(condition.into())))
    }

    /// The columns a row holds, as SQL text, and the shape of the row they make.
    pub fn select<S>(&self, columns: impl Into<Sql>) -> Query<'r, S> {
        let mut parts = self.parts.clone();
        parts.select = Some(checked(columns.into()));
        Query { runner: self.runner.clone(), parts, row: PhantomData }
    }

    /// Gives each row the rows of another query as a list under `name`, a
    /// field of the row's type, from the same statement: the query names the
    /// row it belongs to in its `filter`, by this table's alias, names its
    /// columns with `select`, and has a `limit`, which counts for each row.
    /// Every value comes back as it is kept.
    ///
    /// ```no_run
    /// # use tinystore::sql::Direction;
    /// # #[derive(serde::Deserialize)] struct Post { id: i64, title: String }
    /// # #[derive(serde::Deserialize)] struct Author { id: i64, posts: Vec<Post> }
    /// # fn main() -> tinystore::Result<()> {
    /// # let db = tinystore::Store::open("data", Default::default())?.database("app").open()?;
    /// let latest = db
    ///     .table::<Post>("posts as p")
    ///     .select::<Post>("p.id, p.title")
    ///     .filter("p.author_id = u.id")
    ///     .order_by("p.id", Direction::Desc)
    ///     .limit(3);
    /// let authors: Vec<Author> = db.table::<Author>("users as u").include("posts", &latest).all()?;
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn include<C>(&self, name: &str, query: &Query<'_, C>) -> Self {
        let included = included(name, &query.parts, &self.parts);
        self.with(|parts| match included {
            Ok(included) => parts.includes.push(included),
            Err(why) => refuse(parts, format!("the included {name:?}: {why}")),
        })
    }

    /// Joins a table, `"users as u"`, on a condition.
    #[must_use]
    pub fn join(&self, table: &str, on: impl Into<Sql>) -> Self {
        self.joined("join", table, on.into())
    }

    /// Joins a table whose columns may be null where no row of it meets the condition.
    #[must_use]
    pub fn left_join(&self, table: &str, on: impl Into<Sql>) -> Self {
        self.joined("left join", table, on.into())
    }

    /// Groups the rows by columns, quoted as names.
    #[must_use]
    pub fn group_by<I: IntoIterator<Item = S>, S: AsRef<str>>(&self, columns: I) -> Self {
        let quoted: Vec<Result<String, String>> = columns.into_iter().map(|column| quote(column.as_ref())).collect();
        self.with(|parts| {
            for column in quoted {
                match column {
                    Ok(column) => parts.groups.push(column),
                    Err(why) => refuse(parts, why),
                }
            }
        })
    }

    /// A condition on the groups, as `filter` takes one.
    #[must_use]
    pub fn having(&self, condition: impl Into<Sql>) -> Self {
        self.with(|parts| parts.having.push(checked(condition.into())))
    }

    /// Orders the rows by a column, quoted as a name, so that a sort a
    /// request chooses cannot be SQL.
    #[must_use]
    pub fn order_by(&self, column: &str, direction: Direction) -> Self {
        let quoted = quote(column);
        self.with(|parts| match quoted {
            Ok(quoted) => parts.order.push(Order {
                piece: Sql::piece(quoted, Vec::new(), true),
                column: Some(column.to_owned()),
                direction,
            }),
            Err(why) => refuse(parts, why),
        })
    }

    /// Orders the rows by a piece of SQL: `sql!("lower(title)")`.
    #[must_use]
    pub fn order_by_sql(&self, piece: impl Into<Sql>, direction: Direction) -> Self {
        self.with(|parts| parts.order.push(Order { piece: checked(piece.into()), column: None, direction }))
    }

    #[must_use]
    pub fn limit(&self, rows: u64) -> Self {
        self.with(|parts| parts.limit = Some(rows))
    }

    #[must_use]
    pub fn offset(&self, rows: u64) -> Self {
        self.with(|parts| parts.offset = Some(rows))
    }

    /// The text and the values the query sends, to read or to log.
    pub fn to_sql(&self) -> Result<Sql> {
        render(&self.parts)
    }

    /// The rows the conditions match, without the order and the limit.
    pub fn count(&self) -> Result<u64> {
        let mut counted = self.parts.clone();
        counted.select = Some(Sql::new("count(*)"));
        counted.order.clear();
        counted.limit = None;
        counted.offset = None;
        counted.includes.clear();
        let statement = if counted.groups.is_empty() {
            render(&counted)?
        } else {
            counted.select = Some(Sql::new("1"));
            let (inner, values, _) = render(&counted)?.into_parts();
            Sql::piece(format!("select count(*) from ({inner})"), values, false)
        };
        self.runner.rows_of(&statement, Wanted::Scalar)?.scalar()
    }

    /// SQLite's plan for the query, a line a step.
    pub fn explain(&self) -> Result<Vec<String>> {
        let (text, values, _) = render(&self.parts)?.into_parts();
        let plan = Sql::piece(format!("explain query plan {text}"), values, false);
        let rows: Vec<(i64, i64, i64, String)> = self.runner.rows_of(&plan, Wanted::All)?.decode()?;
        Ok(rows.into_iter().map(|(_, _, _, detail)| detail).collect())
    }

    /// Sets columns of the rows the conditions match, each a value from
    /// `sql::value` or a piece of SQL, and says how many changed.
    pub fn update<I, S>(&self, sets: I) -> Result<Done>
    where
        I: IntoIterator<Item = (S, Sql)>,
        S: AsRef<str>,
    {
        self.refuse_every_row("an update")?;
        let mut texts = Vec::new();
        let mut values = Vec::new();
        for (column, value) in sets {
            let column = quote(column.as_ref()).map_err(Error::invalid)?;
            let (text, value_values, refused) = checked(value).into_parts();
            if let Some(why) = refused {
                return Err(Error::invalid(format!("{column}: {why}")));
            }
            texts.push(format!("{column} = {text}"));
            values.extend(value_values);
        }
        if texts.is_empty() {
            return Err(Error::invalid("an update sets at least one column"));
        }
        let (condition, condition_values) = conditions_text(&self.parts.conditions)?;
        values.extend(condition_values);
        let table = quote(&self.parts.table).map_err(Error::invalid)?;
        let text = format!("update {table} set {} where {condition}", texts.join(", "));
        self.runner.exec_of(Sql::piece(text, values, false))
    }

    /// Deletes the rows the conditions match, and says how many it deleted.
    pub fn delete(&self) -> Result<Done> {
        self.refuse_every_row("a delete")?;
        let (condition, values) = conditions_text(&self.parts.conditions)?;
        let table = quote(&self.parts.table).map_err(Error::invalid)?;
        self.runner.exec_of(Sql::piece(format!("delete from {table} where {condition}"), values, false))
    }

    fn with(&self, change: impl FnOnce(&mut Parts)) -> Self {
        let mut parts = self.parts.clone();
        change(&mut parts);
        Query { runner: self.runner.clone(), parts, row: PhantomData }
    }

    fn joined(&self, kind: &str, table: &str, on: Sql) -> Self {
        let joined = table_ref(table).map(|(name, alias)| table_text(&name, alias.as_deref()));
        let on = checked(on);
        self.with(|parts| match joined {
            Ok(table) => {
                let (text, values, refused) = on.into_parts();
                if let Some(why) = refused {
                    return refuse(parts, why);
                }
                parts.joins.push(Sql::piece(format!("{kind} {table} on {text}"), values, false));
            }
            Err(why) => refuse(parts, why),
        })
    }

    /// Refuses an update or a delete of every row, which SQL says plainly.
    fn refuse_every_row(&self, what: &str) -> Result<()> {
        if let Some(why) = &self.parts.refused {
            return Err(Error::invalid(why.clone()));
        }
        if self.parts.conditions.iter().all(|condition| condition.text() == "1") {
            return Err(Error::invalid(format!("{what} without a condition: every row is said in SQL, delete from …")));
        }
        if !self.parts.joins.is_empty() {
            return Err(Error::invalid(format!("{what} of a join: write it in SQL")));
        }
        Ok(())
    }
}

impl<T: DeserializeOwned> Query<'_, T> {
    pub fn all(&self) -> Result<Vec<T>> {
        self.runner.rows_of(&render(&self.parts)?, Wanted::All)?.nesting(nested(&self.parts)).decode()
    }

    pub fn one(&self) -> Result<Option<T>> {
        let rows = self.runner.rows_of(&render(&self.parts)?, Wanted::One)?.nesting(nested(&self.parts));
        Ok(rows.decode::<T>()?.into_iter().next())
    }

    pub fn scalar<V: DeserializeOwned>(&self) -> Result<V> {
        self.runner.rows_of(&render(&self.parts)?, Wanted::Scalar)?.scalar()
    }

    /// A page after the last row of the one before, by the query's order,
    /// which ends in a unique column so that no row is read twice or skipped;
    /// `next` reads the next page, and only of this query.
    pub fn list(&self, limit: u64, after: Option<&str>) -> Result<Page<T>> {
        let order = &self.parts.order;
        if order.is_empty() || order.iter().any(|each| each.column.is_none()) {
            return Err(Error::invalid(
                "a page is read by the query's order, of columns by their names: order_by first",
            ));
        }
        let mut whole = self.parts.clone();
        whole.limit = None;
        whole.offset = None;
        let digest = digest_of(&render(&whole)?);
        let mut parts = self.parts.clone();
        parts.limit = Some(limit);
        parts.offset = None;
        if let Some(after) = after {
            parts.conditions.push(keyset(order, &cursor_of(after, &digest)?));
        }
        let rows = self.runner.rows_of(&render(&parts)?, Wanted::All)?;
        let last = last_order_values(&rows, order)?;
        let full = rows.len() as u64 == limit;
        let decoded: Vec<T> = rows.nesting(nested(&self.parts)).decode()?;
        let next = if full && limit > 0 { last.map(|values| cursor_text(&values, &digest)) } else { None };
        Ok(Page { rows: decoded, next })
    }
}

/// The values of the order's columns in the last of `rows`.
fn last_order_values(rows: &Rows, order: &[Order]) -> Result<Option<Vec<Value>>> {
    if rows.len() == 0 {
        return Ok(None);
    }
    let width = rows.width();
    let start = (rows.len() - 1) * width;
    let mut values = Vec::with_capacity(order.len());
    for each in order {
        let column = each.column.as_deref().unwrap_or_default();
        let name = column.rsplit('.').next().unwrap_or(column);
        let at = rows.columns().iter().position(|have| have == name).ok_or_else(|| {
            Error::invalid(format!("the page is ordered by {column}, which its rows lack: select it"))
        })?;
        values.push(rows.value_at(start + at).clone());
    }
    Ok(Some(values))
}

fn refuse(parts: &mut Parts, why: String) {
    parts.refused.get_or_insert(why);
}

/// `"orders"` or `"orders as o"`: a name, and an alias after `as`.
fn table_ref(text: &str) -> std::result::Result<(String, Option<String>), String> {
    let words: Vec<&str> = text.split_whitespace().collect();
    let (name, alias) = match words.as_slice() {
        [name] => (*name, None),
        [name, alias] => (*name, Some(*alias)),
        [name, as_, alias] if as_.eq_ignore_ascii_case("as") => (*name, Some(*alias)),
        _ => return Err(format!("the table {text:?}: a table is its name, and an alias after as")),
    };
    let named = |part: &str| {
        part.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
    };
    let name_ok = name.split('.').count() <= 2 && name.split('.').all(named);
    if !name_ok || alias.is_some_and(|alias| !named(alias)) {
        return Err(format!("the table {text:?}: a table is its name, and an alias after as"));
    }
    Ok((name.to_owned(), alias.map(str::to_owned)))
}

fn table_text(name: &str, alias: Option<&str>) -> String {
    let name = quote(name).unwrap_or_default();
    match alias {
        Some(alias) => format!("{name} as {}", quote(alias).unwrap_or_default()),
        None => name,
    }
}

fn conditions_text(conditions: &[Sql]) -> Result<(String, Vec<Value>)> {
    let (text, values, refused) = joined_with(conditions.to_vec(), "and").into_parts();
    match refused {
        Some(why) => Err(Error::invalid(why)),
        None => Ok((text, values)),
    }
}

/// The query as one statement.
fn render(parts: &Parts) -> Result<Sql> {
    if let Some(why) = &parts.refused {
        return Err(Error::invalid(why.clone()));
    }
    let (columns, mut values) = selected(parts)?;
    let mut text = format!("select {columns} from {}", table_text(&parts.table, parts.alias.as_deref()));
    for join in &parts.joins {
        text.push(' ');
        text.push_str(join.text());
        values.extend(join.values().iter().cloned());
    }
    if !parts.conditions.is_empty() {
        let (condition, condition_values) = conditions_text(&parts.conditions)?;
        text.push_str(&format!(" where {condition}"));
        values.extend(condition_values);
    }
    if !parts.groups.is_empty() {
        text.push_str(&format!(" group by {}", parts.groups.join(", ")));
    }
    if !parts.having.is_empty() {
        let (condition, condition_values) = conditions_text(&parts.having)?;
        text.push_str(&format!(" having {condition}"));
        values.extend(condition_values);
    }
    push_order_and_bounds(parts, &mut text, &mut values)?;
    Ok(Sql::piece(text, values, false))
}

/// What the query selects: its own columns, then the rows of each query it
/// includes. A query that joins or includes reads its own table's columns
/// where it names none, since `*` would read every table's.
fn selected(parts: &Parts) -> Result<(String, Vec<Value>)> {
    let mut values: Vec<Value> = Vec::new();
    let mut columns = match &parts.select {
        Some(select) => {
            if let Some(why) = select.why_refused() {
                return Err(Error::invalid(why.to_owned()));
            }
            values.extend(select.values().iter().cloned());
            select.text().to_owned()
        }
        None if !parts.joins.is_empty() || !parts.includes.is_empty() => {
            format!("{}.*", quote(parts.alias.as_deref().unwrap_or(&parts.table)).unwrap_or_default())
        }
        None => "*".to_owned(),
    };
    for included in &parts.includes {
        let list = included_text(included)?;
        columns.push_str(&format!(", {} as {}", list.text(), quote(&included.name).map_err(Error::invalid)?));
        values.extend(list.values().iter().cloned());
    }
    Ok((columns, values))
}

fn push_order_and_bounds(parts: &Parts, text: &mut String, values: &mut Vec<Value>) -> Result<()> {
    if !parts.order.is_empty() {
        let orders: Vec<String> =
            parts.order.iter().map(|each| format!("{} {}", each.piece.text(), each.direction.text())).collect();
        text.push_str(&format!(" order by {}", orders.join(", ")));
        for each in &parts.order {
            if let Some(why) = each.piece.why_refused() {
                return Err(Error::invalid(why.to_owned()));
            }
            values.extend(each.piece.values().iter().cloned());
        }
    }
    if let Some(limit) = parts.limit {
        text.push_str(" limit cast(? as integer)");
        values.push(Value::Integer(i64::try_from(limit).unwrap_or(i64::MAX)));
    }
    if let Some(offset) = parts.offset {
        if parts.limit.is_none() {
            text.push_str(" limit -1");
        }
        text.push_str(" offset cast(? as integer)");
        values.push(Value::Integer(i64::try_from(offset).unwrap_or(i64::MAX)));
    }
    Ok(())
}

/// What an included query must be, and the names of its columns.
fn included(name: &str, query: &Parts, into: &Parts) -> std::result::Result<Included, String> {
    if let Some(why) = &query.refused {
        return Err(why.clone());
    }
    let Some(select) = &query.select else {
        return Err("it names its columns with select(\"p.id, p.title\")".to_owned());
    };
    if query.limit.is_none() {
        return Err("it needs a limit, which counts for each row".to_owned());
    }
    if !query.includes.is_empty() {
        return Err("an included query includes nothing itself".to_owned());
    }
    if name.is_empty() || into.includes.iter().any(|each| each.name == name) {
        return Err("an included list has a name of its own".to_owned());
    }
    Ok(Included { name: name.to_owned(), parts: query.clone(), columns: names_of(select.text())? })
}

/// An included query as one value of its row: its rows as SQL literals, in the
/// query's order, which the aggregate takes from columns the query gives it.
fn included_text(included: &Included) -> Result<Sql> {
    let mut child = included.parts.clone();
    let select = child.select.take().ok_or_else(|| Error::internal("an included query without its select"))?;
    let (mut text, mut values) = (select.text().to_owned(), select.values().to_vec());
    let mut ordered = Vec::new();
    for (at, each) in child.order.iter().enumerate() {
        text.push_str(&format!(", {} as \"$o{}\"", each.piece.text(), at + 1));
        values.extend(each.piece.values().iter().cloned());
        ordered.push(format!("\"$o{}\" {}", at + 1, each.direction.text()));
    }
    child.select = Some(Sql::piece(text, values, false));
    let inner = render(&child)?;
    let row: Vec<String> = included.columns.iter().map(|column| literal_of(column)).collect();
    let order = if ordered.is_empty() { String::new() } else { format!(" order by {}", ordered.join(", ")) };
    let text =
        format!("(select group_concat('(' || {} || ')', ','{order}) from ({}))", row.join(" || ',' || "), inner.text());
    Ok(Sql::piece(text, inner.values().to_vec(), false))
}

/// The columns of a query's rows that hold the rows of the queries it includes.
fn nested(parts: &Parts) -> Vec<Nested> {
    let list = |included: &Included| Nested { column: included.name.clone(), columns: included.columns.clone().into() };
    parts.includes.iter().map(list).collect()
}

/// An insert of rows of one shape, the first row's columns.
fn insert_of<T: Serialize>(table: &str, rows: &[T]) -> Result<Sql> {
    let mut columns: Option<Vec<String>> = None;
    let mut values = Vec::new();
    let mut tuples = Vec::with_capacity(rows.len());
    for (at, row) in rows.iter().enumerate() {
        let fields = row_of(row).map_err(|why| Error::invalid(format!("row {}: {why}", at + 1)))?;
        let names: Vec<String> = fields.iter().map(|(name, _)| name.clone()).collect();
        match &columns {
            None => columns = Some(names),
            Some(first) if *first != names => {
                return Err(Error::invalid(format!(
                    "row {}: not the first row's columns: insert rows of one shape",
                    at + 1
                )));
            }
            Some(_) => {}
        }
        tuples.push(format!("({})", vec!["?"; fields.len()].join(", ")));
        values.extend(fields.into_iter().map(|(_, value)| value));
    }
    let columns = columns.unwrap_or_default();
    if columns.is_empty() {
        return Err(Error::invalid("a row to insert with no column"));
    }
    let quoted =
        columns.iter().map(|name| quote(name)).collect::<std::result::Result<Vec<_>, _>>().map_err(Error::invalid)?;
    let table = quote(table).map_err(Error::invalid)?;
    let text = format!("insert into {table} ({}) values {}", quoted.join(", "), tuples.join(", "));
    Ok(Sql::piece(text, values, false))
}

/// The condition that starts a page after a row's order values.
fn keyset(order: &[Order], after: &[Value]) -> Sql {
    let mut alternatives = Vec::with_capacity(order.len());
    let mut values = Vec::new();
    for (at, each) in order.iter().enumerate() {
        let mut terms: Vec<String> =
            order[..at].iter().map(|earlier| format!("{} = ?", earlier.piece.text())).collect();
        values.extend(after[..at].iter().cloned());
        let past = if each.direction == Direction::Asc { ">" } else { "<" };
        terms.push(format!("{} {past} ?", each.piece.text()));
        values.push(after.get(at).cloned().unwrap_or(Value::Null));
        alternatives.push(if terms.len() == 1 { terms.remove(0) } else { format!("({})", terms.join(" and ")) });
    }
    Sql::piece(format!("({})", alternatives.join(" or ")), values, true)
}

/// A short digest of a query, which its cursors carry so that another query
/// refuses them.
fn digest_of(statement: &Sql) -> String {
    let mut hash: u32 = 0x811c_9dc5;
    let values: Vec<serde_json::Value> = statement.values().iter().map(json_of).collect();
    let text = format!("{}\u{0}{}", statement.text(), serde_json::Value::Array(values));
    for byte in text.bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    format!("{hash:08x}")
}

fn json_of(value: &Value) -> serde_json::Value {
    match value {
        Value::Null => serde_json::Value::Null,
        Value::Integer(n) => serde_json::Value::from(*n),
        Value::Real(x) => serde_json::Value::from(*x),
        Value::Text(text) => serde_json::Value::from(text.as_str()),
        Value::Blob(bytes) => serde_json::json!({ "bytes": bytes }),
    }
}

fn value_of(json: &serde_json::Value) -> Option<Value> {
    Some(match json {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Number(n) => {
            n.as_i64().map_or_else(|| Value::Real(n.as_f64().unwrap_or_default()), Value::Integer)
        }
        serde_json::Value::String(text) => Value::Text(text.clone()),
        serde_json::Value::Object(object) => Value::Blob(serde_json::from_value(object.get("bytes")?.clone()).ok()?),
        _ => return None,
    })
}

fn cursor_text(values: &[Value], digest: &str) -> String {
    let json = serde_json::json!({ "q": digest, "v": values.iter().map(json_of).collect::<Vec<_>>() });
    json.to_string().bytes().map(|byte| format!("{byte:02x}")).collect()
}

fn cursor_of(cursor: &str, digest: &str) -> Result<Vec<Value>> {
    let not_one = || Error::invalid("a cursor that is not one: pass the next of a page");
    let bytes = (0..cursor.len())
        .step_by(2)
        .map(|at| cursor.get(at..at + 2).and_then(|pair| u8::from_str_radix(pair, 16).ok()))
        .collect::<Option<Vec<u8>>>()
        .ok_or_else(not_one)?;
    let read: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| not_one())?;
    if read.get("q").and_then(serde_json::Value::as_str) != Some(digest) {
        return Err(Error::invalid("a cursor of another query: a page's next reads only the query that made it"));
    }
    let values = read.get("v").and_then(serde_json::Value::as_array).ok_or_else(not_one)?;
    values.iter().map(value_of).collect::<Option<Vec<_>>>().ok_or_else(not_one)
}
