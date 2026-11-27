//! A mirror whose calls are awaited: the blocking one's list, compared on
//! the writer in a shared commit, its answer decoded on the store's pool.

use std::future::Future;
use std::sync::Arc;

use serde::Serialize;
use serde::de::DeserializeOwned;

use super::super::run;
use super::Database;
use crate::pool::Pool;
use crate::sql::{self, Diff, Found, Mode};
use crate::{Error, Result};

impl Database {
    /// A table that mirrors another system, as [`sql::Database::mirror`]
    /// opens one: its `replace` and `upsert` awaited.
    pub fn mirror<T>(&self, table: &str) -> MirrorBuilder<T> {
        MirrorBuilder { builder: self.db.mirror(table), pool: Arc::clone(&self.pool) }
    }
}

/// How an awaited mirror opens, with the blocking builder's options.
#[derive(Debug)]
#[must_use = "a mirror opens with `open`"]
pub struct MirrorBuilder<T> {
    builder: sql::MirrorBuilder<T>,
    pool: Arc<Pool>,
}

impl<T> MirrorBuilder<T> {
    /// The columns that name a row at the source, as [`sql::MirrorBuilder::key`] takes them.
    pub fn key<I: IntoIterator<Item = S>, S: Into<String>>(self, columns: I) -> Self {
        MirrorBuilder { builder: self.builder.key(columns), pool: self.pool }
    }

    /// The column that marks what the source dropped, as [`sql::MirrorBuilder::gone`] says.
    pub fn gone(self, mark: impl Into<sql::Mark>) -> Self {
        MirrorBuilder { builder: self.builder.gone(mark), pool: self.pool }
    }

    /// The columns whose change makes a row changed, as [`sql::MirrorBuilder::change_on`] says.
    pub fn change_on<I: IntoIterator<Item = S>, S: Into<String>>(self, columns: I) -> Self {
        MirrorBuilder { builder: self.builder.change_on(columns), pool: self.pool }
    }

    /// Opens the mirror as [`sql::MirrorBuilder::open`] does: a plain call,
    /// which reads the table's columns and indexes.
    pub fn open(self) -> Result<Mirror<T>> {
        self.builder.open().map(|mirror| Mirror { mirror, pool: self.pool })
    }
}

/// A table that mirrors another system whose calls are awaited, the same
/// mirror as [`sql::Mirror`]. Each call checks and converts its rows when it
/// is made, so that its future borrows nothing, and writes in its first poll.
#[derive(Clone, Debug)]
pub struct Mirror<T> {
    mirror: sql::Mirror<T>,
    pool: Arc<Pool>,
}

impl<T: Serialize + DeserializeOwned + Send + 'static> Mirror<T> {
    /// The scope made what `rows` lists, as [`sql::Mirror::replace`] makes it.
    pub fn replace<S: AsRef<str>, V: Serialize>(
        &self,
        scope: &[(S, V)],
        rows: &[T],
    ) -> impl Future<Output = Result<Diff<T>>> + Send + 'static {
        let mode = sql::scope_of(scope).map(|scope| Mode::Replace { scope, min_kept: None });
        self.run(mode, rows)
    }

    /// `replace` with its min_kept guard, as [`sql::Mirror::min_kept`] guards it.
    pub fn min_kept(&self, fraction: f64) -> Guarded<'_, T> {
        Guarded { mirror: self, min_kept: fraction }
    }

    /// A `replace` whose list comes in parts, as [`sql::Mirror::begin`]
    /// begins one: its table checked now, its parts and commit awaited.
    pub fn begin<S: AsRef<str>, V: Serialize>(&self, scope: &[(S, V)]) -> Result<Run<T>> {
        Ok(Run { run: self.mirror.begin(scope)?, pool: Arc::clone(&self.pool) })
    }

    /// What `rows` adds and changes, as [`sql::Mirror::upsert`] writes it.
    pub fn upsert(&self, rows: &[T]) -> impl Future<Output = Result<Diff<T>>> + Send + 'static {
        self.run(Ok(Mode::Upsert), rows)
    }

    fn run(&self, mode: Result<Mode>, rows: &[T]) -> impl Future<Output = Result<Diff<T>>> + Send + 'static {
        let (prepared, decoding) = (self.mirror.prepare(mode, rows), Arc::clone(&self.pool));
        run::write(&self.pool, self.mirror.describe(), move |answer| {
            let decode = move |found: Result<Found>| {
                // decoded on the pool, never on the thread that commits every write of the group
                decoding.run_always(Box::new(move || {
                    if answer.is_awaited() {
                        answer.send(found.and_then(Diff::of));
                    }
                }));
            };
            match prepared {
                Ok(prepared) => prepared.submit(decode),
                Err(error) => decode(Err::<Found, Error>(error)),
            }
        })
    }
}

/// An awaited mirror's `replace` with its min_kept guard; [`Mirror::min_kept`]
/// makes one.
#[derive(Debug)]
pub struct Guarded<'m, T> {
    mirror: &'m Mirror<T>,
    min_kept: f64,
}

impl<T: Serialize + DeserializeOwned + Send + 'static> Guarded<'_, T> {
    /// [`Mirror::replace`], which writes nothing when the guard holds it back.
    pub fn replace<S: AsRef<str>, V: Serialize>(
        &self,
        scope: &[(S, V)],
        rows: &[T],
    ) -> impl Future<Output = Result<Diff<T>>> + Send + 'static {
        let mode = sql::scope_of(scope).map(|scope| Mode::Replace { scope, min_kept: Some(self.min_kept) });
        self.mirror.run(mode, rows)
    }

    /// [`Mirror::begin`], whose commit writes nothing when the guard holds it back.
    pub fn begin<S: AsRef<str>, V: Serialize>(&self, scope: &[(S, V)]) -> Result<Run<T>> {
        let run = self.mirror.mirror.min_kept(self.min_kept).begin(scope)?;
        Ok(Run { run, pool: Arc::clone(&self.mirror.pool) })
    }
}

/// An awaited mirror's `replace` in parts, the same run as [`sql::Run`].
#[derive(Debug)]
pub struct Run<T> {
    run: sql::Run<T>,
    pool: Arc<Pool>,
}

impl<T: Serialize + DeserializeOwned + Send + 'static> Run<T> {
    /// Gives a part of the list, as [`sql::Run::add`] does, checked when
    /// the call is made.
    pub fn add(&self, rows: &[T]) -> impl Future<Output = Result<()>> + Send + 'static {
        let staged = Arc::clone(self.run.staged());
        let listed = sql::listed_of(rows).map_err(|error| error.within(staged.describe()));
        run::write(&self.pool, staged.describe().to_owned(), move |answer| match listed {
            Ok(listed) => staged.add_then(listed, move |kept| answer.send(kept)),
            Err(error) => answer.send(Err(error)),
        })
    }

    /// Makes the scope what every part listed, as [`sql::Run::commit`]
    /// does, its answer decoded on the store's pool.
    pub fn commit(self) -> impl Future<Output = Result<Diff<T>>> + Send + 'static {
        let (staged, decoding) = (Arc::clone(self.run.staged()), Arc::clone(&self.pool));
        run::write(&self.pool, staged.describe().to_owned(), move |answer| {
            let ran = Arc::clone(&staged);
            ran.commit_then(None, move |found: Result<Found>| {
                decoding.run_always(Box::new(move || {
                    drop(staged);
                    if answer.is_awaited() {
                        answer.send(found.and_then(Diff::of));
                    }
                }));
            });
        })
    }
}
