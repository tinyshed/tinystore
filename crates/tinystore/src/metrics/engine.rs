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
use std::sync::{Arc, Mutex, OnceLock};
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
#[cfg(feature = "async")]
use crate::store::WeakStore;
use crate::sync::lock;
use crate::{Clock, Durability, Memory, Result, Store, unix_millis};

const MIGRATIONS: &[Migration] = &[Migration::of(1, "0001_schema.sql", include_str!("migrations/0001_schema.sql"))];

/// metrics' file in the store's directory.
const FILE: &str = "metrics.db";

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
    pub(super) clock: Arc<dyn Clock>,
    /// The series the store holds at most, as its options said.
    pub(super) max_series: i64,
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
    /// Run as each write of the totals begins: the store's own gauges read.
    before_write: OnceLock<Box<dyn Fn() + Send + Sync>>,
    /// What the engine tells the store's own metrics.
    pub(super) probe: Arc<crate::probe::Probe>,
    /// The sessions of clients' instruments: the last batch each applied.
    #[cfg(feature = "wire")]
    pub(super) sessions: Sessions,
    closed: AtomicBool,
    /// The store, held weakly as an engine holds it: an async handle made
    /// from a blocking one finds its store's pool through it.
    #[cfg(feature = "async")]
    pub(super) store: WeakStore,
    _claim: Claim,
}

impl Engine {
    /// Runs `work` as each write of the totals begins, before it collects
    /// them; a second is ignored, as the store has one listener.
    pub(super) fn before_each_write(&self, work: Box<dyn Fn() + Send + Sync>) {
        let _ = self.before_write.set(work);
    }

    pub(super) fn before_write(&self) {
        if let Some(work) = self.before_write.get() {
            work();
        }
    }
}

impl crate::engine::Engine for Engine {
    /// Writes the instruments' totals last, then closes the file, which it
    /// closes whatever the write did.
    fn close(&self) -> Result<()> {
        if self.closed.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        // A copy's totals are its home's to write; a closing store's are its
        // own work, which waits for no copy of a synchronous home.
        let written = if self.file.is_copy() { Ok(()) } else { crate::sqlite::own_work(|| self.write_totals()) };
        let closed = self.file.close();
        written.and(closed)
    }

    /// Writes the instruments' totals, then names metrics.db.
    fn snapshot(&self) -> Result<crate::engine::Copying<'_>> {
        if !self.file.is_copy() {
            self.write_totals()?;
        }
        crate::engine::to_copy(&self.file, FILE, "metrics")
    }

    /// A name a copy's home made after the copy read its names is read once
    /// the home's commits arrive.
    fn applied(&self, file: &str) {
        if file != FILE {
            return;
        }
        if let Err(error) = self.names.read_again(&self.file) {
            tracing::warn!(target: "tinystore", %error, "metrics: a copy's names, read again after its home's commit");
        }
    }
}

impl Engine {
    /// The store's metrics engine, opened by its first call: its file claimed
    /// and migrated, its names read.
    pub(crate) fn of(store: &Store) -> Result<Arc<Engine>> {
        store.engine(|store| {
            let claim = store.claim(FILE)?;
            // 'os' unless the store says: a sample a commit is not worth a sync
            let durability = store.durability().or(Some(Durability::Os));
            let config = Config { incremental_vacuum: true, ..Config::committing(durability) };
            let file = store.open_file(claim.name(), config)?;
            file.migrate("metrics", MIGRATIONS)?;
            let engine = Arc::new(Engine {
                names: Names::read(&file)?,
                file,
                memory: Arc::clone(store.memory()),
                totals: Arc::new(Totals::new(unix_millis(store.now()))),
                clock: store.clock(),
                max_series: i64::try_from(store.max_series()).unwrap_or(i64::MAX),
                codecs: Codecs::default(),
                maintaining: Mutex::default(),
                pack_cursor: AtomicI64::new(0),
                wake: Mutex::default(),
                writing: Mutex::default(),
                before_write: OnceLock::new(),
                probe: Arc::clone(store.probe()),
                #[cfg(feature = "wire")]
                sessions: Sessions::default(),
                closed: AtomicBool::new(false),
                #[cfg(feature = "async")]
                store: store.downgrade(),
                _claim: claim,
            });
            // A copy's packing, expiry and instruments are its home's.
            if engine.file.is_copy() {
                return Ok(engine);
            }
            let weak = Arc::downgrade(&engine);
            let wake = store.every("metrics: maintenance", MAINTENANCE, move || {
                weak.upgrade().map_or(Ok(()), |engine| {
                    // a burst of reads otherwise keeps its readers open for good
                    engine.file.sweep();
                    engine.maintain_in_background()
                })
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

pub(super) use crate::sqlite::execute_cached as execute;

/// The one value of a statement's one row.
pub(super) fn query_one<T: FromSql>(connection: &Connection, sql: &str, params: impl Params, what: &str) -> Result<T> {
    let one = connection.prepare_cached(sql).and_then(|mut statement| statement.query_row(params, |row| row.get(0)));
    one.map_err(|error| sql_error(what, error))
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;
