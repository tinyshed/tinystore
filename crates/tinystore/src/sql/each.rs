//! The rows of a statement one at a time, as `each` gives them: from a walk
//! that reads them a part at a time, or, inside a transaction and for a write,
//! from the rows `all` read.

use std::marker::PhantomData;
use std::sync::Arc;

use serde::de::DeserializeOwned;

use super::database::Database;
use super::rows::{Decoding, Nested, Rows};
use super::run::Wanted;
use super::statement::Sql;
use super::walk::{End, Key, Taken, Walk, Walking};
use crate::{Error, Result};

/// The bytes of values a part of a walk holds: a few of them are read ahead,
/// so a walk holds a few MiB whatever its rows come to.
pub(crate) const PART_BYTES: usize = 1 << 20;

/// The rows of a statement, each as a `T`, as [`Database::each`] and
/// [`Query::each`](super::Query::each) give them. Dropped, it stops its read
/// and gives its reader back.
#[must_use = "an iterator reads its rows only as they are asked for"]
pub struct Each<T> {
    rows: Source,
    nested: Arc<[Nested]>,
    fail: Box<dyn Fn(Error) -> Error + Send + Sync>,
    row: PhantomData<fn() -> T>,
}

enum Source {
    /// A walk, and the part its rows are being taken from.
    Walk(Walk<Rows>, Option<Decoding>),
    /// Rows read whole: inside a transaction, or a write's.
    Read(Decoding),
    Done,
}

impl<T: DeserializeOwned> Each<T> {
    /// Walks `statement`'s rows from a reader of `database`, its first part
    /// read before it returns, so that a statement that fails fails here; one
    /// that writes gives its rows as `all` does.
    pub(crate) fn walk(
        database: &Database,
        statement: Sql,
        key: Option<Vec<Key>>,
        nested: Arc<[Nested]>,
    ) -> Result<Each<T>> {
        let fail = database.failed_by(&statement);
        if database.file().held_here() {
            let refused = "a walk from inside a transaction of its file would not see what the transaction wrote: \
                           make it through the transaction";
            return Err(fail(Error::invalid(refused)));
        }
        if database.known_to_write(&statement) {
            return Ok(Each::read(database.rows_of(&statement, Wanted::All)?, nested, fail));
        }
        let walking = Walking { database: database.clone(), statement: statement.clone(), key, bound: PART_BYTES };
        let (walk, reader) = Walk::new(walking).map_err(&fail)?;
        reader.spawn();
        let rows = match walk.wait() {
            Taken::Part(part) => Source::Walk(walk, Some(part.decoding(Arc::clone(&nested)))),
            Taken::End(End::Done) | Taken::Pending => Source::Done,
            Taken::End(End::Failed(error)) => return Err(error),
            Taken::End(End::Writes) => {
                database.remember_write(&statement);
                return Ok(Each::read(database.rows_of(&statement, Wanted::All)?, nested, fail));
            }
        };
        Ok(Each { rows, nested, fail: Box::new(fail), row: PhantomData })
    }

    /// The rows `all` read, given one at a time.
    pub(crate) fn read(
        rows: Rows,
        nested: Arc<[Nested]>,
        fail: impl Fn(Error) -> Error + Send + Sync + 'static,
    ) -> Each<T> {
        let rows = Source::Read(rows.decoding(Arc::clone(&nested)));
        Each { rows, nested, fail: Box::new(fail), row: PhantomData }
    }
}

impl<T: DeserializeOwned> Iterator for Each<T> {
    type Item = Result<T>;

    /// The next row; a failure is the last item.
    fn next(&mut self) -> Option<Result<T>> {
        loop {
            let (walk, part) = match &mut self.rows {
                Source::Done => return None,
                Source::Read(part) => match part.next() {
                    Some(row) => return Some(row.map_err(&self.fail)),
                    None => {
                        self.rows = Source::Done;
                        return None;
                    }
                },
                Source::Walk(walk, part) => (walk, part),
            };
            if let Some(row) = part.as_mut().and_then(|part| part.next::<T>()) {
                return Some(row.map_err(&self.fail));
            }
            *part = None;
            match walk.wait() {
                Taken::Part(next) => *part = Some(next.decoding(Arc::clone(&self.nested))),
                Taken::End(End::Failed(error)) => {
                    self.rows = Source::Done;
                    return Some(Err(error));
                }
                Taken::End(_) | Taken::Pending => self.rows = Source::Done,
            }
        }
    }
}

impl<T> Each<T> {
    /// The walk it takes its rows from, for a test that watches it.
    #[cfg(test)]
    pub(crate) fn walking(&self) -> Option<&Walk<Rows>> {
        match &self.rows {
            Source::Walk(walk, _) => Some(walk),
            _ => None,
        }
    }
}

impl<T> std::fmt::Debug for Each<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let reading = match self.rows {
            Source::Walk(..) => "a walk",
            Source::Read(_) => "rows read",
            Source::Done => "done",
        };
        write!(f, "Each({reading})")
    }
}
