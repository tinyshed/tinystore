use std::cell::Cell;
use std::path::Path;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use rusqlite::config::DbConfig;
use rusqlite::limits::Limit;
use rusqlite::{Connection, ErrorCode, OpenFlags};

use super::given::{self, OWN};
use super::{Config, Durability};
use crate::{Error, ErrorKind, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Role {
    Writer,
    Reader,
}

/// Opens one connection with every setting its role needs. Each setting is
/// per connection in SQLite, so a reader opened again is the same reader.
pub(crate) fn open(path: &Path, config: &Config, role: Role) -> Result<Connection> {
    let what = || format!("{}: {} connection", path.display(), role.name());
    super::give_mutexes();
    warn_of_a_slow_build();
    let connection = Connection::open_with_flags(path, flags(role)).map_err(|error| sql_error(what(), error))?;
    connection.busy_timeout(config.busy_timeout).map_err(|error| sql_error(what(), error))?;
    connection.set_prepared_statement_cache_capacity(config.statements);
    match role {
        Role::Writer => {
            let mode = configure_writer(&connection, config).map_err(|error| sql_error(what(), error))?;
            if !mode.eq_ignore_ascii_case("wal") {
                return Err(Error::invalid(format!("{}: the journal mode is {mode}, not wal", what())));
            }
        }
        Role::Reader => configure_reader(&connection, config).map_err(|error| sql_error(what(), error))?,
    }
    keep_to_its_file(&connection).map_err(|error| sql_error(what(), error))?;
    if role == Role::Writer {
        commit_only_through_commit(&connection).map_err(|error| sql_error(what(), error))?;
    }
    Ok(connection)
}

thread_local! {
    /// This thread runs the adapter's own commit, [`commit`].
    static COMMITTING: Cell<bool> = const { Cell::new(false) };
}

/// Has `writer` refuse every commit but [`commit`]'s.
///
/// SQLite rolls a whole transaction back, its savepoints with it, for a
/// statement's `or rollback`, a trigger's `raise(rollback)`, or a disk that
/// failed mid-statement, and the connection is then in autocommit: each
/// statement run after it would commit by itself, beside the writes that
/// shared the transaction and were told it rolled back. With the hook such a
/// statement fails instead, and writes nothing.
fn commit_only_through_commit(writer: &Connection) -> rusqlite::Result<()> {
    writer.commit_hook(Some(|| !COMMITTING.get()))
}

/// Commits the transaction open on `writer`.
pub(crate) fn commit(writer: &Connection) -> rusqlite::Result<()> {
    let before = COMMITTING.replace(true);
    let committed = execute(writer, super::group::COMMIT);
    COMMITTING.set(before);
    committed
}

/// Leaves the connection its one file and its settings: no statement
/// attaches another, the store's own included, and a statement the store was
/// given changes nothing the connection's next user would meet.
fn keep_to_its_file(connection: &Connection) -> rusqlite::Result<()> {
    connection.set_limit(Limit::SQLITE_LIMIT_ATTACHED, 0)?;
    given::guard(connection)
}

const SHARED_PAGE_CACHE: &str = "SQLite was built with SQLITE_ENABLE_MEMORY_MANAGEMENT, so every connection \
    shares one page cache behind one mutex and readers stop one another: point reads from 16 threads ran 7-9x \
    slower (research rust-slice-2026-10-10); build with LIBSQLITE3_FLAGS=\"SQLITE_DQS=0 \
    -USQLITE_ENABLE_MEMORY_MANAGEMENT SQLITE_ENABLE_DBPAGE_VTAB -DHAVE_FDATASYNC=1\"";

const OWN_MUTEXES: &str = "SQLite had started before the store opened and keeps its own mutexes, which park a \
    thread at once: a few threads reading one file wait on each other; open the store before the program's own SQLite";

/// The warnings of a slow build, once they were found, and whether they were
/// logged before any subscriber listened.
static SLOW_BUILD: OnceLock<(Vec<&'static str>, AtomicBool)> = OnceLock::new();

/// Says once what the SQLite linked in costs readers, where it was built or
/// started without what the workspace gives it: a program that builds the
/// crate elsewhere finds out from its log, not from a profile.
fn warn_of_a_slow_build() {
    SLOW_BUILD.get_or_init(|| {
        let mut warnings = Vec::new();
        if super::memory::shares_page_cache() {
            warnings.push(SHARED_PAGE_CACHE);
        }
        if !super::mutexes_given() {
            warnings.push(OWN_MUTEXES);
        }
        for warning in &warnings {
            tracing::warn!(target: "tinystore", "{warning}");
        }
        // The guides set the subscriber up after the store opens, with the
        // store's logger in it: these went nowhere, and that logger says them.
        (warnings, AtomicBool::new(!tracing::dispatcher::has_been_set()))
    });
}

/// The warnings of a slow build that no subscriber heard, once: what a
/// store's logger prints first.
pub(crate) fn unheard_warnings() -> &'static [&'static str] {
    match SLOW_BUILD.get() {
        Some((warnings, unheard)) if unheard.swap(false, Ordering::Relaxed) => warnings,
        _ => &[],
    }
}

fn flags(role: Role) -> OpenFlags {
    // Each connection is used by one thread at a time, under the file's own
    // locks, so SQLite's per-connection mutex would only cost.
    let shared = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX | OpenFlags::SQLITE_OPEN_EXRESCODE;
    match role {
        Role::Writer => shared | OpenFlags::SQLITE_OPEN_CREATE,
        Role::Reader => shared,
    }
}

const PAGES: &str = "pragma page_count";
const INCREMENTAL_AT_FIRST_WRITE: &str = "pragma auto_vacuum = incremental";

/// Sets the writer up and says the journal mode the file is in.
fn configure_writer(connection: &Connection, config: &Config) -> rusqlite::Result<String> {
    // page_size and auto_vacuum take effect only before the first table, so
    // they come first.
    connection.pragma_update(None, "page_size", config.page_size)?;
    if config.incremental_vacuum && connection.query_row(PAGES, [], |row| row.get::<_, i64>(0))? == 0 {
        // Compiling the pragma sets the mode the file's first write makes it
        // with; running it would write page 1 on its own as well. On a file
        // with pages it is never run: it would commit page 1 at every open,
        // a write beside the store that a node's log takes for a gap.
        drop(connection.prepare(INCREMENTAL_AT_FIRST_WRITE)?);
    }
    let mode: String = connection.pragma_update_and_check(None, "journal_mode", "wal", |row| row.get(0))?;
    let synchronous = match config.durability {
        Durability::Full => "full",
        Durability::Os => "normal",
    };
    connection.pragma_update(None, "synchronous", synchronous)?;
    // Without it macOS's fsync leaves the data in the drive's cache.
    if cfg!(target_os = "macos") {
        connection.pragma_update(None, "fullfsync", true)?;
        connection.pragma_update(None, "checkpoint_fullfsync", true)?;
    }
    configure_shared(connection, config)?;
    Ok(mode)
}

fn configure_reader(connection: &Connection, config: &Config) -> rusqlite::Result<()> {
    connection.pragma_update(None, "query_only", true)?;
    // A statement stops once the call stepping it was cancelled or timed out:
    // the handler reads the tokens of the thread that steps it, its caller's.
    connection.progress_handler(crate::cancel::STEPS, Some(crate::cancel::stopped))?;
    configure_shared(connection, config)
}

fn configure_shared(connection: &Connection, config: &Config) -> rusqlite::Result<()> {
    connection.pragma_update(None, "foreign_keys", true)?;
    connection.pragma_update(None, "trusted_schema", false)?;
    if config.keep_wal {
        // The last connection to close would checkpoint frames its log may
        // not hold durably yet.
        connection.set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)?;
    }
    // A negative cache_size is KiB rather than pages.
    connection.pragma_update(None, "cache_size", -i64::from(config.cache_kib))
}

/// Copies the SQLite file at `source` into `target`, a file not there yet,
/// as one moment of it while its writer keeps going: `vacuum into`, through
/// a connection of its own, since it attaches its target, which no
/// connection of the store's may, and writes it, which a reader's
/// `query_only` refuses. The copy holds every row and no free page.
pub(crate) fn copy(source: &Path, target: &Path) -> Result<()> {
    let what = || format!("copy {} to {}", source.display(), target.display());
    let into = target.to_str().ok_or_else(|| Error::invalid(format!("{}: a path that is no UTF-8", what())))?;
    super::give_mutexes();
    let connection =
        Connection::open_with_flags(source, flags(Role::Reader)).map_err(|error| sql_error(what(), error))?;
    connection.busy_timeout(Config::default().busy_timeout).map_err(|error| sql_error(what(), error))?;
    connection.execute(COPY, [into]).map_err(|error| sql_error(what(), error))?;
    Ok(())
}

const COPY: &str = "vacuum into ?1";

/// Reads a copy [`copy`] made, which nothing else has open.
pub(crate) fn read_copy<T>(path: &Path, read: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T> {
    let what = || format!("read the copy {}", path.display());
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX | OpenFlags::SQLITE_OPEN_EXRESCODE;
    let connection = Connection::open_with_flags(path, flags).map_err(|error| sql_error(what(), error))?;
    read(&connection).map_err(|error| sql_error(what(), error))
}

/// Reads the SQLite file at `path` once, through a connection of its own,
/// whether a connection of the store's has it open or not: for what the
/// store asks of a file no engine may have opened yet. The connection writes
/// nothing, not even the checkpoint SQLite runs as the last one closes, which
/// would move a node's frames into the file before its log holds them.
#[cfg(feature = "kv")]
pub(crate) fn read_file<T>(path: &Path, read: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T> {
    let what = || format!("read {}", path.display());
    super::give_mutexes();
    let open = || -> rusqlite::Result<Connection> {
        let connection = Connection::open_with_flags(path, flags(Role::Reader))?;
        connection.set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)?;
        connection.busy_timeout(Config::default().busy_timeout)?;
        connection.pragma_update(None, "query_only", true)?;
        Ok(connection)
    };
    let connection = open().map_err(|error| sql_error(what(), error))?;
    read(&connection).map_err(|error| sql_error(what(), error))
}

/// Runs a statement of the adapter's own that returns no rows, compiled once
/// a connection.
pub(crate) fn execute(connection: &Connection, sql: &str) -> rusqlite::Result<()> {
    debug_assert!(sql.starts_with(OWN), "{sql}: the adapter's own statement is written with own!");
    connection.prepare_cached(sql)?.execute([]).map(|_| ())
}

/// A name as SQL quotes it whole, its quotes doubled: `"created_at"`.
pub(crate) fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Runs an engine's statement, compiled once a connection, and says how many
/// rows it changed; a failure is named `what`.
pub(crate) fn execute_cached(
    connection: &Connection,
    sql: &str,
    params: impl rusqlite::Params,
    what: &str,
) -> crate::Result<usize> {
    let ran = connection.prepare_cached(sql).and_then(|mut statement| statement.execute(params));
    ran.map_err(|error| sql_error(what, error))
}

/// The rows an engine's statement gives, compiled once a connection, each
/// read by `row`; a failure is named `what`.
pub(crate) fn query_cached<T, C: FromIterator<T>>(
    connection: &Connection,
    (sql, what): (&str, &str),
    params: impl rusqlite::Params,
    row: impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
) -> crate::Result<C> {
    let read = || -> rusqlite::Result<C> { connection.prepare_cached(sql)?.query_map(params, row)?.collect() };
    read().map_err(|error| sql_error(what, error))
}

impl Role {
    fn name(self) -> &'static str {
        match self {
            Self::Writer => "writer",
            Self::Reader => "reader",
        }
    }
}

/// An SQLite failure as the error kind a caller acts on.
pub(crate) fn sql_error(what: impl Into<String>, error: rusqlite::Error) -> Error {
    error_of(what, error, ErrorKind::Internal)
}

/// A failure of a statement the program wrote, its SQLite plain error
/// `invalid`: a syntax error, a table or a column that is not there, a
/// full-text query FTS5 cannot read are the statement's own, not the store's.
pub(crate) fn given_sql_error(what: impl Into<String>, error: rusqlite::Error) -> Error {
    error_of(what, error, ErrorKind::Invalid)
}

/// `plain` is what SQLite's plain `SQLITE_ERROR` means to the caller.
fn error_of(what: impl Into<String>, error: rusqlite::Error, plain: ErrorKind) -> Error {
    let kind = match &error {
        rusqlite::Error::SqliteFailure(failure, _) | rusqlite::Error::SqlInputError { error: failure, .. } => {
            kind_of(failure, plain)
        }
        rusqlite::Error::QueryReturnedNoRows => ErrorKind::NotFound,
        // What the file holds is not what the schema says it holds.
        rusqlite::Error::FromSqlConversionFailure(..)
        | rusqlite::Error::InvalidColumnType(..)
        | rusqlite::Error::IntegralValueOutOfRange(..) => ErrorKind::Corrupt,
        _ => ErrorKind::Internal,
    };
    super::name_constraint(Error::new(kind, what), &error).with_source(error)
}

/// A unique or a primary key already held is `conflict`; any other
/// constraint, a check, a not null or a foreign key, is `invalid`.
fn kind_of(failure: &rusqlite::ffi::Error, plain: ErrorKind) -> ErrorKind {
    use rusqlite::ffi;
    let held = [ffi::SQLITE_CONSTRAINT_UNIQUE, ffi::SQLITE_CONSTRAINT_PRIMARYKEY, ffi::SQLITE_CONSTRAINT_ROWID];
    match failure.code {
        ErrorCode::ConstraintViolation if held.contains(&failure.extended_code) => ErrorKind::Conflict,
        ErrorCode::ConstraintViolation => ErrorKind::Invalid,
        ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked => ErrorKind::Unavailable,
        ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase => ErrorKind::Corrupt,
        ErrorCode::DiskFull | ErrorCode::TooBig => ErrorKind::Limit,
        ErrorCode::SystemIoFailure | ErrorCode::CannotOpen | ErrorCode::PermissionDenied => ErrorKind::Io,
        ErrorCode::ReadOnly => ErrorKind::Invalid,
        ErrorCode::OperationInterrupted => ErrorKind::Cancelled,
        ErrorCode::Unknown => plain,
        _ => ErrorKind::Internal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn broken(sql: &str) -> Error {
        // as the adapter does before a connection of its own, so that this one
        // does not start SQLite with other mutexes than the adapter's
        super::super::give_mutexes();
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE parent (id INTEGER PRIMARY KEY);
                 CREATE TABLE t (id INTEGER PRIMARY KEY, email TEXT NOT NULL UNIQUE,
                     total INTEGER CHECK (total > 0), parent INTEGER REFERENCES parent (id));
                 INSERT INTO t VALUES (1, 'a', 1, NULL);
                 PRAGMA foreign_keys = ON;",
            )
            .unwrap();
        sql_error("t", connection.execute_batch(sql).unwrap_err())
    }

    #[test]
    fn only_a_key_already_held_is_a_conflict() {
        assert_eq!(broken("INSERT INTO t VALUES (2, 'a', 1, NULL)").kind(), ErrorKind::Conflict, "unique");
        assert_eq!(broken("INSERT INTO t VALUES (1, 'b', 1, NULL)").kind(), ErrorKind::Conflict, "primary key");
        assert_eq!(broken("INSERT INTO t VALUES (2, NULL, 1, NULL)").kind(), ErrorKind::Invalid, "not null");
        assert_eq!(broken("INSERT INTO t VALUES (2, 'b', 0, NULL)").kind(), ErrorKind::Invalid, "check");
        assert_eq!(broken("INSERT INTO t VALUES (2, 'b', 1, 9)").kind(), ErrorKind::Invalid, "foreign key");
    }
}
