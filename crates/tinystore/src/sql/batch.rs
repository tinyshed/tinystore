//! A database's batch: writes known before they run, in one savepoint of the
//! next shared commit, all of them or none. A write is a statement, or a
//! queue's add for a queue kept in the database's file, so that a job
//! commits with the rows it is about and still shares the commit with every
//! other write of the file, where `db.tx` holds the writer and pays a sync
//! of its own.

use std::fmt;

use super::database::Done;
use super::run;
use super::statement::Sql;
use crate::Result;
#[cfg(feature = "jobs")]
use crate::engine::LentWrite;
use crate::sqlite::{File, Tx as SqlTx};
#[cfg(feature = "jobs")]
use crate::{Error, ErrorKind};

/// A write of a batch: a statement, or a job's add of a queue opened from
/// the database, `queue.adding(&job)`. A statement becomes one by `into`:
///
/// ```no_run
/// # #[derive(serde::Serialize, serde::Deserialize)]
/// # struct Index { note: i64 }
/// # fn main() -> tinystore::Result<()> {
/// # let store = tinystore::Store::open("data", Default::default())?;
/// # let (body, note) = ("milk", 7);
/// let db = store.database("app").open()?;
/// let indexing = db.queue::<Index>("indexing").open()?;
/// db.batch([
///     tinystore::sql!("update notes set body = ? where id = ?", body, note).into(),
///     indexing.adding(&Index { note }), // committed with the row, or not at all
/// ])?;
/// # Ok(())
/// # }
/// ```
pub struct Write(Kind);

enum Kind {
    Statement(Sql),
    #[cfg(feature = "jobs")]
    Lent(LentWrite),
    /// What made the write could not: its batch fails with it, and runs
    /// nothing.
    #[cfg(feature = "jobs")]
    Refused(Error),
}

impl Write {
    /// A write of a handle kept in a file another engine lends, or why it
    /// could not be made.
    #[cfg(feature = "jobs")]
    pub(crate) fn lent(made: Result<LentWrite>) -> Write {
        Write(made.map_or_else(Kind::Refused, Kind::Lent))
    }
}

impl From<Sql> for Write {
    fn from(statement: Sql) -> Write {
        Write(Kind::Statement(statement))
    }
}

impl From<&str> for Write {
    fn from(text: &str) -> Write {
        Write::from(Sql::from(text))
    }
}

impl From<String> for Write {
    fn from(text: String) -> Write {
        Write::from(Sql::from(text))
    }
}

impl From<&String> for Write {
    fn from(text: &String) -> Write {
        Write::from(Sql::from(text))
    }
}

impl fmt::Debug for Write {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Kind::Statement(statement) => f.debug_tuple("Write").field(statement).finish(),
            #[cfg(feature = "jobs")]
            Kind::Lent(lent) => f.debug_tuple("Write").field(lent).finish(),
            #[cfg(feature = "jobs")]
            Kind::Refused(error) => f.debug_tuple("Write").field(error).finish(),
        }
    }
}

/// A batch's writes, checked against the file it runs on before it waits
/// for the writer.
pub(super) struct Batch {
    writes: Vec<Kind>,
}

/// What a batch did: a `Done` a write, in their order, and what runs once
/// its commit is durable.
pub(super) struct Ran {
    done: Vec<Done>,
    committed: Vec<Box<dyn FnOnce() + Send>>,
}

impl Ran {
    /// What each write did, once what waited for the commit ran.
    pub(super) fn finish(self) -> Vec<Done> {
        for committed in self.committed {
            committed();
        }
        self.done
    }
}

impl Batch {
    /// The writes of a batch on `file`, which `owner` names: a write that
    /// could not be made, or one of a handle kept in another file, fails it.
    /// A copy passes statements on to its file's home, and refuses the rest,
    /// which the home's own batch runs.
    pub(super) fn checked(writes: Vec<Write>, file: &File, owner: &str) -> Result<Batch> {
        let writes = writes.into_iter().map(|Write(write)| kept_in(write, file, owner));
        Ok(Batch { writes: writes.collect::<Result<_>>()? })
    }

    /// The batch's statements, every write being one: what a copy passes on.
    #[cfg(feature = "replication")]
    pub(super) fn statements(self) -> Vec<Sql> {
        let mut statements = Vec::with_capacity(self.writes.len());
        for write in self.writes {
            match write {
                Kind::Statement(statement) => statements.push(statement),
                #[cfg(feature = "jobs")]
                Kind::Lent(_) | Kind::Refused(_) => {}
            }
        }
        statements
    }

    /// What the batch weighs in its commit, and its write: every write in
    /// order, a failure naming its own.
    pub(super) fn write(self) -> (usize, impl FnOnce(&SqlTx<'_>) -> Result<Ran> + Send + 'static) {
        let weight = self.writes.iter().map(weight_of).sum();
        let writes = self.writes;
        let write = move |tx: &SqlTx<'_>| {
            let mut ran = Ran { done: Vec::with_capacity(writes.len()), committed: Vec::new() };
            for write in writes {
                match write {
                    Kind::Statement(statement) => {
                        let done = run::exec(tx, &statement).map_err(|error| error.within(statement.describe()))?;
                        ran.done.push(done);
                    }
                    #[cfg(feature = "jobs")]
                    Kind::Lent(lent) => {
                        let describe = lent.describe;
                        let wrote = (lent.write)(tx).map_err(|error| error.within(describe))?;
                        ran.done.push(Done { changes: u64::from(wrote.changed), last_insert_rowid: 0 });
                        ran.committed.extend(wrote.committed);
                    }
                    #[cfg(feature = "jobs")]
                    Kind::Refused(error) => return Err(error),
                }
            }
            Ok(ran)
        };
        (weight, write)
    }
}

/// A write a batch on `file` can run: a statement, or a lent write of a
/// handle kept in `file` while `file` is not a copy.
#[cfg(feature = "jobs")]
fn kept_in(write: Kind, file: &File, owner: &str) -> Result<Kind> {
    match write {
        Kind::Refused(error) => Err(error),
        Kind::Lent(lent) if !std::ptr::eq(&*lent.file, file) => {
            let why = format!("kept in {}, outside this batch of {owner}", lent.place);
            Err(Error::invalid(why).within(lent.describe))
        }
        Kind::Lent(lent) if file.is_copy() => {
            let why = "a batch that writes a queue's job runs at its file's home, which this copy is not";
            Err(Error::new(ErrorKind::Unavailable, why).within(lent.describe))
        }
        write => Ok(write),
    }
}

/// Without jobs a batch holds statements alone, which any file runs.
#[cfg(not(feature = "jobs"))]
fn kept_in(write: Kind, _file: &File, _owner: &str) -> Result<Kind> {
    Ok(write)
}

fn weight_of(write: &Kind) -> usize {
    match write {
        Kind::Statement(statement) => statement.weight(),
        #[cfg(feature = "jobs")]
        Kind::Lent(lent) => lent.weight,
        #[cfg(feature = "jobs")]
        Kind::Refused(_) => 0,
    }
}
