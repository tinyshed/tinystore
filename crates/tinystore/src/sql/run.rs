//! A statement run on a connection: a read's rows, a write's rows, a write's
//! counts.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(feature = "search")]
use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, Statement, params_from_iter};

use super::database::Done;
#[cfg(feature = "search")]
use super::rows::Rows;
use super::rows::{Answer, Held, MOST_READ, failure};
use super::statement::Sql;
#[cfg(feature = "search")]
use super::values::Value;
#[cfg(feature = "search")]
use crate::Memory;
use crate::sqlite::given;
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
    #[cfg(feature = "wire")]
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

    fn check(self, rows: &impl Answer) -> Result<()> {
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

/// A read's rows, or that its statement writes, which then goes to the writer.
pub(crate) enum Read<A> {
    Rows(A),
    Writes,
}

/// Runs `statement` on a reader, unless SQLite says it writes.
pub(crate) fn read<A: Answer>(connection: &Connection, statement: &Sql, wanted: Wanted, held: Held) -> Result<Read<A>> {
    given(statement.text(), || {
        let mut prepared = connection.prepare_cached(statement.text()).map_err(|error| failure("its text", error))?;
        if !prepared.readonly() {
            return Ok(Read::Writes);
        }
        answered(&mut prepared, statement, (wanted, false), held).map(Read::Rows)
    })
}

/// Runs `statement` on the writer and keeps its rows. Called in a savepoint,
/// so a write whose rows break the call's shape, two for `one`, rolls back.
pub(crate) fn write<A: Answer>(connection: &Connection, statement: &Sql, wanted: Wanted, held: Held) -> Result<A> {
    given(statement.text(), || {
        let mut prepared = connection.prepare_cached(statement.text()).map_err(|error| failure("its text", error))?;
        let writes = !prepared.readonly();
        answered(&mut prepared, statement, (wanted, writes), held)
    })
}

/// The rows a compiled statement gives with its values, as `wanted` shapes
/// them; `writes` says it is a write, which is stepped to its end.
fn answered<A: Answer>(
    prepared: &mut Statement<'_>,
    statement: &Sql,
    (wanted, writes): (Wanted, bool),
    held: Held,
) -> Result<A> {
    statement.check(prepared.parameter_count())?;
    let rows = A::read(prepared, statement.values(), (wanted.most(writes), MOST_READ), held)?;
    wanted.check(&rows)?;
    Ok(rows)
}

/// The rows of a table's search index's hits as `T`s, each the one row
/// `select` gives with its hit's key, or none: in the search's snapshot, and
/// within the store's memory and one call's bound together, as `db.all`
/// reads its rows.
#[cfg(feature = "search")]
pub(crate) fn rows_on<T: serde::de::DeserializeOwned>(
    connection: &Connection,
    memory: &Arc<Memory>,
    (select, keys): (&str, &[SqlValue]),
) -> Result<Vec<Option<T>>> {
    hit_rows(connection, memory, (select, keys), |rows| Ok(rows.decode()?.into_iter().next()))
}

/// A row as its columns' names and its values.
#[cfg(all(feature = "search", feature = "wire"))]
pub(crate) type Cells = (Vec<String>, Vec<Value>);

/// The rows of a table's index's hits as [`rows_on`] reads them, each as its
/// cells: the wire sends them so.
#[cfg(all(feature = "search", feature = "wire"))]
pub(crate) fn cells_on(
    connection: &Connection,
    memory: &Arc<Memory>,
    (select, keys): (&str, &[SqlValue]),
) -> Result<Vec<Option<Cells>>> {
    hit_rows(connection, memory, (select, keys), |rows| {
        Ok((rows.len() > 0).then(|| (rows.columns().to_vec(), rows.into_values())))
    })
}

/// Each hit's row, read by a statement of its own and made what the caller
/// keeps by `read`, while every row read so far holds the store's memory
/// and counts toward one call's bound: a thousand hits of large rows are
/// `limit`, not gigabytes the store does not know of.
#[cfg(feature = "search")]
fn hit_rows<R>(
    connection: &Connection,
    memory: &Arc<Memory>,
    (select, keys): (&str, &[SqlValue]),
    read: impl Fn(Rows) -> Result<Option<R>>,
) -> Result<Vec<Option<R>>> {
    let mut held = Held::of(memory);
    let mut bytes = 0;
    let mut found = Vec::with_capacity(keys.len());
    for key in keys {
        let key = match key {
            SqlValue::Integer(number) => Value::Integer(*number),
            SqlValue::Text(text) => Value::Text(text.clone()),
            _ => return Err(Error::internal("a row's key of neither an integer nor text")),
        };
        let rows: Rows =
            write(connection, &Sql::piece(select.to_owned(), vec![key], false), Wanted::One, Held::default())?;
        bytes += rows.bytes();
        held.cover(bytes)?;
        found.push(read(rows)?);
    }
    Ok(found)
}

/// Runs `statement` on the writer to its end and says what it changed.
pub(crate) fn exec(connection: &Connection, statement: &Sql) -> Result<Done> {
    given(statement.text(), || exec_given(connection, statement))
}

fn exec_given(connection: &Connection, statement: &Sql) -> Result<Done> {
    let mut prepared = connection.prepare_cached(statement.text()).map_err(|error| failure("its text", error))?;
    statement.check(prepared.parameter_count())?;
    let writes = !prepared.readonly();
    // The writer is shared by every write of a commit: a rowid left by
    // another write is not this one's.
    let before = connection.last_insert_rowid();
    // SQLite counts `changes` only for an insert, update or delete: a schema
    // change or a pragma would report another write's count otherwise.
    let total = connection.total_changes();
    let same_rowid = writes.then(|| inserted_again(connection, before)).transpose()?;
    let ran = run_to_end(&mut prepared, statement);
    if same_rowid.is_some() {
        connection
            .update_hook(None::<fn(rusqlite::hooks::Action, &str, &str, i64)>)
            .map_err(|error| failure("its run", error))?;
    }
    ran?;
    let Some(same_rowid) = same_rowid else {
        return Ok(Done::default());
    };
    let after = connection.last_insert_rowid();
    let changes = if connection.total_changes() == total { 0 } else { connection.changes() };
    let inserted = after != before || same_rowid.load(Ordering::Relaxed);
    Ok(Done { changes, last_insert_rowid: if inserted { after } else { 0 } })
}

fn run_to_end(prepared: &mut Statement<'_>, statement: &Sql) -> Result<()> {
    let mut rows = prepared.query(params_from_iter(statement.values())).map_err(|error| failure("its run", error))?;
    while rows.next().map_err(|error| failure("its run", error))?.is_some() {}
    Ok(())
}

/// Watches the run for an insert whose rowid is the one the connection's last
/// insert left: a chat's first message is row 1 of its table right after its
/// chat was row 1 of another, and comparing rowids alone would answer 0. The
/// store's own tables take rows only from its triggers, never from the
/// statement: a table's index notes an updated row's key as row 1 of a table
/// emptied at every commit.
fn inserted_again(connection: &Connection, before: i64) -> Result<Arc<AtomicBool>> {
    let seen = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&seen);
    connection
        .update_hook(Some(move |action, _: &str, table: &str, rowid| {
            let own = table.starts_with("_tinystore_");
            if action == rusqlite::hooks::Action::SQLITE_INSERT && rowid == before && !own {
                flag.store(true, Ordering::Relaxed);
            }
        }))
        .map_err(|error| failure("its run", error))?;
    Ok(seen)
}
