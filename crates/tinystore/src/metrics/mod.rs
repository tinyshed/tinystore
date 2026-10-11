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
//! stats.ingest(&[Series::gauge("cpu").label("host", "web-1").sample(SystemTime::now(), 0.42)])?;
//! let cpu = stats.query().name("cpu").since(Duration::from_hours(1)).read()?;
//! assert_eq!(cpu[0].labels()["host"], "web-1");
//! # Ok(())
//! # }
//! ```

mod binary;
mod budget;
mod clock;
mod codec;
mod directory;
mod engine;
mod exact;
#[cfg(test)]
mod fixture;
mod head;
mod ingest;
mod labels;
mod ladder;
mod maintain;
mod matching;
mod names;
mod pack;
mod publish;
mod query;
mod read;
mod registry;
mod residuals;
mod retention;
mod series;
mod summary;
mod timestamp;
mod values;

use std::fmt;
use std::sync::Arc;

pub use maintain::{Maintenance, maintain};
pub use query::{Condition, Not, Prefix, Query, Values};
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

    /// A query of series by name and labels, over a range of time.
    pub fn query(&self) -> Query {
        Query::of(Arc::clone(&self.engine))
    }
}
