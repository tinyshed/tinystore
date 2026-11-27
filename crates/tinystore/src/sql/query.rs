//! A table's queries, built from pieces into the SQL a person would write: the
//! builder adds the ands, the parentheses, the placeholders and the quotes
//! around names, and nothing else. The Bun SDK's builder writes the same text
//! from the same steps, which testdata/sql/queries.json holds both to.
//!
//! A query is its [`Parts`] and what runs them: each step changes the parts,
//! and each call that ends a query renders their statement and makes its
//! answer from the rows. The async API's queries hold the same parts and run
//! the same statements on the store's pool.

use std::marker::PhantomData;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde::de::DeserializeOwned;

use super::database::{Database, Done};
use super::each::Each;
use super::included::{literal_of, names_of};
use super::pieces::{checked, joined_with, quote, quote_all, quote_one};
use super::rows::{Nested, Rows};
use super::run::Wanted;
use super::statement::Sql;
use super::tx::Tx;
use super::values::{Value, row_of};
use super::walk::Key;
use crate::{Error, ErrorKind, Result};

/// What runs a table's statements: the database, or a transaction of it.
#[derive(Clone, Debug)]
enum Runner<'r> {
    Database(Database),
    Tx(&'r Tx<'r>),
    /// A test's runner, which keeps the last statement and answers no row, or
    /// a scalar's zero.
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

    /// Runs an edit of a table's rows: alone, as a write of a shared commit,
    /// or, with `if_version`, in a transaction with the read that tells why
    /// it changed no row, so that both see one state; on a copy of a node,
    /// which holds no transaction, passed on to its home as `exec` is.
    fn edit(&self, edit: Edit) -> Result<Done> {
        if !edit.expects() {
            return self.exec_of(edit.into_statement());
        }
        match self {
            Runner::Database(database) if database.file().is_copy() => edit.passed_on(database),
            Runner::Database(database) => database.tx(|tx| edit.run_in(tx)),
            Runner::Tx(tx) => edit.run_in(tx),
            #[cfg(test)]
            Runner::Recorder(_) => self.exec_of(edit.into_statement()),
        }
    }

    /// The database as an error names it: `sql app`.
    fn shown(&self) -> String {
        match self {
            Runner::Database(database) => database.shown(),
            Runner::Tx(tx) => tx.shown(),
            #[cfg(test)]
            Runner::Recorder(_) => "sql vectors".to_owned(),
        }
    }

    /// The store's clock in unix milliseconds, which a delete marks a row with.
    fn now_millis(&self) -> i64 {
        millis_of(match self {
            Runner::Database(database) => database.now(),
            Runner::Tx(tx) => tx.now(),
            #[cfg(test)]
            Runner::Recorder(_) => UNIX_EPOCH,
        })
    }
}

/// A time in unix milliseconds, as a deleted column keeps it.
pub(crate) fn millis_of(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH).map_or(0, |since| i64::try_from(since.as_millis()).unwrap_or(i64::MAX))
}

/// Which rows a table with a deleted column shows: the live ones, unless a
/// handle asks for the marked ones too, or for them alone.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Sees {
    #[default]
    Live,
    All,
    Deleted,
}

/// The columns a table's handle keeps for its rows, from [`Table::deleted`]
/// and [`Table::version`], and which rows it shows. A handle with neither
/// writes the statements a plain one does.
#[derive(Clone, Debug, Default)]
pub(crate) struct Tracked {
    deleted: Option<String>,
    version: Option<String>,
    sees: Sees,
}

/// A write of a table's rows: its statement, and, with `expect`, what tells
/// a row at another version from no row at all when it changed none.
#[derive(Debug)]
pub(crate) struct Edit {
    statement: Sql,
    expected: Option<Expected>,
}

#[derive(Debug)]
struct Expected {
    table: String,
    version: i64,
    /// The version of a row the edit's conditions name that is not `version`.
    other: Sql,
}

impl Edit {
    /// Whether the edit has `expect`, and runs in a transaction with
    /// [`Edit::run_in`]; one without it is its statement, run alone.
    pub(crate) fn expects(&self) -> bool {
        self.expected.is_some()
    }

    pub(crate) fn into_statement(self) -> Sql {
        self.statement
    }

    /// Runs the edit inside `tx`: its statement, which writes every row its
    /// conditions name or none, and after one that changed none, the read
    /// that tells a row at another version, a conflict, from no row.
    pub(crate) fn run_in(self, tx: &Tx<'_>) -> Result<Done> {
        let done = tx.exec(self.statement)?;
        let Some(expected) = self.expected else { return Ok(done) };
        if done.changes > 0 {
            return Ok(done);
        }
        check.answer(done, tx.one::<i64>(check.other.clone())?, &tx.shown())
    }

    /// Runs the edit on a copy of a node: its statement passed on to the
    /// file's home, as `exec` passes one, and after one that changed none,
    /// the read that tells why on this copy, which holds the home's commit
    /// by then. The two are one state only as nearly as the copy follows its
    /// home: an edit at the home between them may be read as a conflict
    /// where the statement found no row.
    pub(crate) fn passed_on(self, database: &Database) -> Result<Done> {
        let done = database.exec(self.statement)?;
        let Some(check) = self.check else { return Ok(done) };
        if done.changes > 0 {
            return Ok(done);
        }
        check.answer(done, database.one::<i64>(check.other.clone())?, &database.shown())
    }
}

impl VersionCheck {
    /// An edit that changed no row: no conflict when no row its conditions
    /// name is at another version, `found`, and one naming it otherwise.
    fn answer(&self, done: Done, found: Option<i64>, database: &str) -> Result<Done> {
        let Some(at) = found else { return Ok(done) };
        let why = format!("table {}: the row is at version {at}, not {}", self.table, self.version);
        Err(Error::new(ErrorKind::Conflict, why).within(database))
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

    /// Refuses an upsert that names no key or no column to update, as every
    /// SDK does before anything is sent.
    pub(crate) fn check(&self) -> Result<()> {
        if self.conflict.is_empty() {
            return Err(Error::invalid("an upsert names its conflict columns, the key"));
        }
        if self.update.is_empty() {
            return Err(Error::invalid("an upsert names the columns its update sets"));
        }
        Ok(())
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

/// What a query is made of, whatever runs it: each step changes a copy, and
/// a refused piece is kept to fail the call that runs the query.
#[derive(Clone, Debug)]
pub(crate) struct Parts {
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
    tracked: Tracked,
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
///
/// A table may keep two columns of its rows for the program: `deleted`, which
/// marks a row deleted rather than deleting it, so that every query and write
/// of the handle passes it by, and `version`, which every update raises, so
/// that an edit made from an old version is refused rather than written over
/// another's. A statement written by hand is not rewritten: they are the
/// handle's.
///
/// ```no_run
/// # use tinystore::{ErrorKind, sql};
/// # #[derive(serde::Serialize, serde::Deserialize)] struct Task { id: i64, title: String }
/// # fn main() -> tinystore::Result<()> {
/// # let db = tinystore::Store::open("data", Default::default())?.database("app").open()?;
/// let tasks = db.table::<Task>("tasks").deleted("deleted_at").version("version");
/// let edit = tasks.filter(sql::eq("id", &1)).expect(0).update([("title", sql::value("from b"))]);
/// if edit.as_ref().is_err_and(|error| error.kind() == ErrorKind::Conflict) {
///     // sql app: table tasks: the row is at version 1, not 0
/// }
/// tasks.filter(sql::eq("id", &1)).delete()?; // marks deleted_at; no query of tasks finds it
/// tasks.deleted_only().filter(sql::eq("id", &1)).restore()?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Debug)]
pub struct Table<'r, T> {
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
        Table { query: Query { runner, parts: Parts::of_table(name), row: PhantomData } }
    }

    pub fn name(&self) -> &str {
        self.query.parts.table()
    }

    /// Marks a row deleted rather than deleting it: `column`, a nullable
    /// integer, holds when, in unix milliseconds of the store's clock. Every
    /// query and write of the handle passes the marked rows by, unless
    /// [`Query::with_deleted`] or [`Query::deleted_only`] asks for them;
    /// `delete` marks, [`Query::restore`] clears the mark, and
    /// [`Query::purge`] deletes for good. A marked row is still in the file
    /// and holds its key, so an insert of that key fails `Conflict`; an
    /// upsert whose key a marked row holds updates that row and clears its
    /// mark: the row came back.
    #[must_use]
    pub fn deleted(mut self, column: &str) -> Self {
        self.query.parts.track_deleted(column);
        self
    }

    /// Counts a row's versions in `column`, an integer, not null, default 0:
    /// every update, delete, restore and upsert of the handle raises it by
    /// one, and an edit after [`Query::if_version`] writes only rows at the
    /// version it names. The handle sets the column itself: an update or an
    /// upsert's update that names it is `Invalid`, and an insert starts a row
    /// at version 0 whatever the row gives for it.
    #[must_use]
    pub fn version(mut self, column: &str) -> Self {
        self.query.parts.track_version(column);
        self
    }

    /// A table whose statements a test reads rather than runs.
    #[cfg(test)]
    pub(crate) fn recorded(name: &str, last: std::sync::Arc<std::sync::Mutex<Option<Sql>>>) -> Self {
        Table::new(Runner::Recorder(last), name)
    }
}

impl<T: Serialize + DeserializeOwned> Table<'_, T> {
    /// Inserts a row, every field it has, and gives it back as the file keeps
    /// it: the defaults and the generated columns the database filled. On a
    /// versioned table the row starts at version 0, whatever it gives for
    /// the column, since the version is the handle's to count.
    pub fn insert(&self, row: &T) -> Result<T> {
        let mut rows = self.insert_all(std::slice::from_ref(row))?;
        rows.pop().ok_or_else(|| Error::internal("an insert that gave back no row"))
    }

    /// Inserts rows of one shape as one write, all of them or none.
    pub fn insert_all(&self, rows: &[T]) -> Result<Vec<T>> {
        if rows.is_empty() {
            return Ok(Vec::new());
        }
        self.query.runner.rows_of(&insert_sql(&self.query.parts, rows)?, Wanted::All)?.decode()
    }

    /// Inserts a row, or, when its key is taken, sets the columns `update`
    /// names on the row that holds it, if that row meets the upsert's
    /// condition; `None` when the condition kept it. On a table with a
    /// deleted column the update clears a mark, and on a versioned one it
    /// raises the version.
    pub fn upsert(&self, row: &T, upsert: Upsert) -> Result<Option<T>> {
        let statement = self.query.built(upsert_sql(&self.query.parts, std::slice::from_ref(row), upsert))?;
        let rows: Vec<T> = self.query.runner.rows_of(&statement, Wanted::One)?.decode()?;
        Ok(rows.into_iter().next())
    }

    /// Upserts rows of one shape as one statement, all of them or none, each
    /// as [`Table::upsert`] does one. It gives back the rows it wrote as the
    /// file keeps them, without a row whose key is held by a row the
    /// condition kept; SQLite refuses a list that holds one key twice.
    pub fn upsert_all(&self, rows: &[T], upsert: Upsert) -> Result<Vec<T>> {
        upsert.check()?;
        if rows.is_empty() {
            return Ok(Vec::new());
        }
        let statement = self.query.built(upsert_sql(&self.query.parts, rows, upsert))?;
        self.query.runner.rows_of(&statement, Wanted::All)?.decode()
    }
}

impl<'r, T> Query<'r, T> {
    /// Adds a condition with `and`: a piece from `sql!`, `sql::eq`, `sql::or`
    /// and the rest. `filter`, since `where` is Rust's own word.
    #[must_use]
    pub fn filter(&self, condition: impl Into<Sql>) -> Self {
        self.with(|parts| parts.filter(condition.into()))
    }

    /// The columns a row holds, as SQL text, and the shape of the row they make.
    pub fn select<S>(&self, columns: impl Into<Sql>) -> Query<'r, S> {
        let mut parts = self.parts.clone();
        parts.select(columns.into());
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
        self.with(|parts| parts.include(name, &query.parts))
    }

    /// Joins a table, `"users as u"`, on a condition.
    #[must_use]
    pub fn join(&self, table: &str, on: impl Into<Sql>) -> Self {
        self.with(|parts| parts.join(table, on.into()))
    }

    /// Joins a table whose columns may be null where no row of it meets the condition.
    #[must_use]
    pub fn left_join(&self, table: &str, on: impl Into<Sql>) -> Self {
        self.with(|parts| parts.left_join(table, on.into()))
    }

    /// Groups the rows by columns, quoted as names.
    #[must_use]
    pub fn group_by<I: IntoIterator<Item = S>, S: AsRef<str>>(&self, columns: I) -> Self {
        self.with(|parts| parts.group_by(columns))
    }

    /// A condition on the groups, as `filter` takes one.
    #[must_use]
    pub fn having(&self, condition: impl Into<Sql>) -> Self {
        self.with(|parts| parts.having(condition.into()))
    }

    /// Orders the rows by a column, quoted as a name, so that a sort a
    /// request chooses cannot be SQL.
    #[must_use]
    pub fn order_by(&self, column: &str, direction: Direction) -> Self {
        self.with(|parts| parts.order_by(column, direction))
    }

    /// Orders the rows by a piece of SQL: `sql!("lower(title)")`.
    #[must_use]
    pub fn order_by_sql(&self, piece: impl Into<Sql>, direction: Direction) -> Self {
        self.with(|parts| parts.order_by_sql(piece.into(), direction))
    }

    #[must_use]
    pub fn limit(&self, rows: u64) -> Self {
        self.with(|parts| parts.limit(rows))
    }

    #[must_use]
    pub fn offset(&self, rows: u64) -> Self {
        self.with(|parts| parts.offset(rows))
    }

    /// The text and the values the query sends, to read or to log.
    pub fn to_sql(&self) -> Result<Sql> {
        self.parts.to_sql()
    }

    /// How many rows the conditions match, or how many groups with
    /// `group_by`, without the order and the limit.
    pub fn count(&self) -> Result<u64> {
        self.runner.rows_of(&self.parts.count_sql()?, Wanted::Scalar)?.scalar()
    }

    /// SQLite's plan for the query, a line a step.
    pub fn explain(&self) -> Result<Vec<String>> {
        plan_of(self.runner.rows_of(&self.parts.explain_sql()?, Wanted::All)?)
    }

    /// Sets columns of the rows the conditions match, each a value from
    /// `sql::value` or a piece of SQL, and says how many changed. A versioned
    /// table's version goes up by one.
    pub fn update<I, S>(&self, sets: I) -> Result<Done>
    where
        I: IntoIterator<Item = (S, Sql)>,
        S: AsRef<str>,
    {
        self.runner.edit(self.built(self.parts.update_sql(sets, None))?)
    }

    /// Deletes the rows the conditions match, and says how many it deleted.
    /// On a table with a deleted column it marks the live ones instead, with
    /// the store's clock, and raises a versioned table's version.
    pub fn delete(&self) -> Result<Done> {
        self.runner.edit(self.built(self.parts.delete_sql(|| self.runner.now_millis(), None))?)
    }

    /// The query of every row, those the deleted column marks among them.
    #[must_use]
    pub fn with_deleted(&self) -> Self {
        self.with(|parts| parts.sees(Sees::All))
    }

    /// The query of the rows the deleted column marks, and of no other.
    #[must_use]
    pub fn deleted_only(&self) -> Self {
        self.with(|parts| parts.sees(Sees::Deleted))
    }

    /// Clears the mark of the marked rows the conditions match, whichever
    /// rows the handle shows, and says how many came back; a versioned
    /// table's version goes up by one.
    pub fn restore(&self) -> Result<Done> {
        self.runner.edit(self.built(self.parts.restore_sql())?)
    }

    /// Deletes for good the rows the conditions match of those the handle
    /// shows: the marked ones of [`Query::deleted_only`], or every one of
    /// [`Query::with_deleted`]. A handle that shows the live rows alone
    /// refuses it, `Invalid`, so that nothing live is purged by a handle
    /// that forgot to ask.
    pub fn purge(&self) -> Result<Done> {
        self.runner.exec_of(self.built(self.parts.purge_sql())?)
    }

    /// The edits of a versioned table made from `version`: they write only
    /// when every row the conditions match is at it, all of those rows, and
    /// otherwise nothing, failing `Conflict` with the version a row is at.
    /// No row at all is no conflict: the edit changes none.
    pub fn expect(&self, version: i64) -> Expecting<'r, T> {
        Expecting { query: self.with(|_| {}), version }
    }

    /// What building a statement refused, named by the database on a
    /// handle that keeps a column, whose refusals name their table.
    fn built<R>(&self, built: Result<R>) -> Result<R> {
        if self.parts.tracks() { built.map_err(|error| error.within(self.runner.shown())) } else { built }
    }

    fn with(&self, change: impl FnOnce(&mut Parts)) -> Self {
        let mut parts = self.parts.clone();
        change(&mut parts);
        Query { runner: self.runner.clone(), parts, row: PhantomData }
    }
}

/// The edits of a query made from a version a program saw, from
/// [`Query::expect`]: `tasks.filter(sql::eq("id", &1)).expect(3).update(…)`.
#[derive(Clone, Debug)]
#[must_use = "an expect is an edit's: update or delete"]
pub struct Expecting<'r, T> {
    query: Query<'r, T>,
    version: i64,
}

impl<T> Expecting<'_, T> {
    /// [`Query::update`] of the rows at the version, all of them or none.
    pub fn update<I, S>(&self, sets: I) -> Result<Done>
    where
        I: IntoIterator<Item = (S, Sql)>,
        S: AsRef<str>,
    {
        let query = &self.query;
        query.runner.edit(query.built(query.parts.update_sql(sets, Some(self.version)))?)
    }

    /// [`Query::delete`] of the rows at the version, all of them or none: a
    /// mark, on a table with a deleted column, which a delete for good is not.
    pub fn delete(&self) -> Result<Done> {
        let query = &self.query;
        query.runner.edit(query.built(query.parts.delete_sql(|| query.runner.now_millis(), Some(self.version)))?)
    }
}

impl<T: DeserializeOwned> Query<'_, T> {
    pub fn all(&self) -> Result<Vec<T>> {
        self.runner.rows_of(&self.parts.to_sql()?, Wanted::All)?.nesting(self.parts.nested()).decode()
    }

    pub fn one(&self) -> Result<Option<T>> {
        let rows = self.runner.rows_of(&self.parts.to_sql()?, Wanted::One)?.nesting(self.parts.nested());
        Ok(rows.decode::<T>()?.into_iter().next())
    }

    pub fn scalar<V: DeserializeOwned>(&self) -> Result<V> {
        self.runner.rows_of(&self.parts.to_sql()?, Wanted::Scalar)?.scalar()
    }

    /// The rows one at a time, read a part at a time from one snapshot, so
    /// that memory holds a part and not the rows: for more than fits in
    /// memory. A snapshot lasts five seconds at most. Ordered by columns by
    /// their names, a unique one last, as `list` is, the walk reads on past
    /// that in a snapshot of its own, after the last row it gave, which sees
    /// the rows as they are then; any other walk fails `limit` there. Inside
    /// a transaction it gives the rows `all` reads.
    ///
    /// ```no_run
    /// # use tinystore::sql::Direction;
    /// # #[derive(serde::Deserialize)] struct Order { id: i64 }
    /// # fn main() -> tinystore::Result<()> {
    /// # let db = tinystore::Store::open("data", Default::default())?.database("app").open()?;
    /// for order in db.table::<Order>("orders").order_by("id", Direction::Asc).each()? {
    ///     let order = order?;
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn each(&self) -> Result<Each<T>> {
        let statement = self.parts.to_sql()?;
        let nested: Arc<[Nested]> = self.parts.nested().into();
        match &self.runner {
            Runner::Database(database) => Each::walk(database, statement, self.parts.walk_key(), nested),
            runner => Ok(Each::read(runner.rows_of(&statement, Wanted::All)?, nested, |error| error)),
        }
    }

    /// A page after the last row of the one before, by the query's order,
    /// which ends in a unique column so that no row is read twice or skipped;
    /// `next` reads the next page, and only of this query.
    pub fn list(&self, limit: u64, after: Option<&str>) -> Result<Page<T>> {
        let (statement, paging) = self.parts.page_sql(limit, after)?;
        paging.page(self.runner.rows_of(&statement, Wanted::All)?)
    }
}

impl Parts {
    /// The parts of the query of every row of the table `name`, `"orders"` or
    /// `"orders as o"`; a name that is not one fails the query when it runs.
    pub(crate) fn of_table(name: &str) -> Parts {
        let (table, alias, refused) = match table_ref(name) {
            Ok((table, alias)) => (table, alias, None),
            Err(why) => (name.to_owned(), None, Some(why)),
        };
        Parts {
            table,
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
            tracked: Tracked::default(),
            refused,
        }
    }

    pub(crate) fn table(&self) -> &str {
        &self.table
    }

    pub(crate) fn track_deleted(&mut self, column: &str) {
        match quote_one(column) {
            Ok(_) => self.tracked.deleted = Some(column.to_owned()),
            Err(why) => refuse(self, why),
        }
    }

    pub(crate) fn track_version(&mut self, column: &str) {
        match quote_one(column) {
            Ok(_) => self.tracked.version = Some(column.to_owned()),
            Err(why) => refuse(self, why),
        }
    }

    pub(crate) fn sees(&mut self, sees: Sees) {
        if self.tracked.deleted.is_none() {
            let why = format!("table {}: only a table with a deleted column has marked rows to show", self.table);
            return refuse(self, why);
        }
        self.tracked.sees = sees;
    }

    /// Whether the handle keeps a deleted or a version column.
    pub(crate) fn tracks(&self) -> bool {
        self.tracked.deleted.is_some() || self.tracked.version.is_some()
    }

    fn refusal(&self, why: &str) -> Error {
        Error::invalid(format!("table {}: {why}", self.table))
    }

    pub(crate) fn filter(&mut self, condition: Sql) {
        self.conditions.push(checked(condition));
    }

    pub(crate) fn select(&mut self, columns: Sql) {
        self.select = Some(checked(columns));
    }

    pub(crate) fn include(&mut self, name: &str, query: &Parts) {
        match included(name, query, self) {
            Ok(included) => self.includes.push(included),
            Err(why) => refuse(self, format!("the included {name:?}: {why}")),
        }
    }

    pub(crate) fn join(&mut self, table: &str, on: Sql) {
        self.joined("join", table, on);
    }

    pub(crate) fn left_join(&mut self, table: &str, on: Sql) {
        self.joined("left join", table, on);
    }

    pub(crate) fn group_by<I: IntoIterator<Item = S>, S: AsRef<str>>(&mut self, columns: I) {
        for column in columns {
            match quote(column.as_ref()) {
                Ok(column) => self.groups.push(column),
                Err(why) => refuse(self, why),
            }
        }
    }

    pub(crate) fn having(&mut self, condition: Sql) {
        self.having.push(checked(condition));
    }

    pub(crate) fn order_by(&mut self, column: &str, direction: Direction) {
        match quote(column) {
            Ok(quoted) => self.order.push(Order {
                piece: Sql::piece(quoted, Vec::new(), true),
                column: Some(column.to_owned()),
                direction,
            }),
            Err(why) => refuse(self, why),
        }
    }

    pub(crate) fn order_by_sql(&mut self, piece: Sql, direction: Direction) {
        self.order.push(Order { piece: checked(piece), column: None, direction });
    }

    pub(crate) fn limit(&mut self, rows: u64) {
        self.limit = Some(rows);
    }

    pub(crate) fn offset(&mut self, rows: u64) {
        self.offset = Some(rows);
    }

    /// The query as one statement: what `all`, `one` and `scalar` run.
    pub(crate) fn to_sql(&self) -> Result<Sql> {
        render(self)
    }

    /// The columns of the query's rows that hold the rows of the queries it
    /// includes, which its rows are decoded with.
    pub(crate) fn nested(&self) -> Vec<Nested> {
        let list =
            |included: &Included| Nested { column: included.name.clone(), columns: included.columns.clone().into() };
        self.includes.iter().map(list).collect()
    }

    /// What `count` runs: the rows the conditions match, or the groups,
    /// without the order and the limit.
    pub(crate) fn count_sql(&self) -> Result<Sql> {
        let mut counted = self.clone();
        counted.select = Some(Sql::new("count(*)"));
        counted.order.clear();
        counted.limit = None;
        counted.offset = None;
        counted.includes.clear();
        if counted.groups.is_empty() {
            return render(&counted);
        }
        counted.select = Some(Sql::new("1"));
        let (inner, values, _) = render(&counted)?.into_parts();
        Ok(Sql::piece(format!("select count(*) from ({inner})"), values, false))
    }

    /// What `explain` runs: SQLite's plan of the query, which [`plan_of`]
    /// reads.
    pub(crate) fn explain_sql(&self) -> Result<Sql> {
        let (text, values, _) = render(self)?.into_parts();
        Ok(Sql::piece(format!("explain query plan {text}"), values, false))
    }

    /// What `list` runs for the page of `limit` rows after the cursor
    /// `after`, and what makes that page of the rows it gives.
    pub(crate) fn page_sql(&self, limit: u64, after: Option<&str>) -> Result<(Sql, Paging)> {
        let order = &self.order;
        if order.is_empty() || order.iter().any(|each| each.column.is_none()) {
            return Err(Error::invalid(
                "a page is read by the query's order, of columns by their names: order_by first",
            ));
        }
        // A page reads `limit` rows after its cursor: the query's own limit or
        // offset would be dropped, and `limit(100)` page through every row.
        if self.limit.is_some() || self.offset.is_some() {
            return Err(Error::invalid(
                "list after a limit or an offset: list pages the rows itself and takes its own limit",
            ));
        }
        let digest = digest_of(&render(self)?);
        let mut parts = self.clone();
        parts.limit = Some(limit);
        if let Some(after) = after {
            let values = cursor_of(after, &digest)?;
            // A cursor is anyone's to send: one short of the order's values is no page's.
            if values.len() != order.len() {
                return Err(Error::invalid("a cursor that is not one: pass the next of a page"));
            }
            parts.conditions.push(keyset(order, &values));
        }
        let paging = Paging { order: order.clone(), limit, digest, nested: self.nested() };
        Ok((render(&parts)?, paging))
    }

    /// The columns the query's rows are ordered by, as its rows name them,
    /// when each is a column by its name: a walk of them reads on past one
    /// snapshot by them, as `list` pages by them.
    pub(crate) fn walk_key(&self) -> Option<Vec<Key>> {
        if self.order.is_empty() {
            return None;
        }
        let key = |order: &Order| {
            let column = order.column.as_deref()?;
            Some(Key { column: column.rsplit('.').next().unwrap_or(column).to_owned(), direction: order.direction })
        };
        self.order.iter().map(key).collect()
    }

    /// What `update` runs: the columns `sets` names set on the rows the
    /// conditions match, of those the handle shows.
    pub(crate) fn update_sql<I, S>(&self, sets: I, expect: Option<i64>) -> Result<Edit>
    where
        I: IntoIterator<Item = (S, Sql)>,
        S: AsRef<str>,
    {
        self.refuse_every_row("an update")?;
        let mut texts = Vec::new();
        let mut values = Vec::new();
        for (column, value) in sets {
            if self.tracked.version.as_deref() == Some(column.as_ref()) {
                return Err(self.refusal("the version column is the handle's to raise: set the others"));
            }
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
        self.edit(texts, values, self.tracked.sees, expect)
    }

    /// What `delete` runs: a delete of the rows the conditions match, or,
    /// on a table with a deleted column, a mark of the live ones at `now`.
    pub(crate) fn delete_sql(&self, now: impl FnOnce() -> i64, expect: Option<i64>) -> Result<Edit> {
        self.refuse_every_row("a delete")?;
        let Some(deleted) = &self.tracked.deleted else {
            if expect.is_some() {
                return Err(self.refusal("expect on a delete is a mark's: open the table with a deleted column"));
            }
            let (table, condition, values) = self.target(None)?;
            let statement = Sql::piece(format!("delete from {table} where {condition}"), values, false);
            return Ok(Edit { statement, expected: None });
        };
        let mark = format!("{} = ?", quote_one(deleted).map_err(Error::invalid)?);
        self.edit(vec![mark], vec![Value::Integer(now())], Sees::Live, if_version)
    }

    /// What `restore` runs: the mark cleared of the marked rows the
    /// conditions match.
    pub(crate) fn restore_sql(&self) -> Result<Edit> {
        self.refuse_every_row("a restore")?;
        let Some(deleted) = &self.tracked.deleted else {
            return Err(self.refusal("a restore clears a mark: open the table with a deleted column"));
        };
        let cleared = format!("{} = null", quote_one(deleted).map_err(Error::invalid)?);
        self.edit(vec![cleared], Vec::new(), Sees::Deleted, None)
    }

    /// What `purge` runs: a delete for good of the rows the conditions match
    /// of those the handle shows, which are never the live ones alone.
    pub(crate) fn purge_sql(&self) -> Result<Sql> {
        self.refuse_every_row("a purge")?;
        if self.tracked.sees == Sees::Live {
            return Err(self.refusal(
                "a purge deletes for good, and only what deleted_only() or with_deleted() shows: ask one first",
            ));
        }
        let (table, condition, values) = self.target(self.mark(self.tracked.sees, None))?;
        Ok(Sql::piece(format!("delete from {table} where {condition}"), values, false))
    }

    /// An update of the rows `sees` shows of those the conditions match,
    /// which `update`, a marking `delete` and `restore` are. A versioned
    /// table's version goes up by one; with `expect`, the update writes only
    /// when no row the conditions match is at another version, so that one
    /// statement writes every row or none:
    ///
    /// ```text
    /// update "tasks" set "title" = ?, "version" = "version" + 1
    ///   where "id" = ? and "deleted_at" is null and "version" = ?
    ///   and not exists (select 1 from "tasks" where "id" = ? and "deleted_at" is null and "version" is not ?)
    /// ```
    fn edit(&self, mut sets: Vec<String>, mut values: Vec<Value>, sees: Sees, if_version: Option<i64>) -> Result<Edit> {
        let version = self.tracked.version.as_deref().map(quote_one).transpose().map_err(Error::invalid)?;
        if let Some(version) = &version {
            sets.push(format!("{version} = {version} + 1"));
        }
        let mark = self.mark(sees, None);
        let Some(expect) = expect else {
            let (table, condition, condition_values) = self.target(mark)?;
            values.extend(condition_values);
            let statement =
                Sql::piece(format!("update {table} set {} where {condition}", sets.join(", ")), values, false);
            return Ok(Edit { statement, expected: None });
        };
        let Some(version) = version else {
            return Err(self.refusal("expect is a versioned table's: open the table with a version column"));
        };
        let table = quote(&self.table).map_err(Error::invalid)?;
        let named = self.conditions.iter().cloned().chain(mark);
        let at = |test: &str| Sql::piece(format!("{version} {test} ?"), vec![Value::Integer(expect)], true);
        let (other, other_values) = conditions_text(&named.clone().chain([at("is not")]).collect::<Vec<_>>())?;
        let guard = format!("not exists (select 1 from {table} where {other})");
        let guarded = named.chain([at("="), Sql::piece(guard, other_values.clone(), true)]).collect::<Vec<_>>();
        let (condition, condition_values) = conditions_text(&guarded)?;
        values.extend(condition_values);
        let statement = Sql::piece(format!("update {table} set {} where {condition}", sets.join(", ")), values, false);
        let other = Sql::piece(format!("select {version} from {table} where {other} limit 1"), other_values, false);
        Ok(Edit { statement, expected: Some(Expected { table: self.table.clone(), version: expect, other }) })
    }

    /// The condition that shows the rows `sees` asks for of a table with a
    /// deleted column, its column named by `owner`, the alias of a read;
    /// none on a table without one, or for every row.
    fn mark(&self, sees: Sees, owner: Option<&str>) -> Option<Sql> {
        let column = self.tracked.deleted.as_deref()?;
        let test = match sees {
            Sees::Live => "is null",
            Sees::Deleted => "is not null",
            Sees::All => return None,
        };
        // the column is the table's own, a dot in its name its own too
        let column = quote_one(column).unwrap_or_default();
        let named =
            owner.map_or_else(|| column.clone(), |owner| format!("{}.{column}", quote(owner).unwrap_or_default()));
        Some(Sql::piece(format!("{named} {test}"), Vec::new(), true))
    }

    /// The table an update or a delete writes, quoted, and the condition
    /// that picks its rows, `mark` last, with the condition's values.
    fn target(&self, mark: Option<Sql>) -> Result<(String, String, Vec<Value>)> {
        let (condition, values) = match mark {
            None => conditions_text(&self.conditions)?,
            Some(mark) => conditions_text(&self.conditions.iter().cloned().chain([mark]).collect::<Vec<_>>())?,
        };
        Ok((quote(&self.table).map_err(Error::invalid)?, condition, values))
    }

    fn joined(&mut self, kind: &str, table: &str, on: Sql) {
        let joined = table_ref(table).map(|(name, alias)| table_text(&name, alias.as_deref()));
        match joined {
            Ok(table) => {
                let (text, values, refused) = checked(on).into_parts();
                if let Some(why) = refused {
                    return refuse(self, why);
                }
                self.joins.push(Sql::piece(format!("{kind} {table} on {text}"), values, false));
            }
            Err(why) => refuse(self, why),
        }
    }

    /// Refuses an update or a delete of every row, which SQL says plainly.
    fn refuse_every_row(&self, what: &str) -> Result<()> {
        if let Some(why) = &self.refused {
            return Err(Error::invalid(why.clone()));
        }
        if self.conditions.iter().all(|condition| condition.text() == "1") {
            return Err(Error::invalid(format!("{what} without a condition: every row is said in SQL, delete from …")));
        }
        if !self.joins.is_empty() {
            return Err(Error::invalid(format!("{what} of a join: write it in SQL")));
        }
        // SQLite's update and delete take none of these: `limit(10).delete()`
        // would delete every row the conditions match.
        let shaped = self.limit.is_some()
            || self.offset.is_some()
            || !self.order.is_empty()
            || !self.groups.is_empty()
            || !self.having.is_empty()
            || self.select.is_some()
            || !self.includes.is_empty();
        if shaped {
            return Err(Error::invalid(format!(
                "{what} with a limit, offset, order, group, select or include: narrow it by conditions, or write it in SQL"
            )));
        }
        Ok(())
    }
}

/// What makes a page of the rows a page's statement gave: the rows decoded,
/// and the cursor of the next page from the last row's order values when the
/// page is full.
pub(crate) struct Paging {
    order: Vec<Order>,
    limit: u64,
    digest: String,
    nested: Vec<Nested>,
}

impl Paging {
    pub(crate) fn page<T: DeserializeOwned>(self, rows: Rows) -> Result<Page<T>> {
        let last = last_order_values(&rows, &self.order)?;
        let full = rows.len() as u64 == self.limit;
        let decoded: Vec<T> = rows.nesting(self.nested).decode()?;
        let next = if full && self.limit > 0 { last.map(|values| cursor_text(&values, &self.digest)) } else { None };
        Ok(Page { rows: decoded, next })
    }
}

/// A walk's statement read on after the row whose key is `after`: its rows
/// past that row in its order, as a page after a cursor reads them, whatever
/// limit, offset or include the statement has.
///
/// ```text
/// select * from (select * from "orders" order by "id" asc) where "id" > ? order by "id" asc
/// ```
pub(crate) fn read_on(statement: &Sql, key: &[Key], after: &[Value]) -> Result<Sql> {
    let ordered = |key: &Key| {
        let piece = Sql::piece(quote(&key.column).map_err(Error::invalid)?, Vec::new(), true);
        Ok(Order { piece, column: Some(key.column.clone()), direction: key.direction })
    };
    let order = key.iter().map(ordered).collect::<Result<Vec<_>>>()?;
    let past = keyset(&order, after);
    let by: Vec<String> = order.iter().map(|each| format!("{} {}", each.piece.text(), each.direction.text())).collect();
    let text = format!("select * from ({}) where {} order by {}", statement.text(), past.text(), by.join(", "));
    let values = statement.values().iter().chain(past.values()).cloned().collect();
    Ok(Sql::piece(text, values, false))
}

/// What `insert` and `insert_all` run: an insert of rows of one shape that
/// gives each back as the file keeps it.
pub(crate) fn insert_sql<T: Serialize>(parts: &Parts, rows: &[T]) -> Result<Sql> {
    let (text, values, _) = insert_of(parts, rows)?.into_parts();
    Ok(Sql::piece(format!("{text} returning *"), values, false))
}

/// What `upsert` and `upsert_all` run: an insert of `rows` that, when a
/// row's key is taken, sets the columns the upsert names on the row that
/// holds it, and gives back the rows it wrote, without those the upsert's
/// condition kept. The update clears a deleted column's mark, since the row
/// came back, and raises a version column.
pub(crate) fn upsert_sql<T: Serialize>(parts: &Parts, rows: &[T], upsert: Upsert) -> Result<Sql> {
    upsert.check()?;
    let tracked = &parts.tracked;
    if let Some(version) = &tracked.version
        && upsert.update.contains(version)
    {
        return Err(parts.refusal("the version column is the handle's to raise: update the others"));
    }
    let insert = insert_of(parts, rows)?;
    let (insert_text, mut values, _) = insert.into_parts();
    let conflict = quote_all(&upsert.conflict)?;
    let mut sets = upsert
        .update
        .iter()
        .map(|name| quote(name).map(|quoted| format!("{quoted} = excluded.{quoted}")))
        .collect::<Result<Vec<_>, _>>()
        .map_err(Error::invalid)?;
    if let Some(deleted) = &tracked.deleted {
        sets.push(format!("{} = null", quote_one(deleted).map_err(Error::invalid)?));
    }
    if let Some(version) = &tracked.version {
        let version = quote_one(version).map_err(Error::invalid)?;
        sets.push(format!("{version} = {version} + 1"));
    }
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
    Ok(Sql::piece(format!("{text} returning *"), values, false))
}

/// SQLite's plan as `explain query plan` gives it: a line a step.
pub(crate) fn plan_of(rows: Rows) -> Result<Vec<String>> {
    let rows: Vec<(i64, i64, i64, String)> = rows.decode()?;
    Ok(rows.into_iter().map(|(_, _, _, detail)| detail).collect())
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

/// `"orders"`, `"orders as o"` or `"orders o"`: a name, and an alias after
/// it, with `as` or without.
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
    // a join's columns of the same name would make the bare column ambiguous
    let owner = parts.alias.as_deref().or((!parts.joins.is_empty()).then_some(parts.table.as_str()));
    match parts.mark(parts.tracked.sees, owner) {
        None if parts.conditions.is_empty() => {}
        None => {
            let (condition, condition_values) = conditions_text(&parts.conditions)?;
            text.push_str(&format!(" where {condition}"));
            values.extend(condition_values);
        }
        Some(mark) => {
            let all: Vec<Sql> = parts.conditions.iter().cloned().chain([mark]).collect();
            let (condition, condition_values) = conditions_text(&all)?;
            text.push_str(&format!(" where {condition}"));
            values.extend(condition_values);
        }
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

/// An insert of rows of one shape, the first row's columns, each row's
/// version 0 on a versioned table whatever the row gave.
fn insert_of<T: Serialize>(parts: &Parts, rows: &[T]) -> Result<Sql> {
    let version = parts.tracked.version.as_deref();
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
        let first =
            |(name, value): (String, Value)| if Some(name.as_str()) == version { Value::Integer(0) } else { value };
        values.extend(fields.into_iter().map(first));
    }
    let columns = columns.unwrap_or_default();
    if columns.is_empty() {
        return Err(Error::invalid("a row to insert with no column"));
    }
    let quoted = quote_all(&columns)?;
    let table = quote(&parts.table).map_err(Error::invalid)?;
    let text = format!("insert into {table} ({}) values {}", quoted.join(", "), tuples.join(", "));
    Ok(Sql::piece(text, values, false))
}

/// The condition that starts a page after a row's order values. SQLite
/// orders a null before every value, first in ascending order and last in
/// descending, and a comparison with a null is never true: equal is `is`,
/// and past a null is every value ascending and nothing descending.
fn keyset(order: &[Order], after: &[Value]) -> Sql {
    let mut alternatives = Vec::with_capacity(order.len());
    let mut values = Vec::new();
    for (at, each) in order.iter().enumerate() {
        let column = each.piece.text();
        let value = after.get(at).cloned().unwrap_or(Value::Null);
        let past = match (each.direction, value) {
            (Direction::Asc, Value::Null) => (format!("{column} is not null"), None),
            (Direction::Desc, Value::Null) => continue,
            (Direction::Asc, value) => (format!("{column} > ?"), Some(value)),
            (Direction::Desc, value) => (format!("({column} < ? or {column} is null)"), Some(value)),
        };
        let mut terms: Vec<String> =
            order[..at].iter().map(|earlier| format!("{} is ?", earlier.piece.text())).collect();
        values.extend(after[..at].iter().cloned());
        terms.push(past.0);
        values.extend(past.1);
        alternatives.push(if terms.len() == 1 { terms.remove(0) } else { format!("({})", terms.join(" and ")) });
    }
    if alternatives.is_empty() {
        return Sql::piece("0".to_owned(), Vec::new(), true);
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

/// A value as JSON, and back by [`value_of`]: an infinity, which JSON has no
/// number for, as its text.
fn json_of(value: &Value) -> serde_json::Value {
    match value {
        Value::Null => serde_json::Value::Null,
        Value::Integer(n) => serde_json::Value::from(*n),
        Value::Real(x) if !x.is_finite() => serde_json::json!({ "real": x.to_string() }),
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
        serde_json::Value::Object(object) if object.contains_key("real") => {
            Value::Real(object.get("real")?.as_str()?.parse().ok()?)
        }
        serde_json::Value::Object(object) => Value::Blob(serde_json::from_value(object.get("bytes")?.clone()).ok()?),
        _ => return None,
    })
}

fn cursor_text(values: &[Value], digest: &str) -> String {
    let json = serde_json::json!({ "q": digest, "v": values.iter().map(json_of).collect::<Vec<_>>() });
    crate::text::hex(json.to_string().as_bytes())
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
