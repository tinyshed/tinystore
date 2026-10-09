use rusqlite::params;

use super::Tx;
use super::connection::sql_error;
use crate::{Error, Result};

/// One step of a schema's history: applied once, in order, and never edited
/// after it shipped. Until `v0.1.0` an engine keeps one step and edits it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Migration {
    pub(crate) version: u32,
    pub(crate) sql: &'static str,
}

const CREATE_HISTORY: &str = "create table if not exists _tinystore_migrations (
    history text not null,
    version integer not null,
    sql text not null,
    primary key (history, version)
) strict, without rowid";
const SELECT_APPLIED: &str = "select version, sql from _tinystore_migrations where history = ?1 order by version";
const INSERT_APPLIED: &str = "insert into _tinystore_migrations (history, version, sql) values (?1, ?2, ?3)";

/// Applies the steps of `history` the file has not seen, after checking that
/// the ones it has seen are these steps, word for word. Each history is one
/// engine's or one database's, so engines sharing a file keep theirs apart.
pub(crate) fn apply(tx: &Tx<'_>, history: &str, migrations: &[Migration]) -> Result<()> {
    check_order(history, migrations)?;
    tx.execute_batch(CREATE_HISTORY).map_err(|error| sql_error("its migration history", error))?;
    let applied = applied(tx, history)?;
    check_applied(history, migrations, &applied)?;
    for migration in &migrations[applied.len()..] {
        let what = || format!("{history}: migration {}", migration.version);
        tx.execute_batch(migration.sql).map_err(|error| sql_error(what(), error))?;
        tx.prepare_cached(INSERT_APPLIED)
            .and_then(|mut insert| insert.execute(params![history, migration.version, migration.sql]))
            .map_err(|error| sql_error(what(), error))?;
    }
    Ok(())
}

fn check_order(history: &str, migrations: &[Migration]) -> Result<()> {
    for (index, migration) in migrations.iter().enumerate() {
        if usize::try_from(migration.version).ok() != Some(index + 1) {
            return Err(Error::invalid(format!(
                "{history}: migration {} is step {} of its history, which counts from 1 without a gap",
                migration.version,
                index + 1
            )));
        }
    }
    Ok(())
}

fn applied(tx: &Tx<'_>, history: &str) -> Result<Vec<(u32, String)>> {
    let read = || -> rusqlite::Result<Vec<(u32, String)>> {
        let mut select = tx.prepare_cached(SELECT_APPLIED)?;
        let rows = select.query_map([history], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect()
    };
    read().map_err(|error| sql_error(format!("{history}: its applied migrations"), error))
}

fn check_applied(history: &str, migrations: &[Migration], applied: &[(u32, String)]) -> Result<()> {
    for (index, (version, sql)) in applied.iter().enumerate() {
        let Some(migration) = migrations.get(index) else {
            return Err(Error::invalid(format!(
                "{history}: the file has migration {version}, which this program does not have: a newer program migrated it"
            )));
        };
        if migration.version != *version || migration.sql != sql {
            return Err(Error::invalid(format!(
                "{history}: migration {version} is not the one the file applied: a shipped migration was edited"
            )));
        }
    }
    Ok(())
}
