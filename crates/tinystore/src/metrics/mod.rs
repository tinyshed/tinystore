//! Numbers that move over time: a program's requests, its queue's length, how
//! long its calls take, and samples other programs send, kept exactly in
//! `metrics.db` and read back by series and range.
//!
//! ```no_run
//! # use std::time::{Duration, SystemTime};
//! # fn main() -> tinystore::Result<()> {
//! use tinystore::metrics::Series;
//! let store = tinystore::Store::open("data", Default::default())?;
//! let stats = store.metrics()?;
//! let requests = stats.counter("http_requests_total").open()?;
//! requests.with(&[("route", "/notes")]).inc();
//! stats.ingest(&[Series::gauge("cpu").label("host", "web-1").sample(SystemTime::now(), 0.42)])?;
//! let cpu = stats.query().name("cpu").since(Duration::from_hours(1)).read()?;
//! assert_eq!(cpu[0].labels()["host"], "web-1");
//! # Ok(())
//! # }
//! ```
//!
//! A program measures itself with instruments, whose totals the store keeps
//! and writes every 15 seconds; samples from elsewhere come by `ingest`.
//! Both write series, and every read finds both.

mod aggregate;
mod binary;
mod bucket;
mod buckets;
mod budget;
mod clock;
mod codec;
mod counting;
mod damage;
mod delete;
mod describe;
mod directory;
mod engine;
mod exact;
mod explain;
#[cfg(test)]
pub(crate) mod fixture;
mod flush;
#[cfg(any(test, feature = "fuzzing"))]
#[doc(hidden)]
pub mod fuzzing;
mod grid;
mod head;
mod ingest;
mod instruments;
mod labels;
mod ladder;
mod latest;
mod listing;
mod maintain;
mod matching;
mod names;
mod own;
mod pack;
mod prometheus;
mod publish;
mod query;
mod rank;
mod read;
mod recorder;
mod registry;
#[cfg(feature = "wire")]
mod remote;
mod residuals;
mod retention;
mod series;
mod snapshot;
mod spread;
mod summary;
#[cfg(feature = "system")]
mod system;
mod timer;
mod timestamp;
mod totals;
mod values;

use std::fmt;
use std::sync::Arc;

pub use aggregate::{Aggregate, Bucket, Op, Value};
pub use buckets::{Buckets, exponential_buckets};
pub use damage::Damage;
pub use describe::Description;
pub use explain::{Exceeds, Plan};
pub use instruments::{Builder, Counter, Gauge, GaugeFunc, Histogram, Reading, Timer, Timing};
pub use latest::Latest;
pub use listing::{LabelPage, List, NameInfo, NamePage, ValuePage};
pub use maintain::{Maintenance, maintain};
pub use own::SelfMetrics;
pub(crate) use own::measure_the_store;
pub use prometheus::Prometheus;
pub use query::{Condition, Not, Prefix, Query, Values};
pub use recorder::Recorder;
#[cfg(feature = "wire")]
pub(crate) use remote::{Ack, Batch, Change, Declared, Remote, Set};
pub use series::{Kind, Labels, Sample, Series};

use crate::{Result, Store};

impl Store {
    /// The store's metrics: series written by `ingest`, read back by `query`.
    /// The engine opens with the first call.
    pub fn metrics(&self) -> Result<Metrics> {
        Ok(Metrics { engine: engine::Engine::of(self)? })
    }
}

/// The store's metrics, a handle that clones cheaply.
#[derive(Clone)]
pub struct Metrics {
    engine: Arc<engine::Engine>,
}

impl fmt::Debug for Metrics {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Metrics")
    }
}

impl Metrics {
    /// Writes series and their samples, every one or none: a refused series
    /// names itself by its labels, so the call can be sent again without it.
    /// A time given twice keeps the value given last, until the store has
    /// packed that stretch of its series; it returns once the samples are
    /// written, and a read finds them at once.
    pub fn ingest(&self, series: &[Series]) -> Result<()> {
        self.engine.ingest(series)
    }

    /// An ingest's series checked and gathered, before the call reserves the
    /// store's memory their write holds: an awaited ingest waits for those
    /// bytes on no thread, then writes them with `ingest_checked`.
    #[cfg(feature = "async")]
    pub(crate) fn check(&self, series: &[Series]) -> Result<ingest::Checked> {
        self.engine.check(series)
    }

    #[cfg(feature = "async")]
    pub(crate) fn ingest_checked(&self, checked: ingest::Checked, held: crate::Reservation) -> Result<()> {
        self.engine.ingest_checked(checked, held)
    }

    /// The store's memory, which an awaited ingest waits for on no thread.
    #[cfg(feature = "async")]
    pub(crate) fn memory(&self) -> &Arc<crate::Memory> {
        &self.engine.memory
    }

    /// A query of series by name and labels, over a range of time, which
    /// `read`, `aggregate`, `latest` or `explain` then runs.
    pub fn query(&self) -> Query {
        Query::of(Arc::clone(&self.engine))
    }

    /// The store these metrics are of, while a handle of it is held: an
    /// async handle made from them runs its calls on that store's pool.
    #[cfg(feature = "async")]
    pub(crate) fn store(&self) -> Option<Store> {
        self.engine.store.upgrade()
    }

    #[cfg(all(test, feature = "async"))]
    pub(crate) fn file(&self) -> &crate::sqlite::File {
        &self.engine.file
    }
}
