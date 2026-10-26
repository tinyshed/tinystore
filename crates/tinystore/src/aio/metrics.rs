//! The async API's handles of the Metrics engine, beside the blocking ones of
//! [`crate::metrics`]: the same names, builders and verbs, each call that
//! reads or writes `metrics.db` awaited.
//!
//! ```no_run
//! # async fn example() -> tinystore::Result<()> {
//! use std::time::{Duration, SystemTime};
//! use tinystore::Options;
//! use tinystore::aio::Store;
//! use tinystore::aio::metrics::{Op, Series};
//!
//! let store = Store::open("data", Options::default())?;
//! let stats = store.metrics()?; // opening stays a plain call
//! let requests = stats.counter("http_requests_total").open()?; // so do instruments: memory only
//! requests.with(&[("route", "/notes")]).inc();
//!
//! stats.ingest(&[Series::gauge("cpu").label("host", "web-1").sample(SystemTime::now(), 0.42)]).await?;
//! let cpu = stats.query().name("cpu").since(Duration::from_hours(1)).read().await?;
//! let p95 = stats
//!     .query()
//!     .name("http_request_ms")
//!     .since(Duration::from_hours(1))
//!     .every(Duration::from_mins(5))
//!     .aggregate(Op::Percentile(95.0))
//!     .await?;
//! store.close().await?;
//! # Ok(())
//! # }
//! ```
//!
//! What a call takes and gives back, a [`Series`], an [`Aggregate`], a
//! [`NamePage`] or a [`Description`], is the blocking module's own type,
//! re-exported here, and so are the instruments: a [`Counter`], a [`Gauge`]
//! or a [`Timer`] counts in memory, and its calls are plain.
//!
//! | A call                              | Runs                                            | Dropped once polled |
//! |-------------------------------------|-------------------------------------------------|---------------------|
//! | `ingest` and [`Prometheus::ingest`] | on the store's pool, its memory on no thread    | runs to its end     |
//! | `describe`, `delete`, `flush`       | blocking, on the store's pool                   | runs to its end     |
//! | a [`Query`]'s four calls            | checked in the first poll, then on the pool     | stops               |
//! | a [`List`]'s `page`, `damaged`      | on the store's pool                             | stops               |
//! | `description`                       | in the first poll, a reader idle; else the pool | answered, or stops  |
//!
//! None commits, syncs the disk or waits on the thread that polls its
//! future. A write runs blocking on a thread of the pool, which waits there
//! for its commit; once polled it runs to its end whether or not anyone
//! still awaits it, and a future made and dropped unpolled writes nothing.
//! An ingest that waits for the store's memory waits on no thread, so that
//! ingests waiting together never take the pool's threads from the reads. A
//! read whose future is dropped stops where it is, waiting for a reader or
//! for memory, or running, and gives back what it held;
//! `tokio::time::timeout` around it stops it so. A query's `since` becomes an
//! absolute start in its first poll, and so does the time of a text's
//! samples that give none.

use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use super::run;
use crate::Result;
use crate::cancel;
use crate::pool::Pool;

pub use crate::metrics::{
    Aggregate, AggregatePage, Bucket, Buckets, Builder, Condition, Counter, Damage, Description, Exceeds, Gauge,
    GaugeFunc, Histogram, Kind, LabelPage, Labels, Latest, NameInfo, NamePage, Not, Op, Plan, Prefix, Reading,
    Recorder, Sample, Series, SeriesPage, Timer, Timing, Value, ValuePage, Values, exponential_buckets,
};

/// What an awaited ingest names in its errors, as its blocking twin names
/// its wait for memory.
const INGEST: &str = "metrics: an ingest";

impl super::Store {
    /// The store's metrics, as [`crate::Store::metrics`] gives them: a plain
    /// call, the first of which opens the engine and migrates `metrics.db`.
    pub fn metrics(&self) -> Result<Metrics> {
        Ok(Metrics { blocking: self.blocking().metrics()?, pool: Arc::clone(self.pool()) })
    }
}

/// The store's metrics, their calls awaited: the same engine and series as
/// [`crate::metrics::Metrics`], a handle that clones cheaply.
#[derive(Clone)]
pub struct Metrics {
    blocking: crate::metrics::Metrics,
    pool: Arc<Pool>,
}

impl From<crate::metrics::Metrics> for Metrics {
    /// The same metrics, their calls awaited; once their store is gone,
    /// every awaited call fails `Closed`.
    fn from(blocking: crate::metrics::Metrics) -> Metrics {
        let pool = blocking.store().map_or_else(Pool::stopped, |store| Pool::of_or_stopped(&store));
        Metrics { blocking, pool }
    }
}

impl fmt::Debug for Metrics {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.blocking, f)
    }
}

impl Metrics {
    /// The same metrics, for their blocking calls.
    pub fn blocking(&self) -> &crate::metrics::Metrics {
        &self.blocking
    }

    /// Writes series and their samples, every one or none, as
    /// [`crate::metrics::Metrics::ingest`] does. The series are copied when
    /// the call is made, and checked on a thread of the store's pool; the
    /// store's memory their write holds is awaited on no thread, and the
    /// write runs blocking on a thread of the pool, which waits there for
    /// the commit.
    pub fn ingest(&self, series: &[Series]) -> impl Future<Output = Result<()>> + Send + 'static {
        let (stats, series) = (self.blocking.clone(), series.to_vec());
        // Not a submit: a call that makes a name holds the names' lock until its commit.
        run::holding(&self.pool, self.blocking.memory(), INGEST.to_owned(), move || {
            let checked = stats.check(&series)?;
            Ok((checked.holds(), move |held| stats.ingest_checked(checked, held)))
        })
    }

    /// A query of series by name and labels, over a range of time, which
    /// `read`, `aggregate`, `latest` or `explain` then runs.
    pub fn query(&self) -> Query {
        Query { blocking: self.blocking.query(), pool: Arc::clone(&self.pool) }
    }

    /// What ingests the text an exporter serves, in Prometheus' format or
    /// OpenMetrics': `stats.prometheus().label("instance", "web-1").ingest(&body)`.
    pub fn prometheus(&self) -> Prometheus {
        Prometheus { blocking: self.blocking.prometheus(), stats: self.blocking.clone(), pool: Arc::clone(&self.pool) }
    }

    /// The names the store holds samples of, as
    /// [`crate::metrics::Metrics::names`] lists them.
    pub fn names(&self) -> List<NamePage> {
        self.list(self.blocking.names())
    }

    /// A name's label names, but the store's own.
    pub fn label_names(&self, name: impl Into<String>) -> List<LabelPage> {
        self.list(self.blocking.label_names(name))
    }

    /// The values a label of a name's series holds.
    pub fn label_values(&self, name: impl Into<String>, label: impl Into<String>) -> List<ValuePage> {
        self.list(self.blocking.label_values(name, label))
    }

    /// Sets a name's unit and help in place of what it had, as
    /// [`crate::metrics::Metrics::describe`] does, blocking on a thread of
    /// the store's pool, which waits there for the commit.
    pub fn describe(&self, name: &str, description: Description) -> impl Future<Output = Result<()>> + Send + 'static {
        let (stats, name) = (self.blocking.clone(), name.to_owned());
        let what = format!("metrics {name}: its description");
        // Not a submit: the write holds the names' lock until its commit.
        run::blocking(&self.pool, None, what, move || stats.describe(&name, description))
    }

    /// What a name's unit and help are; both empty when nothing described
    /// it.
    pub fn description(&self, name: &str) -> impl Future<Output = Result<Description>> + Send + 'static {
        let (stats, name) = (self.blocking.clone(), name.to_owned());
        run::point(&self.pool, format!("metrics {name}: its description"), move || stats.description(&name))
    }

    /// The series whose bytes no longer read, as
    /// [`crate::metrics::Metrics::damaged`] lists them.
    pub fn damaged(&self) -> impl Future<Output = Result<Vec<Damage>>> + Send + 'static {
        let stats = self.blocking.clone();
        run::read(&self.pool, "metrics: the damaged series".to_owned(), move || stats.damaged())
    }

    /// Removes the one series of exactly this name and these labels with
    /// every sample it holds, as [`crate::metrics::Metrics::delete`] does,
    /// and says whether the store held it. It runs blocking on a thread of
    /// the store's pool, which waits there for maintenance's pass to end.
    pub fn delete<K: AsRef<str>, V: AsRef<str>>(
        &self,
        name: &str,
        labels: impl IntoIterator<Item = (K, V)>,
    ) -> impl Future<Output = Result<bool>> + Send + 'static {
        let labels: Vec<(String, String)> =
            labels.into_iter().map(|(label, value)| (label.as_ref().to_owned(), value.as_ref().to_owned())).collect();
        let (stats, name) = (self.blocking.clone(), name.to_owned());
        let what = format!("metrics {name}: a delete");
        run::blocking(&self.pool, None, what, move || stats.delete(&name, labels))
    }

    /// Writes every instrument's total now, as the store does every 15
    /// seconds and when it closes, blocking on a thread of the store's pool:
    /// a gauge function reads its value there.
    pub fn flush(&self) -> impl Future<Output = Result<()>> + Send + 'static {
        let stats = self.blocking.clone();
        run::blocking(&self.pool, None, "metrics: a flush".to_owned(), move || stats.flush())
    }

    /// A total that only goes up, which the store keeps, as
    /// [`crate::metrics::Metrics::counter`] opens one: an instrument counts
    /// in memory, so its calls are plain.
    pub fn counter(&self, name: impl Into<String>) -> Builder<Counter> {
        self.blocking.counter(name)
    }

    /// A value that goes up and down: a queue's length, a temperature.
    pub fn gauge(&self, name: impl Into<String>) -> Builder<Gauge> {
        self.blocking.gauge(name)
    }

    /// A gauge whose value `read` gives at each write, on the thread that
    /// writes, the store's own: it must not block.
    pub fn gauge_func<R: Reading>(
        &self,
        name: impl Into<String>,
        read: impl Fn() -> R + Send + Sync + 'static,
    ) -> Builder<GaugeFunc> {
        self.blocking.gauge_func(name, read)
    }

    /// How many things took how long: their count, their sum and the
    /// longest, in milliseconds, and with `buckets` their spread.
    pub fn timer(&self, name: impl Into<String>) -> Builder<Timer> {
        self.blocking.timer(name)
    }

    /// A timer of numbers that are not durations: a response's size.
    pub fn histogram(&self, name: impl Into<String>) -> Builder<Histogram> {
        self.blocking.histogram(name)
    }

    /// A recorder for the `metrics` crate, whose macros then count into
    /// these metrics.
    pub fn recorder(&self) -> Recorder {
        self.blocking.recorder()
    }

    fn list<P>(&self, blocking: crate::metrics::List<P>) -> List<P> {
        List { blocking, pool: Arc::clone(&self.pool) }
    }
}

/// A query being asked, as [`crate::metrics::Query`] builds one: its name,
/// conditions and range, and for an aggregate its buckets and groups.
///
/// A call checks the query in its future's first poll, on the polling
/// thread: `since` becomes an absolute start from the store's clock there,
/// and a query that is invalid fails without taking a thread. The read then
/// runs on the store's pool under a token that the future's drop cancels.
#[derive(Clone)]
pub struct Query {
    blocking: crate::metrics::Query,
    pool: Arc<Pool>,
}

impl fmt::Debug for Query {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.blocking, f)
    }
}

impl Query {
    /// The series of this name.
    pub fn name(self, name: impl Into<String>) -> Self {
        self.with(|query| query.name(name))
    }

    /// A label's value under a condition: a value, a list of them, a
    /// [`Not`] or a [`Prefix`].
    pub fn filter(self, label: impl Into<String>, condition: impl Into<Condition>) -> Self {
        self.with(|query| query.filter(label, condition))
    }

    /// The span before now the range covers; or `from` and `to`.
    pub fn since(self, span: Duration) -> Self {
        self.with(|query| query.since(span))
    }

    /// The range's start, which is in it.
    pub fn from(self, at: SystemTime) -> Self {
        self.with(|query| query.from(at))
    }

    /// The range's end, which is not in it; now unless given.
    pub fn to(self, at: SystemTime) -> Self {
        self.with(|query| query.to(at))
    }

    /// A read in pages, as [`crate::metrics::Query::limit`] has them.
    pub fn limit(self, samples: usize) -> Self {
        self.with(|query| query.limit(samples))
    }

    /// The page after the one whose `next` this is, as
    /// [`crate::metrics::Query::after`] reads it.
    pub fn after(self, next: &str) -> Self {
        self.with(|query| query.after(next))
    }

    /// An aggregate's buckets: spans this long, each beginning at a whole
    /// number of them since 1970 in UTC.
    pub fn every(self, span: Duration) -> Self {
        self.with(|query| query.every(span))
    }

    /// An aggregate's buckets on the calendar of a time zone, as
    /// [`crate::metrics::Query::calendar`] has them.
    pub fn calendar(self, unit: crate::Calendar, zone: &str) -> Self {
        self.with(|query| query.calendar(unit, zone))
    }

    /// The labels an aggregate keeps: the series of a name that share them
    /// are one group.
    pub fn by(self, labels: &[&str]) -> Self {
        self.with(|query| query.by(labels))
    }

    /// The labels an aggregate leaves out, keeping every other.
    pub fn without(self, labels: &[&str]) -> Self {
        self.with(|query| query.without(labels))
    }

    /// A timer's percentile also gives a point between the bounds of the
    /// bucket that holds the rank: an estimate, which no measurement gave.
    pub fn estimate(self) -> Self {
        self.with(crate::metrics::Query::estimate)
    }

    /// The samples of each series the query finds, in its range, whole or a
    /// page of them, as [`crate::metrics::Query::read`] gives them.
    pub fn read(&self) -> impl Future<Output = Result<SeriesPage>> + Send + 'static {
        let query = self.blocking.clone();
        checked_then_read(&self.pool, "metrics: a read", move || query.checked_read())
    }

    /// The buckets of each group of the series the query finds, each one
    /// exact value of `op`, rounded once, as
    /// [`crate::metrics::Query::aggregate`] gives them.
    pub fn aggregate(&self, op: Op) -> impl Future<Output = Result<AggregatePage>> + Send + 'static {
        let query = self.blocking.clone();
        checked_then_read(&self.pool, "metrics: an aggregate", move || query.checked_aggregate(op))
    }

    /// The newest sample of each series the query finds, when it has one in
    /// the range.
    pub fn latest(&self) -> impl Future<Output = Result<Vec<Latest>>> + Send + 'static {
        let query = self.blocking.clone();
        checked_then_read(&self.pool, "metrics: the newest samples", move || query.checked_latest())
    }

    /// What `read`, or `aggregate` when given its `op`, would spend of each
    /// bound of a query, and the bound it would stop at. It decodes no
    /// sample.
    pub fn explain(&self, op: impl Into<Option<Op>>) -> impl Future<Output = Result<Plan>> + Send + 'static {
        let (query, op) = (self.blocking.clone(), op.into());
        checked_then_read(&self.pool, "metrics: a plan", move || query.checked_explain(op))
    }

    fn with(self, change: impl FnOnce(crate::metrics::Query) -> crate::metrics::Query) -> Self {
        Query { blocking: change(self.blocking), pool: self.pool }
    }
}

/// Runs `check` in the future's first poll, on the polling thread, then the
/// read it gives as [`run::read`] runs one: on the store's pool, under a
/// token the future's drop cancels, and under the tokens of the thread that
/// made the future, which `run::read` takes where it is called.
fn checked_then_read<T, C, R>(
    pool: &Arc<Pool>,
    what: &str,
    check: C,
) -> impl Future<Output = Result<T>> + Send + 'static + use<T, C, R>
where
    C: FnOnce() -> Result<R> + Send + 'static,
    R: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    let (pool, what, tokens) = (Arc::clone(pool), what.to_owned(), cancel::captured());
    async move {
        let read = check()?;
        let reading = tokens.run(None, || run::read(&pool, what, read));
        reading.await
    }
}

/// A page of what a store holds, its options before its `page`, as
/// [`crate::metrics::List`] builds one: 1000 unless `limit` says.
#[derive(Clone)]
#[must_use = "a list reads nothing until its page"]
pub struct List<P> {
    blocking: crate::metrics::List<P>,
    pool: Arc<Pool>,
}

impl<P> fmt::Debug for List<P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.blocking, f)
    }
}

impl<P> List<P> {
    /// Only those that start with this text.
    pub fn prefix(self, prefix: impl Into<String>) -> Self {
        List { blocking: self.blocking.prefix(prefix), pool: self.pool }
    }

    /// The page that starts past this one, as a page's `next` says it.
    pub fn after(self, next: impl Into<String>) -> Self {
        List { blocking: self.blocking.after(next), pool: self.pool }
    }

    /// How many a page holds at most, 1 to 10,000.
    pub fn limit(self, most: usize) -> Self {
        List { blocking: self.blocking.limit(most), pool: self.pool }
    }
}

impl List<NamePage> {
    pub fn page(self) -> impl Future<Output = Result<NamePage>> + Send + 'static {
        let list = self.blocking;
        run::read(&self.pool, "metrics: a page of names".to_owned(), move || list.page())
    }
}

impl List<LabelPage> {
    pub fn page(self) -> impl Future<Output = Result<LabelPage>> + Send + 'static {
        let list = self.blocking;
        run::read(&self.pool, "metrics: a page of labels".to_owned(), move || list.page())
    }
}

impl List<ValuePage> {
    pub fn page(self) -> impl Future<Output = Result<ValuePage>> + Send + 'static {
        let list = self.blocking;
        run::read(&self.pool, "metrics: a page of values".to_owned(), move || list.page())
    }
}

/// A text an exporter serves, in Prometheus' format or OpenMetrics', and the
/// labels, the time and the keep its ingest gives every series of it, as
/// [`crate::metrics::Prometheus`] builds one.
#[derive(Clone)]
#[must_use = "a text is written by ingest"]
pub struct Prometheus {
    blocking: crate::metrics::Prometheus,
    stats: crate::metrics::Metrics,
    pool: Arc<Pool>,
}

impl fmt::Debug for Prometheus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.blocking, f)
    }
}

impl Prometheus {
    /// A label every series of the text gets beside its own, such as the
    /// `instance` scraped.
    pub fn label(self, name: impl Into<String>, value: impl fmt::Display) -> Prometheus {
        self.with(|blocking| blocking.label(name, value))
    }

    /// The time of every sample the text gives none: unless given, the
    /// store's clock at the first poll of `ingest`'s future.
    pub fn at(self, at: SystemTime) -> Prometheus {
        self.with(|blocking| blocking.at(at))
    }

    /// How long every name of the text keeps its samples.
    pub fn keep(self, keep: Duration) -> Prometheus {
        self.with(|blocking| blocking.keep(keep))
    }

    /// Reads the text and writes its series, every one or none, as
    /// [`crate::metrics::Prometheus::ingest`] does. The text is copied when
    /// the call is made, and the time of its samples that give none is read
    /// in the future's first poll. The memory its reading and its write hold
    /// is awaited on no thread, and the text is then read and written
    /// blocking on a thread of the store's pool, which waits there for the
    /// commit.
    pub fn ingest(&self, text: impl AsRef<[u8]>) -> impl Future<Output = Result<()>> + Send + 'static {
        let (prometheus, text) = (self.blocking.clone(), text.as_ref().to_vec());
        let (pool, memory) = (Arc::clone(&self.pool), Arc::clone(self.stats.memory()));
        async move {
            let prometheus = prometheus.timed();
            let ingesting = run::holding(&pool, &memory, INGEST.to_owned(), move || {
                Ok((prometheus.holds(&text), move |held| prometheus.ingest_held(&text, held)))
            });
            ingesting.await
        }
    }

    fn with(self, change: impl FnOnce(crate::metrics::Prometheus) -> crate::metrics::Prometheus) -> Prometheus {
        Prometheus { blocking: change(self.blocking), ..self }
    }
}

#[cfg(test)]
#[path = "metrics_tests.rs"]
mod tests;
