//! What a statement answers, without its rows: its columns in its order, and
//! the type each one's table declares. A program that shows another's
//! statement, a console, a dashboard or an export, learns its columns here
//! and not from its first row, which a statement that answers none has not.

use std::panic::{AssertUnwindSafe, catch_unwind};

use rusqlite::{Connection, Statement};

use super::rows::{NOT_UTF8, failure};
use super::statement::Sql;
use crate::sqlite::given;
use crate::{Error, Result};

/// A column a statement answers: its name as its rows name it, and the type
/// its table declares for it where the column is a table's own, as SQLite
/// keeps it: a type a strict table takes in capitals, `INTEGER` or `TEXT`,
/// and any other as written, `varchar(80)`; `None` for an expression, and
/// for a column its table declares no type for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Column {
    pub name: String,
    pub declared: Option<String>,
}

/// Compiles `statement` on a reader and says its columns: it runs nothing,
/// and reads none of the `?`s' values, which may be missing. One that writes
/// is `invalid`: it would run on the writer, and its columns are its
/// `returning`'s, which a read names as well.
pub(crate) fn columns_on(connection: &Connection, statement: &Sql) -> Result<Vec<Column>> {
    see_the_schema(connection)?;
    given(statement.text(), || {
        // Not kept: a kept statement compiled before a migration changed its
        // table names the old columns until it is stepped, and this is never
        // stepped.
        let prepared = connection.prepare(statement.text()).map_err(|error| failure("its text", error))?;
        if !prepared.readonly() {
            return Err(Error::invalid(WRITES));
        }
        columns_of(&prepared)
    })
}

const WRITES: &str = "a statement is described as a read, and this one writes: describe the query that reads its rows";

/// Has the reader read the file's schema as it is now. SQLite compiles a
/// statement against the schema its connection read last, and notices that
/// another connection changed it only when a statement steps: a reader
/// another migration passed would describe the tables as they were.
fn see_the_schema(connection: &Connection) -> Result<()> {
    let mut version = connection.prepare_cached(SCHEMA_VERSION).map_err(|error| failure("its schema", error))?;
    version.query_row([], |_| Ok(())).map_err(|error| failure("its schema", error))
}

/// A read of the schema's table, whose step checks the schema's cookie.
const SCHEMA_VERSION: &str = "select count(*) from sqlite_schema";

/// A compiled statement's columns. A declared type that is not UTF-8, which
/// only a file another program wrote can hold, is `invalid`: rusqlite panics
/// on one rather than fail, so its panic is caught here.
fn columns_of(prepared: &Statement<'_>) -> Result<Vec<Column>> {
    let columns = catch_unwind(AssertUnwindSafe(|| {
        let named = prepared.columns();
        named
            .iter()
            .map(|column| Column { name: column.name().to_owned(), declared: column.decl_type().map(str::to_owned) })
            .collect()
    }));
    columns.map_err(|_| Error::invalid(NOT_UTF8))
}
