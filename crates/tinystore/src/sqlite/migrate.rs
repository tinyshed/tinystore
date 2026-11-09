use std::borrow::Cow;

use rusqlite::{Connection, params};

use super::Tx;
use super::connection::{given_sql_error, sql_error};
use super::given::given;
use crate::{Error, ErrorKind, Result};

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
const RENAME_APPLIED: &str = "update _tinystore_migrations set name = ?3 where history = ?1 and version = ?2";
const FOREIGN_KEY_CHECK: &str = "pragma foreign_key_check";
const HISTORY_KEPT: &str = "select count(*) from sqlite_schema where name = '_tinystore_migrations'";

/// How a step that has not shipped is named: `0007_messages.draft.sql`, which
/// ships as `0007_messages.sql`.
const DRAFT: &str = ".draft.sql";

pub(crate) fn is_draft(name: &str) -> bool {
    name.ends_with(DRAFT)
}

/// The name a draft ships under.
fn shipped_name(draft: &str) -> Option<String> {
    draft.strip_suffix(DRAFT).map(|stem| format!("{stem}.sql"))
}

/// Where a file's applied steps stand beside the program's: how many it
/// applied, those it applied as drafts that the program has shipped since,
/// and whether one it applied is a draft still.
#[derive(Debug, Default)]
pub(crate) struct Compared {
    pub(crate) applied: usize,
    pub(crate) shipped: Vec<(u32, String)>,
    pub(crate) drafts_applied: bool,
}

/// Applies the steps of `history` the file has not seen, after checking that
/// the ones it has seen are these steps, word for word and name for name.
/// Each history is one engine's or one database's, so engines sharing a file
/// keep theirs apart.
pub(crate) fn apply(tx: &Tx<'_>, history: &str, migrations: &[Migration]) -> Result<()> {
    check_order(history, migrations)?;
    tx.execute_batch(CREATE_HISTORY).map_err(|error| sql_error("its migration history", error))?;
    let applied = applied(tx, history)?;
    let compared = check_applied(history, migrations, &applied, None)?;
    apply_steps(tx, history, &migrations[compared.applied..])
}

/// Where the file's steps of `history` stand beside `migrations`, read on
/// `connection`; `redo` is the command a refusal of an edited draft names.
pub(crate) fn compare(
    connection: &Connection,
    history: &str,
    migrations: &[Migration],
    redo: Option<&str>,
) -> Result<Compared> {
    check_order(history, migrations)?;
    check_applied(history, migrations, &applied_if_kept(connection, history)?, redo)
}

/// Gives each step the file applied as a draft the name it shipped under.
pub(crate) fn ship(tx: &Tx<'_>, history: &str, shipped: &[(u32, String)]) -> Result<()> {
    for (version, name) in shipped {
        tx.prepare_cached(RENAME_APPLIED)
            .and_then(|mut rename| rename.execute(params![history, version, name]))
            .map_err(|error| sql_error(format!("{history}: {name}, shipped"), error))?;
    }
    Ok(())
}

/// Applies `steps`, the next of `history` the file has not seen.
pub(crate) fn apply_steps(tx: &Tx<'_>, history: &str, steps: &[Migration]) -> Result<()> {
    tx.execute_batch(CREATE_HISTORY).map_err(|error| sql_error("its migration history", error))?;
    for migration in steps {
        let what = || format!("{history}: {}", migration.name);
        given(&migration.sql, || {
            tx.execute_batch(&migration.sql).map_err(|error| given_sql_error("its statements", error))
        })
        .map_err(|error| error.within(what()))?;
        tx.prepare_cached(INSERT_APPLIED)
            .and_then(|mut insert| insert.execute(params![history, migration.version, migration.name, migration.sql]))
            .map_err(|error| sql_error(what(), error))?;
    }
    Ok(())
}

/// Checks, on a copy, that its home applied these steps of `history` and no
/// other: the steps are the home's to run, and arrive as its pages. A step
/// not there yet is `Unavailable` until the home's commit of it arrives.
pub(crate) fn check_applied_steps(
    connection: &Connection,
    history: &str,
    migrations: &[Migration],
    redo: Option<&str>,
) -> Result<()> {
    let compared = compare(connection, history, migrations, redo)?;
    if let Some((_, name)) = compared.shipped.first() {
        return Err(Error::new(ErrorKind::Unavailable, format!("{history}: {name} shipped since it was a draft")));
    }
    match migrations.get(compared.applied) {
        None => Ok(()),
        Some(missing) => Err(Error::new(
            ErrorKind::Unavailable,
            format!("{history}: {} has not reached this copy from its home yet", missing.name),
        )),
    }
}

/// Whether a step the file applied of `history` is a draft, whatever its
/// text or the program's says.
pub(crate) fn drafts_applied(connection: &Connection, history: &str) -> Result<bool> {
    Ok(applied_if_kept(connection, history)?.iter().any(|step| is_draft(&step.name)))
}

/// The steps the file applied, none when it keeps no history yet.
fn applied_if_kept(connection: &Connection, history: &str) -> Result<Vec<Applied>> {
    let kept: i64 = connection
        .query_row(HISTORY_KEPT, [], |row| row.get(0))
        .map_err(|error| sql_error(format!("{history}: its migration history"), error))?;
    if kept == 0 { Ok(Vec::new()) } else { applied(connection, history) }
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

pub(crate) fn check_order(history: &str, migrations: &[Migration]) -> Result<()> {
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
    // taking the drafts back takes back what follows them, so nothing shipped may
    let first = migrations.iter().position(|migration| is_draft(&migration.name));
    if let Some(first) = first
        && let Some(after) = migrations[first..].iter().find(|later| !is_draft(&later.name))
    {
        return Err(Error::invalid(format!(
            "{history}: {} follows the draft {}, and drafts come after every migration that has shipped",
            after.name, migrations[first].name
        )));
    }
    Ok(())
}

struct Applied {
    version: u32,
    name: String,
    sql: String,
}

fn applied(connection: &Connection, history: &str) -> Result<Vec<Applied>> {
    let read = || -> rusqlite::Result<Vec<Applied>> {
        let mut select = connection.prepare_cached(SELECT_APPLIED)?;
        let rows = select
            .query_map([history], |row| Ok(Applied { version: row.get(0)?, name: row.get(1)?, sql: row.get(2)? }))?;
        rows.collect()
    };
    read().map_err(|error| sql_error(format!("{history}: its applied migrations"), error))
}

fn check_applied(history: &str, migrations: &[Migration], applied: &[Applied], redo: Option<&str>) -> Result<Compared> {
    let mut compared = Compared { applied: applied.len(), ..Compared::default() };
    for (index, step) in applied.iter().enumerate() {
        let shipped_as = shipped_name(&step.name);
        let shipped = migrations.get(index).is_some_and(|migration| shipped_as.as_deref() == Some(&*migration.name));
        let same = migrations.get(index).is_some_and(|migration| {
            migration.version == step.version
                && (migration.name == step.name.as_str() || shipped)
                && same_statements(&migration.sql, &step.sql)
        });
        // a draft edited, renamed or dropped is taken back, whatever changed
        if let Some(redo) = redo.filter(|_| !same && is_draft(&step.name)) {
            return Err(Error::invalid(format!(
                "{history}: the draft {} changed after it was applied: `{redo}` takes the database back to \
                 its copy from before its drafts and applies them again, losing what was written since",
                step.name
            )));
        }
        let Some(migration) = migrations.get(index) else {
            return Err(Error::invalid(format!(
                "{history}: the file applied {}, which this program does not have: a newer program migrated it",
                step.name
            )));
        };
        if migration.version != step.version || (migration.name != step.name.as_str() && !shipped) {
            return Err(Error::invalid(format!(
                "{history}: the file applied {} as step {}, where this program has {}: a shipped migration was renamed or one was put before it",
                step.name, step.version, migration.name
            )));
        }
        if !same_statements(&migration.sql, &step.sql) {
            return Err(changed(history, &step.name, index + 1 == applied.len()));
        }
        if shipped {
            compared.shipped.push((step.version, migration.name.to_string()));
        }
        compared.drafts_applied |= is_draft(&step.name) && !shipped;
    }
    Ok(compared)
}

/// Why a step the file applied, its text since changed, refuses the file:
/// one that shipped never changes, and the newest says how to write a step
/// that has not.
fn changed(history: &str, name: &str, newest: bool) -> Error {
    match newest && !is_draft(name) {
        true => Error::invalid(format!(
            "{history}: {name} changed after it was applied: one that shipped never changes, so write the change \
             as the next migration; one still being written is named NNNN_name.draft.sql before it is first \
             applied, and the database is copied before it, for an edited draft to be applied again"
        )),
        false => Error::invalid(format!("{history}: {name} changed after it was applied")),
    }
}

/// Whether two texts of a step say the same, whatever their lines end with:
/// Git on Windows may check a file out with `\r\n` where the step was applied
/// from it with `\n`, and the step is the same.
fn same_statements(given: &str, applied: &str) -> bool {
    given == applied || given.replace("\r\n", "\n") == applied.replace("\r\n", "\n")
}
