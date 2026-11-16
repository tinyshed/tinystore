//! A statement's rows one at a time, awaited: a `Stream` of a walk whose
//! parts are read on the store's pool, each row decoded as it is polled.

use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use futures_core::Stream;
use serde::de::DeserializeOwned;

use super::Database;
use crate::sql::{Decoding, End, Key, Nested, PART_BYTES, Rows, Sql, Taken, Walk, Walking, Wanted};
use crate::{Error, Result};

type Read<T> = Pin<Box<dyn Future<Output = Result<Vec<T>>> + Send>>;

/// The rows of a statement, each as a `T`, as
/// [`Database::each`](super::Database::each) and
/// [`Query::each`](super::Query::each) give them: a `Stream` that begins its
/// walk at its first poll. Dropped, it stops its read at the next row and
/// gives its reader back. A failure is its last item.
#[must_use = "streams do nothing unless polled"]
pub struct Each<T> {
    rows: Source<T>,
    nested: Arc<[Nested]>,
    row: PhantomData<fn() -> T>,
}

enum Source<T> {
    /// Not polled yet: the walk it begins at its first poll.
    Asked(Database, Sql, Option<Vec<Key>>),
    Walk(Walk<Rows>, Option<Decoding>, Box<dyn Fn(Error) -> Error + Send + Sync>, (Database, Sql)),
    /// A statement that writes, read as `all` reads it.
    Reading(Read<T>),
    Read(std::vec::IntoIter<T>),
    Failed(Error),
    Done,
}

// The stream is never pinned in place: nothing points into it.
impl<T> Unpin for Each<T> {}

impl<T: DeserializeOwned + Send + 'static> Each<T> {
    pub(super) fn new(
        database: Database,
        statement: Result<Sql>,
        key: Option<Vec<Key>>,
        nested: Arc<[Nested]>,
    ) -> Each<T> {
        let rows = match statement {
            Ok(statement) => Source::Asked(database, statement, key),
            Err(error) => Source::Failed(error),
        };
        Each { rows, nested, row: PhantomData }
    }

    /// Begins the walk on the pool, or, for a statement known to write, its
    /// read as `all`.
    fn begin(&self, database: &Database, statement: Sql, key: Option<Vec<Key>>) -> Source<T> {
        let fail = database.db.failed_by(&statement);
        if let Err(error) = super::run::refuse_inside_a_transaction(&database.what("a walk")) {
            return Source::Failed(error);
        }
        if database.db.known_to_write(&statement) {
            return Source::Reading(self.read(database, statement));
        }
        let asked = (database.clone(), statement.clone());
        let walking = Walking { database: database.db.clone(), statement, key, bound: PART_BYTES };
        match Walk::new(walking) {
            Ok((walk, reader)) => {
                reader.spawn();
                Source::Walk(walk, None, Box::new(fail), asked)
            }
            Err(error) => Source::Failed(fail(error)),
        }
    }

    fn read(&self, database: &Database, statement: Sql) -> Read<T> {
        let nested = self.nested.to_vec();
        Box::pin(database.rows(statement, Wanted::All, move |rows| rows.nesting(nested).decode::<T>()))
    }

    /// The walk's next row, or what comes after its part: the next part, or
    /// its end. Pending, the walk wakes the task once a part comes. None when
    /// the walk found its statement writes, and its read as `all` is next.
    fn walked(&mut self, context: &Context<'_>) -> Option<Poll<Option<Result<T>>>> {
        let Source::Walk(walk, part, fail, asked) = &mut self.rows else {
            return Some(Poll::Ready(None));
        };
        let ended = loop {
            if let Some(row) = part.as_mut().and_then(|part| part.next::<T>()) {
                return Some(Poll::Ready(Some(row.map_err(fail))));
            }
            let waker = context.waker().clone();
            match walk.take(|_, _| true, Arc::new(move || waker.wake_by_ref())).0 {
                Taken::Part(next) => *part = Some(next.decoding(Arc::clone(&self.nested))),
                Taken::Pending => return Some(Poll::Pending),
                Taken::End(end) => break end,
            }
        };
        let (database, statement) = asked.clone();
        self.rows = Source::Done;
        match ended {
            End::Done => Some(Poll::Ready(None)),
            End::Failed(error) => Some(Poll::Ready(Some(Err(error)))),
            End::Writes => {
                database.db.remember_write(&statement);
                self.rows = Source::Reading(self.read(&database, statement));
                None
            }
        }
    }
}

impl<T: DeserializeOwned + Send + 'static> Stream for Each<T> {
    type Item = Result<T>;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Result<T>>> {
        let this = self.get_mut();
        loop {
            match std::mem::replace(&mut this.rows, Source::Done) {
                Source::Asked(database, statement, key) => this.rows = this.begin(&database, statement, key),
                Source::Walk(walk, part, fail, asked) => {
                    this.rows = Source::Walk(walk, part, fail, asked);
                    if let Some(walked) = this.walked(context) {
                        return walked;
                    }
                }
                Source::Reading(mut reading) => match reading.as_mut().poll(context) {
                    Poll::Ready(Ok(rows)) => this.rows = Source::Read(rows.into_iter()),
                    Poll::Ready(Err(error)) => return Poll::Ready(Some(Err(error))),
                    Poll::Pending => {
                        this.rows = Source::Reading(reading);
                        return Poll::Pending;
                    }
                },
                Source::Read(mut rows) => {
                    let row = rows.next();
                    this.rows = Source::Read(rows);
                    return Poll::Ready(row.map(Ok));
                }
                Source::Failed(error) => return Poll::Ready(Some(Err(error))),
                Source::Done => return Poll::Ready(None),
            }
        }
    }
}

impl<T> std::fmt::Debug for Each<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let reading = match self.rows {
            Source::Asked(..) => "not polled",
            Source::Walk(..) => "a walk",
            Source::Reading(_) | Source::Read(_) => "rows read",
            Source::Failed(_) | Source::Done => "done",
        };
        write!(f, "Each({reading})")
    }
}
