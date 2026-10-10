//! A statement run on a connection: a read's rows, a write's rows, a write's
//! counts.

use rusqlite::{Connection, params_from_iter};

use super::database::Done;
use super::rows::{Held, MOST_READ, Rows, failure};
use super::statement::Sql;
use crate::{Error, Result};

/// What a call wants of a statement's rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Wanted {
    All,
    /// One row or none.
    One,
    /// The one value of one row.
    Scalar,
}

impl Wanted {
    /// The shape a client asks for by name.
    pub(crate) fn named(name: &str) -> Option<Wanted> {
        match name {
            "all" => Some(Wanted::All),
            "one" => Some(Wanted::One),
            "scalar" => Some(Wanted::Scalar),
            _ => None,
        }
    }

    /// Rows to read before the call can tell it got too many. A write is
    /// stepped to its end whatever it returns, since its end is its effect.
    fn most(self, writes: bool) -> usize {
        match self {
            Wanted::All => usize::MAX,
            Wanted::One | Wanted::Scalar if writes => usize::MAX,
            Wanted::One | Wanted::Scalar => 2,
        }
    }

    fn check(self, rows: &Rows) -> Result<()> {
        match self {
            Wanted::All => Ok(()),
            Wanted::One if rows.len() > 1 => {
                Err(Error::invalid("one row asked for, and the statement gave several: ask all of them, with a limit"))
            }
            Wanted::One => Ok(()),
            Wanted::Scalar if rows.len() != 1 || rows.width() != 1 => Err(Error::invalid(format!(
                "a scalar is one value of one row, and the statement gave {} rows of {} columns",
                rows.len(),
                rows.width()
            ))),
            Wanted::Scalar => Ok(()),
        }
    }
}

/// A read's rows, or word that the statement writes, which goes to the writer.
pub(crate) enum Read {
    Rows(Rows),
    Writes,
}

/// Runs `statement` on a reader, unless SQLite says it writes.
pub(crate) fn read(connection: &Connection, statement: &Sql, wanted: Wanted, held: Held) -> Result<Read> {
    let mut prepared = connection.prepare_cached(statement.text()).map_err(|error| failure("its text", error))?;
    if !prepared.readonly() {
        return Ok(Read::Writes);
    }
    statement.check(prepared.parameter_count())?;
    let rows = Rows::read(&mut prepared, statement.values(), (wanted.most(false), MOST_READ), held)?;
    wanted.check(&rows)?;
    Ok(Read::Rows(rows))
}

/// Runs `statement` on the writer and keeps its rows. Called in a savepoint,
/// so a write whose rows break the call's shape, two for `one`, rolls back.
pub(crate) fn write(connection: &Connection, statement: &Sql, wanted: Wanted, held: Held) -> Result<Rows> {
    let mut prepared = connection.prepare_cached(statement.text()).map_err(|error| failure("its text", error))?;
    statement.check(prepared.parameter_count())?;
    let writes = !prepared.readonly();
    let rows = Rows::read(&mut prepared, statement.values(), (wanted.most(writes), MOST_READ), held)?;
    wanted.check(&rows)?;
    Ok(rows)
}

/// Runs `statement` on the writer to its end and says what it changed.
pub(crate) fn exec(connection: &Connection, statement: &Sql) -> Result<Done> {
    let mut prepared = connection.prepare_cached(statement.text()).map_err(|error| failure("its text", error))?;
    statement.check(prepared.parameter_count())?;
    let writes = !prepared.readonly();
    // The writer is shared by every write of a commit: a rowid left by
    // another write is not this one's.
    let before = connection.last_insert_rowid();
    let mut rows = prepared.query(params_from_iter(statement.values())).map_err(|error| failure("its run", error))?;
    while rows.next().map_err(|error| failure("its run", error))?.is_some() {}
    drop(rows);
    if !writes {
        return Ok(Done::default());
    }
    let after = connection.last_insert_rowid();
    Ok(Done { changes: connection.changes(), last_insert_rowid: if after == before { 0 } else { after } })
}
