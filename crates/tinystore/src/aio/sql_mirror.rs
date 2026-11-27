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
    /// opens one: its `replace` and `patch` awaited.
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
    pub fn gone(self, column: &str) -> Self {
        MirrorBuilder { builder: self.builder.gone(column), pool: self.pool }
    }

    /// The columns whose change makes a row changed, as [`sql::MirrorBuilder::compare`] says.
    pub fn compare<I: IntoIterator<Item = S>, S: Into<String>>(self, columns: I) -> Self {
        MirrorBuilder { builder: self.builder.compare(columns), pool: self.pool }
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
        let mode = sql::scope_of(scope).map(|scope| Mode::Replace { scope, shrink: None });
        self.run(mode, rows)
    }

    /// `replace` with its shrink guard, as [`sql::Mirror::shrink`] guards it.
    pub fn shrink(&self, fraction: f64) -> Guarded<'_, T> {
        Guarded { mirror: self, shrink: fraction }
    }

    /// What `rows` adds and changes, as [`sql::Mirror::patch`] writes it.
    pub fn patch(&self, rows: &[T]) -> impl Future<Output = Result<Diff<T>>> + Send + 'static {
        self.run(Ok(Mode::Patch), rows)
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

/// An awaited mirror's `replace` with its shrink guard; [`Mirror::shrink`]
/// makes one.
#[derive(Debug)]
pub struct Guarded<'m, T> {
    mirror: &'m Mirror<T>,
    shrink: f64,
}

impl<T: Serialize + DeserializeOwned + Send + 'static> Guarded<'_, T> {
    /// [`Mirror::replace`], which writes nothing when the guard holds it back.
    pub fn replace<S: AsRef<str>, V: Serialize>(
        &self,
        scope: &[(S, V)],
        rows: &[T],
    ) -> impl Future<Output = Result<Diff<T>>> + Send + 'static {
        let mode = sql::scope_of(scope).map(|scope| Mode::Replace { scope, shrink: Some(self.shrink) });
        self.mirror.run(mode, rows)
    }
}
