use std::path::Path;
use std::sync::Once;

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
    Ok(connection)
}

/// Leaves the connection its one file and its settings: no statement
/// attaches another, the store's own included, and a statement the store was
/// given changes nothing the connection's next user would meet.
fn keep_to_its_file(connection: &Connection) -> rusqlite::Result<()> {
    connection.set_limit(Limit::SQLITE_LIMIT_ATTACHED, 0)?;
    given::guard(connection)
}

/// Says once what the SQLite linked in costs readers, where it was built or
/// started without what the workspace gives it: a program that builds the
/// crate elsewhere finds out from its log, not from a profile.
fn warn_of_a_slow_build() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        if super::memory::shares_page_cache() {
            tracing::warn!(
                target: "tinystore",
                "SQLite was built with SQLITE_ENABLE_MEMORY_MANAGEMENT, so every connection shares one page cache \
                 behind one mutex and readers stop one another: build with \
                 LIBSQLITE3_FLAGS=\"SQLITE_DQS=0 -USQLITE_ENABLE_MEMORY_MANAGEMENT\""
            );
        }
        if !super::mutexes_given() {
            tracing::warn!(
                target: "tinystore",
                "SQLite had started before the store opened and keeps its own mutexes, which park a thread at once: \
                 a few threads reading one file wait on each other; open the store before the program's own SQLite"
            );
        }
    });
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

/// Sets the writer up and says the journal mode the file is in.
fn configure_writer(connection: &Connection, config: &Config) -> rusqlite::Result<String> {
    // page_size takes effect only before the first table, so it comes first.
    connection.pragma_update(None, "page_size", config.page_size)?;
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
    configure_shared(connection, config)
}

fn configure_shared(connection: &Connection, config: &Config) -> rusqlite::Result<()> {
    connection.pragma_update(None, "foreign_keys", true)?;
    connection.pragma_update(None, "trusted_schema", false)?;
    // A negative cache_size is KiB rather than pages.
    connection.pragma_update(None, "cache_size", -i64::from(config.cache_kib))
}

/// Runs a statement of the adapter's own that returns no rows, compiled once
/// a connection.
pub(crate) fn execute(connection: &Connection, sql: &str) -> rusqlite::Result<()> {
    debug_assert!(sql.starts_with(OWN), "{sql}: the adapter's own statement is written with own!");
    connection.prepare_cached(sql)?.execute([]).map(|_| ())
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
    let kind = match &error {
        rusqlite::Error::SqliteFailure(failure, _) => kind_of(failure.code),
        rusqlite::Error::QueryReturnedNoRows => ErrorKind::NotFound,
        // What the file holds is not what the schema says it holds.
        rusqlite::Error::FromSqlConversionFailure(..)
        | rusqlite::Error::InvalidColumnType(..)
        | rusqlite::Error::IntegralValueOutOfRange(..) => ErrorKind::Corrupt,
        _ => ErrorKind::Internal,
    };
    Error::new(kind, what).with_source(error)
}

fn kind_of(code: ErrorCode) -> ErrorKind {
    match code {
        ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked => ErrorKind::Unavailable,
        ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase => ErrorKind::Corrupt,
        ErrorCode::DiskFull | ErrorCode::TooBig => ErrorKind::Limit,
        ErrorCode::SystemIoFailure | ErrorCode::CannotOpen | ErrorCode::PermissionDenied => ErrorKind::Io,
        ErrorCode::ConstraintViolation => ErrorKind::Conflict,
        ErrorCode::ReadOnly => ErrorKind::Invalid,
        ErrorCode::OperationInterrupted => ErrorKind::Cancelled,
        _ => ErrorKind::Internal,
    }
}
