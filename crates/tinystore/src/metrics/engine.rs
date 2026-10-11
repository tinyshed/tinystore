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

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use rusqlite::types::FromSql;
use rusqlite::{Connection, Params};

use super::codec::Codecs;
use super::names::Names;
use crate::engine::{Claim, Host, Wake};
use crate::sqlite::{Config, File, Migration, sql_error};
use crate::store::WeakStore;
use crate::{Clock, Durability, Memory, Result, Store, unix_millis};

const MIGRATIONS: &[Migration] = &[Migration::of(1, "0001_schema.sql", include_str!("migrations/0001_schema.sql"))];

/// How often maintenance runs on its own: what names keep no longer is
/// removed, and what is ready is packed.
const MAINTENANCE: Duration = Duration::from_mins(1);
/// How far ahead of the store's clock a sample's time may be: a later one
/// would hold its series' watermark in the future.
pub(super) const AHEAD: Duration = Duration::from_mins(10);

pub(crate) struct Engine {
    store: WeakStore,
    pub(super) file: File,
    pub(super) memory: Arc<Memory>,
    clock: Arc<dyn Clock>,
    pub(super) names: Names,
    /// The zstd contexts calls borrow and give back.
    pub(super) codecs: Codecs,
    /// Held by a pass of maintenance, so that two never pack the same series.
    pub(super) maintaining: Mutex<()>,
    /// What runs maintenance sooner, once a series has a block's worth.
    wake: Mutex<Option<Wake>>,
    _claim: Claim,
}

impl crate::engine::Engine for Engine {
    fn close(&self) -> Result<()> {
        self.file.close()
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
                store: store.downgrade(),
                names: Names::read(&file)?,
                file,
                memory: Arc::clone(store.memory()),
                clock: store.clock(),
                codecs: Codecs::default(),
                maintaining: Mutex::default(),
                wake: Mutex::default(),
                _claim: claim,
            });
            let weak = Arc::downgrade(&engine);
            let wake = store.every("metrics: maintenance", MAINTENANCE, move || {
                weak.upgrade().map_or(Ok(()), |engine| engine.maintain_in_background())
            })?;
            *lock(&engine.wake) = Some(wake);
            Ok(engine)
        })
    }

    /// The store's clock, in milliseconds since 1970.
    pub(super) fn now(&self) -> i64 {
        unix_millis(self.clock.now())
    }

    /// The store, while it is open.
    #[expect(dead_code, reason = "instruments write the store's totals through it, from the slice that builds them")]
    pub(super) fn store(&self) -> Option<Store> {
        self.store.upgrade()
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
