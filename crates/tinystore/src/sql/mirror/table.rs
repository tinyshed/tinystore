//! What a mirror's open checks of its table: that every column it names is
//! the table's, and that its key names one row, a unique index or the primary
//! key covering it. A key that named several rows would compare a source's
//! row with whichever came first. It also reads what the table makes of the
//! values it is given, each column's affinity and each key column's
//! collation, which a call compares by.

use std::collections::{BTreeSet, HashMap};

use rusqlite::Connection;

use super::affinity::{Affinity, Collation};
use super::list::Shape;
use crate::{Error, Result};

/// The table's columns, their declared types, and each set of them that
/// names one row.
const COLUMNS: &str = "select name, pk, type from pragma_table_info(?1)";
/// Whether the table is strict, whose ANY keeps every value as given.
const STRICT: &str = "select strict from pragma_table_list(?1) where schema = 'main'";
/// A unique index's key columns and their collations, a row a column; an
/// expression's name is null.
const UNIQUE: &str = "select il.name, ii.name, ii.coll from pragma_index_list(?1) as il
    join pragma_index_xinfo(il.name) as ii
    where il.\"unique\" and not il.partial and ii.key";

/// What the table makes of the values a mirror's list gives it.
#[derive(Debug, Default)]
pub(crate) struct Kept {
    affinities: HashMap<String, Affinity>,
    /// Each of the key's columns' collation, in the key's order.
    pub(crate) collations: Vec<Collation>,
}

impl Kept {
    /// A column's affinity; none for a column the table lacks, which its
    /// write refuses.
    pub(crate) fn affinity(&self, column: &str) -> Affinity {
        self.affinities.get(column).copied().unwrap_or(Affinity::None)
    }
}

pub(crate) fn check(connection: &Connection, shape: &Shape) -> Result<Kept> {
    let found = columns_of(connection, shape)?;
    let named = |column: &String| found.iter().any(|(name, _, _)| name == column);
    let compared = shape.change_on.iter().flatten();
    if let Some(column) = shape.key.iter().chain(&shape.gone).chain(compared).find(|column| !named(column)) {
        return Err(Error::invalid(format!("the table {:?} has no column {column:?}", shape.table)));
    }
    refuse_overlaps(shape)?;
    let strict: bool = connection
        .prepare_cached(STRICT)
        .and_then(|mut strict| strict.query_row([&shape.table], |row| row.get(0)))
        .map_err(failed)?;
    let affinities = found.iter().map(|(name, _, declared)| (name.clone(), Affinity::of(declared, strict))).collect();
    let primary: BTreeSet<&str> = found.iter().filter(|(_, pk, _)| *pk > 0).map(|(name, _, _)| name.as_str()).collect();
    let key: BTreeSet<&str> = shape.key.iter().map(String::as_str).collect();
    let covering = covering(connection, shape, &key)?;
    if covering.is_empty() && !(!primary.is_empty() && primary.is_subset(&key)) {
        return Err(Error::invalid(format!(
            "the key ({}) names no one row: a unique index or the primary key of {:?} covers it",
            shape.key.join(", "),
            shape.table
        )));
    }
    // a column an index covering the key compares by another collation than
    // binary is compared by it, since that index holds "a" and "A" as one
    let collation = |column: &String| -> Result<Collation> {
        let named = covering.iter().filter(|(name, _)| name == column).map(|(_, coll)| coll.as_str());
        let collations = named.map(Collation::named).collect::<Result<Vec<_>>>()?;
        Ok(collations.into_iter().find(|collation| *collation != Collation::Binary).unwrap_or_default())
    };
    let collations = shape.key.iter().map(collation).collect::<Result<_>>()?;
    Ok(Kept { affinities, collations })
}

fn failed(error: rusqlite::Error) -> Error {
    crate::sqlite::sql_error("its table", error)
}

/// The table's columns: each one's name, place in the primary key and
/// declared type.
fn columns_of(connection: &Connection, shape: &Shape) -> Result<Vec<(String, i64, String)>> {
    let mut columns = connection.prepare_cached(COLUMNS).map_err(failed)?;
    let found: Vec<(String, i64, String)> = columns
        .query_map([&shape.table], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .and_then(Iterator::collect)
        .map_err(failed)?;
    if found.is_empty() {
        return Err(Error::invalid(format!("the database has no table {:?}", shape.table)));
    }
    Ok(found)
}

/// The columns of every unique index whose columns are all the key's, each
/// with the collation the index compares it by.
fn covering(connection: &Connection, shape: &Shape, key: &BTreeSet<&str>) -> Result<Vec<(String, String)>> {
    let mut unique = connection.prepare_cached(UNIQUE).map_err(failed)?;
    let indexed: Vec<(String, Option<String>, String)> = unique
        .query_map([&shape.table], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .and_then(Iterator::collect)
        .map_err(failed)?;
    let covers = |index: &String| {
        let mut columns = indexed.iter().filter(|(name, _, _)| name == index).map(|(_, column, _)| column.as_deref());
        columns.all(|column| column.is_some_and(|column| key.contains(column)))
    };
    let covering = indexed.iter().filter(|(index, _, _)| covers(index));
    Ok(covering.filter_map(|(_, column, coll)| Some((column.clone()?, coll.clone()))).collect())
}

/// A key is at least one column, none of them the mark or a compared one.
fn refuse_overlaps(shape: &Shape) -> Result<()> {
    if shape.key.is_empty() {
        return Err(Error::invalid("a mirror's key is at least one column"));
    }
    if let Some(gone) = shape.gone.as_ref().filter(|gone| shape.key.contains(gone)) {
        return Err(Error::invalid(format!("the column {gone:?} that marks what is gone is in the key")));
    }
    if let Some(column) = shape.change_on.iter().flatten().find(|column| shape.key.contains(column)) {
        return Err(Error::invalid(format!("change_on names the key's column {column:?}, which never changes")));
    }
    Ok(())
}
