//! The metrics engine: `metrics.db` in the store's directory, the names it
//! keeps and what each name owns.
//!
//! ```text
//! ingest ──► heads: a series' newest samples, rewritten as they come
//!                  │ packing, in the background
//!                  ▼
//!            groups of blocks, each with an exact summary
//! read   ◄── heads and blocks from one snapshot, decoded after it
//! ```

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use rusqlite::types::FromSql;
use rusqlite::{Connection, Params};

use super::codec::Codecs;
use super::names::Names;
#[cfg(feature = "wire")]
use super::remote::Sessions;
use super::totals::Totals;
use crate::engine::{Claim, Host, Wake};
use crate::sqlite::{Config, File, Migration, sql_error};
use crate::{Clock, Durability, Memory, Result, Store, unix_millis};

const MIGRATIONS: &[Migration] = &[Migration::of(1, "0001_schema.sql", include_str!("migrations/0001_schema.sql"))];

/// How often maintenance runs on its own: what names keep no longer is
/// removed, and what is ready is packed.
const MAINTENANCE: Duration = Duration::from_mins(1);
/// How often the instruments' totals are written: a crash of the store
/// loses what it had not written, this long at most.
const WRITE_EVERY: Duration = Duration::from_secs(15);
/// How far ahead of the store's clock a sample's time may be: a later one
/// would hold its series' watermark in the future.
pub(super) const AHEAD: Duration = Duration::from_mins(10);

pub(crate) struct Engine {
    pub(super) file: File,
    pub(super) memory: Arc<Memory>,
    clock: Arc<dyn Clock>,
    pub(super) names: Names,
    /// The zstd contexts calls borrow and give back.
    pub(super) codecs: Codecs,
    /// Held by a pass of maintenance, so that two never pack the same series.
    pub(super) maintaining: Mutex<()>,
    /// The last series a pass packed, which the next continues after.
    pub(super) pack_cursor: AtomicI64,
    /// What runs maintenance sooner, once a series has a block's worth.
    wake: Mutex<Option<Wake>>,
    /// The instruments and the totals the store keeps of them.
    pub(super) totals: Arc<Totals>,
    /// Held by a write of the totals, so that two never write at once.
    pub(super) writing: Mutex<()>,
    /// The sessions of clients' instruments: the last batch each applied.
    #[cfg(feature = "wire")]
    pub(super) sessions: Sessions,
    closed: AtomicBool,
    _claim: Claim,
}

impl crate::engine::Engine for Engine {
    /// Writes the instruments' totals last, then closes the file, which it
    /// closes whatever the write did.
    fn close(&self) -> Result<()> {
        if self.closed.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let written = self.write_totals();
        let closed = self.file.close();
        written.and(closed)
    }
}

impl Engine {
    /// The store's metrics engine, opened by its first call: its file claimed
    /// and migrated, its names read.
    pub(crate) fn of(store: &Store) -> Result<Arc<Engine>> {
        store.engine(|store| {
            let claim = store.claim("metrics.db")?;
            // 'os' unless the store says: a sample a commit is not worth a sync
            let durability = store.durability().or(Some(Durability::Os));
            let file = File::open(claim.path(), Config::committing(durability))?;
            file.migrate("metrics", MIGRATIONS)?;
            let engine = Arc::new(Engine {
                names: Names::read(&file)?,
                file,
                memory: Arc::clone(store.memory()),
                totals: Arc::new(Totals::new(unix_millis(store.now()))),
                clock: store.clock(),
                codecs: Codecs::default(),
                maintaining: Mutex::default(),
                pack_cursor: AtomicI64::new(0),
                wake: Mutex::default(),
                writing: Mutex::default(),
                #[cfg(feature = "wire")]
                sessions: Sessions::default(),
                closed: AtomicBool::new(false),
                _claim: claim,
            });
            let weak = Arc::downgrade(&engine);
            let wake = store.every("metrics: maintenance", MAINTENANCE, move || {
                weak.upgrade().map_or(Ok(()), |engine| engine.maintain_in_background())
            })?;
            *lock(&engine.wake) = Some(wake);
            let weak = Arc::downgrade(&engine);
            store.every("metrics: instruments", WRITE_EVERY, move || {
                weak.upgrade().map_or(Ok(()), |engine| engine.write_totals())
            })?;
            Ok(engine)
        })
    }

    /// The store's clock, in milliseconds since 1970.
    pub(super) fn now(&self) -> i64 {
        unix_millis(self.clock.now())
    }

    /// Runs maintenance soon rather than at its next minute.
    pub(super) fn wake(&self) {
        if let Some(wake) = &*lock(&self.wake) {
            wake.wake();
        }
    }

    /// A pass of maintenance, and another at once while a pass left work.
    fn maintain_in_background(&self) -> Result<()> {
        let (_, unfinished) = self.pass()?;
        if unfinished {
            self.wake();
        }
        Ok(())
    }
}

/// Runs a statement the connection keeps prepared; `what` says it in an
/// error.
pub(super) fn execute(connection: &Connection, sql: &str, params: impl Params, what: &str) -> Result<usize> {
    let ran = connection.prepare_cached(sql).and_then(|mut statement| statement.execute(params));
    ran.map_err(|error| sql_error(what, error))
}

/// The one value of a statement's one row.
pub(super) fn query_one<T: FromSql>(connection: &Connection, sql: &str, params: impl Params, what: &str) -> Result<T> {
    let one = connection.prepare_cached(sql).and_then(|mut statement| statement.query_row(params, |row| row.get(0)));
    one.map_err(|error| sql_error(what, error))
}

pub(super) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
