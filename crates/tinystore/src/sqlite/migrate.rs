use std::borrow::Cow;

use rusqlite::params;

use super::Tx;
use super::connection::sql_error;
use super::given::given;
use crate::{Error, Result};

/// One step of a schema's history: applied once, in order, and never edited
/// or renamed after it shipped. Until `v0.1.0` an engine keeps one step and
/// edits it; an application's database keeps what its program ships.
#[derive(Clone, Debug)]
pub(crate) struct Migration {
    pub(crate) version: u32,
    /// What an error calls it: the file it came from.
    pub(crate) name: Cow<'static, str>,
    pub(crate) sql: Cow<'static, str>,
}

impl Migration {
    pub(crate) const fn of(version: u32, name: &'static str, sql: &'static str) -> Self {
        Self { version, name: Cow::Borrowed(name), sql: Cow::Borrowed(sql) }
    }
}

const CREATE_HISTORY: &str = "create table if not exists _tinystore_migrations (
    history text not null,
    version integer not null,
    name text not null,
    sql text not null,
    primary key (history, version)
) strict, without rowid";
const SELECT_APPLIED: &str = "select version, name, sql from _tinystore_migrations where history = ?1 order by version";
const INSERT_APPLIED: &str = "insert into _tinystore_migrations (history, version, name, sql) values (?1, ?2, ?3, ?4)";
const FOREIGN_KEY_CHECK: &str = "pragma foreign_key_check";

/// Applies the steps of `history` the file has not seen, after checking that
/// the ones it has seen are these steps, word for word and name for name.
/// Each history is one engine's or one database's, so engines sharing a file
/// keep theirs apart.
pub(crate) fn apply(tx: &Tx<'_>, history: &str, migrations: &[Migration]) -> Result<()> {
    check_order(history, migrations)?;
    tx.execute_batch(CREATE_HISTORY).map_err(|error| sql_error("its migration history", error))?;
    let applied = applied(tx, history)?;
    check_applied(history, migrations, &applied)?;
    for migration in &migrations[applied.len()..] {
        let what = || format!("{history}: {}", migration.name);
        given(&migration.sql, || tx.execute_batch(&migration.sql).map_err(|error| sql_error("its statements", error)))
            .map_err(|error| error.within(what()))?;
        tx.prepare_cached(INSERT_APPLIED)
            .and_then(|mut insert| insert.execute(params![history, migration.version, migration.name, migration.sql]))
            .map_err(|error| sql_error(what(), error))?;
    }
    Ok(())
}

/// Fails when a row refers to a parent that is not there, as migrations run
/// with foreign keys off may leave one; SQLite checks nothing of it itself.
pub(crate) fn check_foreign_keys(tx: &Tx<'_>, history: &str) -> Result<()> {
    let first = || -> rusqlite::Result<Option<(String, String)>> {
        let mut check = tx.prepare(FOREIGN_KEY_CHECK)?;
        let mut rows = check.query([])?;
        rows.next()?.map(|row| Ok((row.get(0)?, row.get(2)?))).transpose()
    };
    match first().map_err(|error| sql_error(format!("{history}: its foreign key check"), error))? {
        None => Ok(()),
        Some((table, parent)) => Err(Error::invalid(format!(
            "{history}: the migrations leave a row of {table} whose {parent} is missing, which a foreign key forbids"
        ))),
    }
}

fn check_order(history: &str, migrations: &[Migration]) -> Result<()> {
    for (index, migration) in migrations.iter().enumerate() {
        if usize::try_from(migration.version).ok() != Some(index + 1) {
            return Err(Error::invalid(format!(
                "{history}: {} is numbered {}, where its place makes it {}: a history counts from 1 without a gap or a repeat",
                migration.name,
                migration.version,
                index + 1
            )));
        }
    }
    Ok(())
}

struct Applied {
    version: u32,
    name: String,
    sql: String,
}

fn applied(tx: &Tx<'_>, history: &str) -> Result<Vec<Applied>> {
    let read = || -> rusqlite::Result<Vec<Applied>> {
        let mut select = tx.prepare_cached(SELECT_APPLIED)?;
        let rows = select
            .query_map([history], |row| Ok(Applied { version: row.get(0)?, name: row.get(1)?, sql: row.get(2)? }))?;
        rows.collect()
    };
    read().map_err(|error| sql_error(format!("{history}: its applied migrations"), error))
}

fn check_applied(history: &str, migrations: &[Migration], applied: &[Applied]) -> Result<()> {
    for (index, step) in applied.iter().enumerate() {
        let Some(migration) = migrations.get(index) else {
            return Err(Error::invalid(format!(
                "{history}: the file applied {}, which this program does not have: a newer program migrated it",
                step.name
            )));
        };
        if migration.version != step.version || migration.name != step.name.as_str() {
            return Err(Error::invalid(format!(
                "{history}: the file applied {} as step {}, where this program has {}: a shipped migration was renamed or one was put before it",
                step.name, step.version, migration.name
            )));
        }
        if migration.sql != step.sql.as_str() {
            return Err(Error::invalid(format!("{history}: {} changed after it was applied", migration.name)));
        }
    }
    Ok(())
}
