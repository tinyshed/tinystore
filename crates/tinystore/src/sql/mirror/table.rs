//! What a mirror's open checks of its table: that every column it names is
//! the table's, and that its key names one row, a unique index or the primary
//! key covering it. A key that named several rows would compare a source's
//! row with whichever came first.

use std::collections::BTreeSet;

use rusqlite::Connection;

use super::list::Shape;
use crate::{Error, Result};

/// The table's columns, and each set of them that names one row.
const COLUMNS: &str = "select name, pk from pragma_table_info(?1)";
/// A unique index's columns, a row a column; an expression's is null.
const UNIQUE: &str = "select il.name, ii.name from pragma_index_list(?1) as il
    join pragma_index_info(il.name) as ii
    where il.\"unique\" and not il.partial";

pub(crate) fn check(connection: &Connection, shape: &Shape) -> Result<()> {
    let failed = |error| crate::sqlite::sql_error("its table", error);
    let mut columns = connection.prepare_cached(COLUMNS).map_err(failed)?;
    let found: Vec<(String, i64)> = columns
        .query_map([&shape.table], |row| Ok((row.get(0)?, row.get(1)?)))
        .and_then(Iterator::collect)
        .map_err(failed)?;
    if found.is_empty() {
        return Err(Error::invalid(format!("the database has no table {:?}", shape.table)));
    }
    let named = |column: &String| found.iter().any(|(name, _)| name == column);
    let compared = shape.compare.iter().flatten();
    if let Some(column) = shape.key.iter().chain(&shape.gone).chain(compared).find(|column| !named(column)) {
        return Err(Error::invalid(format!("the table {:?} has no column {column:?}", shape.table)));
    }
    refuse_overlaps(shape)?;
    let key: BTreeSet<&str> = shape.key.iter().map(String::as_str).collect();
    let primary: BTreeSet<&str> = found.iter().filter(|(_, pk)| *pk > 0).map(|(name, _)| name.as_str()).collect();
    if !primary.is_empty() && primary.is_subset(&key) {
        return Ok(());
    }
    let mut unique = connection.prepare_cached(UNIQUE).map_err(failed)?;
    let indexed: Vec<(String, Option<String>)> = unique
        .query_map([&shape.table], |row| Ok((row.get(0)?, row.get(1)?)))
        .and_then(Iterator::collect)
        .map_err(failed)?;
    let covers = |index: &String| {
        let mut columns = indexed.iter().filter(|(name, _)| name == index).map(|(_, column)| column.as_deref());
        columns.all(|column| column.is_some_and(|column| key.contains(column)))
    };
    if indexed.iter().any(|(index, _)| covers(index)) {
        return Ok(());
    }
    Err(Error::invalid(format!(
        "the key ({}) names no one row: a unique index or the primary key of {:?} covers it",
        shape.key.join(", "),
        shape.table
    )))
}

/// A key is at least one column, none of them the mark or a compared one.
fn refuse_overlaps(shape: &Shape) -> Result<()> {
    if shape.key.is_empty() {
        return Err(Error::invalid("a mirror's key is at least one column"));
    }
    if let Some(gone) = shape.gone.as_ref().filter(|gone| shape.key.contains(gone)) {
        return Err(Error::invalid(format!("the column {gone:?} that marks what is gone is in the key")));
    }
    if let Some(column) = shape.compare.iter().flatten().find(|column| shape.key.contains(column)) {
        return Err(Error::invalid(format!("compare names the key's column {column:?}, which never changes")));
    }
    Ok(())
}
