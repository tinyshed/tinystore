//! A table that mirrors another system: it takes the source's list and says
//! what changed. A program that keeps a copy of another system's rows, a
//! course's works, a shop's orders, gives the list the source answered, and
//! the mirror compares it with what the table holds by the source's key,
//! writes only the difference in one commit, and answers what was added,
//! changed and gone.
//!
//! ```no_run
//! # #[derive(serde::Serialize, serde::Deserialize)]
//! # struct Work { course_id: i64, id: String, grade: Option<f64> }
//! # fn main() -> tinystore::Result<()> {
//! # let db = tinystore::Store::open("data", Default::default())?.database("lms").open()?;
//! # let fetched: Vec<Work> = Vec::new();
//! let works = db.mirror::<Work>("works").key(["course_id", "id"]).gone("gone_at").change_on(["grade"]).open()?;
//! let diff = works.replace(&[("course_id", 1)], &fetched)?;
//! for changed in &diff.changed {
//!     // changed.before.grade, then changed.after.grade
//! }
//! # Ok(())
//! # }
//! ```
//!
//! docs/guides/sql/mirror.md is the guide, in every language.

mod affinity;
mod list;
mod table;

use std::marker::PhantomData;
use std::sync::Arc;

use serde::Serialize;
use serde::de::DeserializeOwned;

use super::database::Database;
use super::rows::Rows;
use super::values::{Value, row_of, to_value};
use crate::transaction::sealed::Sealed;
use crate::{Error, ErrorKind, Result, Transaction, TxHandle, unix_millis};
pub(crate) use list::{Found, Given, Listed, Mode, Running, Shape, mirror_on};

impl Database {
    /// A table that mirrors another system, `T` its rows: its `replace`
    /// takes the source's list of a scope and its `upsert` a change of some
    /// rows, and each answers what was added, changed and gone. The key,
    /// what names a row at the source, is given before it opens.
    pub fn mirror<T>(&self, table: &str) -> MirrorBuilder<T> {
        let shape = Shape { table: table.to_owned(), key: Vec::new(), gone: None, change_on: None };
        MirrorBuilder { db: self.clone(), shape, row: PhantomData }
    }
}

/// How a mirror opens; [`Database::mirror`] makes one.
#[derive(Debug)]
#[must_use = "a mirror opens with `open`"]
pub struct MirrorBuilder<T> {
    db: Database,
    shape: Shape,
    row: PhantomData<fn() -> T>,
}

impl<T> MirrorBuilder<T> {
    /// The columns that name a row at the source, `["course_id", "id"]`: a
    /// unique index or the table's primary key must cover them.
    pub fn key<I: IntoIterator<Item = S>, S: Into<String>>(mut self, columns: I) -> Self {
        self.shape.key = columns.into_iter().map(Into::into).collect();
        self
    }

    /// A column that marks what the source dropped with the store's time, an
    /// integer of Unix milliseconds, instead of deleting the row. A row
    /// marked is neither marked nor answered again, and one the source names
    /// once more loses its mark and is added again.
    pub fn gone(mut self, column: &str) -> Self {
        self.shape.gone = Some(column.to_owned());
        self
    }

    /// The columns whose change makes a row changed; every column the rows
    /// carry but the key's when not given. A row whose compared columns are
    /// the same is not written at all, even where its other columns differ:
    /// a `fetched_at` the source writes at every read changes nothing.
    pub fn change_on<I: IntoIterator<Item = S>, S: Into<String>>(mut self, columns: I) -> Self {
        self.shape.change_on = Some(columns.into_iter().map(Into::into).collect());
        self
    }

    /// Opens the mirror once its table has every column it names and its
    /// key names one row; `Invalid` otherwise.
    pub fn open(self) -> Result<Mirror<T>> {
        let mirror = Mirror { db: self.db, shape: Arc::new(self.shape), row: PhantomData };
        let checked = mirror.db.file().read(|connection| table::check(connection, &mirror.shape).map(drop));
        checked.map_err(|error| error.within(mirror.describe()))?;
        Ok(mirror)
    }
}

/// A table that mirrors another system; [`Database::mirror`] opens one.
/// Clones share it.
#[derive(Debug)]
pub struct Mirror<T> {
    db: Database,
    shape: Arc<Shape>,
    row: PhantomData<fn() -> T>,
}

impl<T> Mirror<T> {
    /// The mirror as an error names it: `sql lms: mirror works`.
    pub(crate) fn describe(&self) -> String {
        format!("{}: mirror {}", self.db.shown(), self.shape.table)
    }
}

impl<T> Clone for Mirror<T> {
    fn clone(&self) -> Self {
        Mirror { db: self.db.clone(), shape: Arc::clone(&self.shape), row: PhantomData }
    }
}

/// What a mirror's call changed, each row a `T`: the rows added, the rows
/// changed as they were and as they are, the rows the source dropped, and
/// how many it named as they were held. A suspect answer wrote nothing.
///
/// A row answers by the columns the list's rows carry: an added row and a
/// changed one's `after` as the list gave it, a changed one's `before` and a
/// gone one as the table held them.
#[derive(Clone, Debug, PartialEq)]
pub struct Diff<T> {
    pub added: Vec<T>,
    pub changed: Vec<Changed<T>>,
    pub gone: Vec<T>,
    pub same: u64,
    pub suspect: bool,
}

/// A row that changed: as the table held it, and as the source gave it.
#[derive(Clone, Debug, PartialEq)]
pub struct Changed<T> {
    pub before: T,
    pub after: T,
}

impl<T: Serialize + DeserializeOwned> Mirror<T> {
    /// Makes the scope, the rows whose columns hold the scope's values, `&[]`
    /// for the whole table, what `rows` lists, in one write, all of it or
    /// none: adds what the table lacks, writes the compared columns' changes,
    /// and deletes, or marks gone, what the list leaves out. A row the list
    /// names outside the scope is `Invalid`, and so is one whose key holds a
    /// null. Values compare as the table keeps them: a column converts what
    /// it is given as SQLite stores it, `4` given to a text column is `"4"`,
    /// an integer and a real compare by their numbers, and a key's text by
    /// the collation of the index that makes it unique. On a copy of a node
    /// it is `Unavailable`: run it at the file's home.
    ///
    /// A scope of mixed types takes `serde_json::Value`s, `json!(1)` beside
    /// `json!("fall")`.
    pub fn replace<S: AsRef<str>, V: Serialize>(&self, scope: &[(S, V)], rows: &[T]) -> Result<Diff<T>> {
        self.run(scope_of(scope).map(|scope| Mode::Replace { scope, min_kept: None }), rows)
    }

    /// Writes what `rows` adds and changes, a delta of the source's, and
    /// marks nothing gone: its `gone` is always empty.
    pub fn upsert(&self, rows: &[T]) -> Result<Diff<T>> {
        self.run(Ok(Mode::Upsert), rows)
    }

    /// The mirror's `replace` guarded against an answer that cannot be
    /// right: when the scope holds live rows and the list would leave fewer
    /// than `fraction` of them live, an empty list among them, it writes
    /// nothing and answers what it would have done, `suspect`. A fraction
    /// outside `(0, 1]` is `Invalid`.
    ///
    /// ```no_run
    /// # #[derive(serde::Serialize, serde::Deserialize)] struct Work { course_id: i64, id: String }
    /// # fn main() -> tinystore::Result<()> {
    /// # let db = tinystore::Store::open("data", Default::default())?.database("lms").open()?;
    /// # let works = db.mirror::<Work>("works").key(["course_id", "id"]).open()?;
    /// let diff = works.min_kept(0.5).replace(&[("course_id", 1)], &[])?;
    /// assert!(diff.suspect); // a course that held works: nothing was deleted
    /// # Ok(())
    /// # }
    /// ```
    pub fn min_kept(&self, fraction: f64) -> Guarded<'_, T> {
        Guarded { mirror: self, min_kept: fraction }
    }

    fn run(&self, mode: Result<Mode>, rows: &[T]) -> Result<Diff<T>> {
        self.prepare(mode, rows)?.write().and_then(Diff::of)
    }

    /// A call made: its rows checked and converted now, before it takes the
    /// writer, and the write that compares them there.
    pub(crate) fn prepare(&self, mode: Result<Mode>, rows: &[T]) -> Result<Prepared> {
        let prepared = mode.and_then(|mode| {
            let given = Given::checked(&self.shape, listed_of(rows)?, &mode)?;
            Ok((given.weight(), given, mode))
        });
        let (weight, given, mode) = prepared.map_err(|error| error.within(self.describe()))?;
        let (shape, memory, now) = (Arc::clone(&self.shape), Arc::clone(self.db.memory()), unix_millis(self.db.now()));
        let write = Box::new(move |tx: &crate::sqlite::Tx<'_>| {
            mirror_on(tx, &shape, (&given, &mode), &Running { now, memory: &memory, answer: None })
        });
        Ok(Prepared { db: self.db.clone(), weight, write, describe: self.describe() })
    }
}

/// What a mirror's call runs on the writer, in its savepoint.
type Compare = Box<dyn FnOnce(&crate::sqlite::Tx<'_>) -> Result<Found> + Send>;

/// A mirror's call whose rows are checked, waiting for the writer.
pub(crate) struct Prepared {
    db: Database,
    weight: usize,
    write: Compare,
    describe: String,
}

impl Prepared {
    fn write(self) -> Result<Found> {
        refuse_on_a_copy(&self.db, &self.describe)?;
        let describe = self.describe;
        self.db.file().write(self.weight, self.write).map_err(|error| error.within(describe))
    }

    /// The write without waiting: `done` is called with what it found on
    /// the thread that commits it.
    #[cfg(feature = "async")]
    pub(crate) fn submit(self, done: impl FnOnce(Result<Found>) + Send + 'static) {
        if let Err(error) = refuse_on_a_copy(&self.db, &self.describe) {
            return done(Err(error));
        }
        let describe = self.describe;
        self.db
            .file()
            .submit(self.weight, self.write, move |found| done(found.map_err(|error| error.within(describe))));
    }
}

/// Refuses a mirror's call on a copy of its database: it compares and writes
/// on the file's writer, which a copy has not, and it is no statement a copy
/// passes on to its home, as `exec` is. A copy otherwise ran it on a reader,
/// which answered a list held as it is and failed any other on its first
/// write.
fn refuse_on_a_copy(db: &Database, describe: &str) -> Result<()> {
    if !db.file().is_copy() {
        return Ok(());
    }
    let why =
        format!("{describe}: a mirror writes on its file's writer, which a copy has not: run it at the file's home");
    Err(Error::new(ErrorKind::Unavailable, why))
}

/// A mirror's `replace` with its min_kept guard; [`Mirror::min_kept`] makes one.
#[derive(Debug)]
pub struct Guarded<'m, T> {
    mirror: &'m Mirror<T>,
    min_kept: f64,
}

impl<T: Serialize + DeserializeOwned> Guarded<'_, T> {
    /// [`Mirror::replace`], which writes nothing when the guard holds it back.
    pub fn replace<S: AsRef<str>, V: Serialize>(&self, scope: &[(S, V)], rows: &[T]) -> Result<Diff<T>> {
        let mode = scope_of(scope).map(|scope| Mode::Replace { scope, min_kept: Some(self.min_kept) });
        self.mirror.run(mode, rows)
    }
}

impl<T: DeserializeOwned> Diff<T> {
    pub(crate) fn of(found: Found) -> Result<Diff<T>> {
        let columns: Arc<[String]> = found.columns.into();
        let decode =
            |rows: Vec<Vec<Value>>| Rows::of(Arc::clone(&columns), rows.into_iter().flatten().collect()).decode();
        let (before, after): (Vec<_>, Vec<_>) = found.changed.into_iter().unzip();
        let changed = decode(before)?.into_iter().zip(decode(after)?).map(|(before, after)| Changed { before, after });
        Ok(Diff {
            added: decode(found.added)?,
            changed: changed.collect(),
            gone: decode(found.gone)?,
            same: found.same,
            suspect: found.suspect,
        })
    }
}

/// The scope's columns and values, each value as SQLite keeps it.
pub(crate) fn scope_of<S: AsRef<str>, V: Serialize>(scope: &[(S, V)]) -> Result<Vec<(String, Value)>> {
    let value = |(column, value): &(S, V)| {
        let column = column.as_ref();
        let kept = to_value(value).map_err(|why| Error::invalid(format!("the scope's {column}: {why}")))?;
        Ok((column.to_owned(), kept))
    };
    scope.iter().map(value).collect()
}

/// The program's rows as cells: one shape, their columns once.
fn listed_of<T: Serialize>(rows: &[T]) -> Result<Listed> {
    let mut listed = Listed::default();
    for (at, row) in rows.iter().enumerate() {
        let fields = row_of(row).map_err(|why| Error::invalid(format!("row {}: {why}", at + 1)))?;
        let (columns, values): (Vec<String>, Vec<Value>) = fields.into_iter().unzip();
        if at == 0 {
            listed.columns = columns;
        } else if columns != listed.columns {
            return Err(Error::invalid(format!("row {}: not the first row's columns: give rows of one shape", at + 1)));
        }
        listed.rows.push(values);
    }
    Ok(listed)
}

impl<T> Sealed for Mirror<T> {}

impl<T> TxHandle for Mirror<T> {
    type InTx<'t>
        = TxMirror<'t, T>
    where
        Self: 't;

    fn in_tx<'t>(&'t self, tx: &'t Transaction<'t>) -> TxMirror<'t, T> {
        TxMirror { mirror: self, tx }
    }
}

/// A mirror's calls inside a transaction of its database, `tx.with(&works)`:
/// what they write commits with the transaction or not at all, and they see
/// what it wrote.
pub struct TxMirror<'t, T> {
    mirror: &'t Mirror<T>,
    tx: &'t Transaction<'t>,
}

impl<T> std::fmt::Debug for TxMirror<'_, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TxMirror({})", self.mirror.shape.table)
    }
}

impl<T: Serialize + DeserializeOwned> TxMirror<'_, T> {
    /// [`Mirror::replace`], inside the transaction.
    pub fn replace<S: AsRef<str>, V: Serialize>(&self, scope: &[(S, V)], rows: &[T]) -> Result<Diff<T>> {
        self.run(scope_of(scope).map(|scope| Mode::Replace { scope, min_kept: None }), rows)
    }

    /// [`Mirror::upsert`], inside the transaction.
    pub fn upsert(&self, rows: &[T]) -> Result<Diff<T>> {
        self.run(Ok(Mode::Upsert), rows)
    }

    fn run(&self, mode: Result<Mode>, rows: &[T]) -> Result<Diff<T>> {
        let mirror = self.mirror;
        let ran = mode.and_then(|mode| {
            let given = Given::checked(&mirror.shape, listed_of(rows)?, &mode)?;
            self.tx.takes(mirror.db.file(), &mirror.db.shown())?;
            let running = Running { now: unix_millis(mirror.db.now()), memory: mirror.db.memory(), answer: None };
            self.tx.call(|raw| mirror_on(raw, &mirror.shape, (&given, &mode), &running))
        });
        ran.and_then(Diff::of).map_err(|error| error.within(mirror.describe()))
    }
}

impl Database {
    /// A mirror's call for a client that is not Rust, the wire's, without
    /// waiting for its commit: its table is checked on a reader first, as an
    /// open checks it, its answer is held within `answer`, and `done` is
    /// called on the thread that commits it.
    #[cfg(feature = "wire")]
    pub(crate) fn mirror_then(
        &self,
        (shape, given, mode): (Shape, Given, Mode),
        answer: crate::limits::Limit,
        done: impl FnOnce(Result<Found>) + Send + 'static,
    ) {
        let describe = format!("{}: mirror {}", self.shown(), shape.table);
        let (memory, now, weight) = (Arc::clone(self.memory()), unix_millis(self.now()), given.weight());
        if let Err(error) = refuse_on_a_copy(self, &describe) {
            return done(Err(error));
        }
        if let Err(error) = self.file().read(|connection| table::check(connection, &shape)) {
            return done(Err(error.within(describe)));
        }
        let write = move |tx: &crate::sqlite::Tx<'_>| {
            mirror_on(tx, &shape, (&given, &mode), &Running { now, memory: &memory, answer: Some(answer) })
        };
        self.file().submit(weight, write, move |found| done(found.map_err(|error| error.within(describe))));
    }
}

#[cfg(test)]
#[path = "mirror_tests.rs"]
mod tests;
