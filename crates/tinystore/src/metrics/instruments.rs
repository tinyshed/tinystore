//! Instruments: what a program measures itself with, its counters, gauges,
//! timers and histograms, whose totals the store keeps and writes.
//!
//! ```no_run
//! # fn f(stats: tinystore::metrics::Metrics) -> tinystore::Result<()> {
//! let requests = stats.counter("http_requests_total").help("Requests served.").open()?;
//! requests.with(&[("route", "/notes"), ("status", "200")]).inc();
//!
//! let latency = stats.timer("http_request_ms").buckets(&[5.0, 10.0, 25.0, 50.0, 100.0]).open()?;
//! let _timing = latency.with(&[("route", "/users")]).start(); // recorded when it drops
//! # Ok(())
//! # }
//! ```
//!
//! A call never waits and never writes: it changes a number in memory, and
//! the store writes every total every 15 seconds, at `flush` and when it
//! closes. A call returns nothing, as the `metrics` crate's are: what it
//! refuses — a NaN, an infinity, a counter's negative add, a label of the
//! store's own — changes nothing and is said once, in the store's log.

use std::collections::BTreeMap;
use std::fmt;
use std::marker::PhantomData;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::Metrics;
use super::buckets::{Bounds, Buckets};
use super::engine::Engine;
use super::labels::Kept;
use super::names::checked_keep;
#[cfg(feature = "wire")]
use super::remote::Declared;
use super::series::{Kind, Labels};
use super::totals::{Form, Instrument, LOCAL, Read, Slot, Told};
use crate::sqlite::sql_error;
use crate::{Error, Result};

/// What a name's unit and help hold at most.
pub(super) const MOST_UNIT: usize = 32;
pub(super) const MOST_HELP: usize = 1024;
/// A timer's sum and longest are milliseconds.
const MILLISECONDS: &str = "ms";

const KINDS: &str = "select name, kind from names where name in (select value from json_each(?1))";

/// What a gauge's function gives: its value, or its value or why it has
/// none.
pub trait Reading {
    fn value(self) -> std::result::Result<f64, String>;
}

impl Reading for f64 {
    fn value(self) -> std::result::Result<f64, String> {
        Ok(self)
    }
}

impl<E: fmt::Display> Reading for std::result::Result<f64, E> {
    fn value(self) -> std::result::Result<f64, String> {
        self.map_err(|error| error.to_string())
    }
}

/// An instrument being opened: its name and what it is told, which `open`
/// checks against what the store and its file hold.
#[must_use = "an instrument opens at `open`"]
pub struct Builder<T> {
    engine: Arc<Engine>,
    name: String,
    form: Form,
    help: Option<String>,
    unit: Option<String>,
    keep: Option<Duration>,
    buckets: Option<Buckets>,
    read: Option<Read>,
    /// The set of labels a gauge's function reads.
    labels: Labels,
    owner: u64,
    /// One of the store's own, whose name may start with `tinystore_`.
    own: bool,
    made: PhantomData<fn() -> T>,
}

impl<T> fmt::Debug for Builder<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Builder")
            .field("name", &self.name)
            .field("form", &self.form)
            .field("help", &self.help)
            .field("unit", &self.unit)
            .field("keep", &self.keep)
            .field("buckets", &self.buckets)
            .finish()
    }
}

impl Metrics {
    /// A total that only goes up, which the store keeps: processes counting
    /// into one series count into one total.
    pub fn counter(&self, name: impl Into<String>) -> Builder<Counter> {
        Builder::of(&self.engine, name.into(), Form::Counter)
    }

    /// A value that goes up and down: a queue's length, a temperature.
    pub fn gauge(&self, name: impl Into<String>) -> Builder<Gauge> {
        Builder::of(&self.engine, name.into(), Form::Gauge)
    }

    /// A gauge whose value `read` gives at each write. It runs on the
    /// thread that writes, the store's own, and must not block: a value
    /// that takes time to read is a gauge the program sets.
    pub fn gauge_func<R: Reading>(
        &self,
        name: impl Into<String>,
        read: impl Fn() -> R + Send + Sync + 'static,
    ) -> Builder<GaugeFunc> {
        let mut builder = Builder::of(&self.engine, name.into(), Form::Gauge);
        builder.read = Some(Box::new(move || read().value()));
        builder
    }

    /// How many things took how long: their count, their sum and the
    /// longest, in milliseconds, and with `buckets` their spread.
    pub fn timer(&self, name: impl Into<String>) -> Builder<Timer> {
        Builder::of(&self.engine, name.into(), Form::Timer)
    }

    /// A timer of numbers that are not durations: a response's size.
    pub fn histogram(&self, name: impl Into<String>) -> Builder<Histogram> {
        Builder::of(&self.engine, name.into(), Form::Histogram)
    }

    /// Writes every instrument's total now, as the store does every 15
    /// seconds and when it closes.
    pub fn flush(&self) -> Result<()> {
        self.engine.write_totals()
    }
}

impl<T> Builder<T> {
    fn of(engine: &Arc<Engine>, name: String, form: Form) -> Builder<T> {
        Builder {
            engine: Arc::clone(engine),
            name,
            form,
            help: None,
            unit: None,
            keep: None,
            buckets: None,
            read: None,
            labels: Labels::new(),
            owner: LOCAL,
            own: false,
            made: PhantomData,
        }
    }

    /// A line on what it means, 1024 bytes at most.
    pub fn help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }

    /// How long its name's samples stay, as `ingest` takes it.
    pub fn keep(mut self, keep: Duration) -> Self {
        self.keep = Some(keep);
        self
    }

    /// One of the store's own instruments.
    pub(super) fn own(mut self) -> Self {
        self.own = true;
        self
    }

    /// The instrument, checked: its name, what it is told, and that no
    /// other instrument or kind holds its series.
    fn opened(&mut self) -> Result<Arc<Instrument>> {
        let engine = Arc::clone(&self.engine);
        let opened = engine.open_instrument(self);
        opened.map_err(|error| error.within(format!("metrics {}", self.name)))
    }

    fn set_unit(mut self, unit: impl Into<String>) -> Self {
        self.unit = Some(unit.into());
        self
    }

    fn set_buckets(mut self, buckets: Buckets) -> Self {
        self.buckets = Some(buckets);
        self
    }
}

macro_rules! unit {
    ($($made:ty),*) => {$(
        impl Builder<$made> {
            /// The unit of its values, 32 bytes at most: `bytes`, `%`.
            pub fn unit(self, unit: impl Into<String>) -> Self {
                self.set_unit(unit)
            }
        }
    )*};
}
unit!(Counter, Gauge, GaugeFunc, Histogram);

macro_rules! buckets {
    ($($made:ty),*) => {$(
        impl Builder<$made> {
            /// Its buckets: the upper bounds of its spread, ascending, each a
            /// number of 0 or more, to which the store adds `+Inf`, or
            /// [`Buckets::Exponential`], a grid the store keeps.
            pub fn buckets(self, buckets: impl Into<Buckets>) -> Self {
                self.set_buckets(buckets.into())
            }
        }
    )*};
}
buckets!(Timer, Histogram);

impl Builder<Counter> {
    pub fn open(mut self) -> Result<Counter> {
        let instrument = self.opened()?;
        let slot = instrument.base()?;
        Ok(Counter { instrument, slot })
    }
}

impl Builder<Gauge> {
    pub fn open(mut self) -> Result<Gauge> {
        let instrument = self.opened()?;
        let slot = instrument.base()?;
        Ok(Gauge { instrument, slot, owner: self.owner })
    }

    /// A gauge a connection sets, which leaves the totals when it does.
    #[cfg(test)]
    pub(super) fn owned_by(mut self, connection: &super::totals::Connection) -> Self {
        self.owner = connection.id;
        self
    }
}

impl Builder<GaugeFunc> {
    /// The set of labels its function reads; another set of the name takes
    /// a function of its own, and a set is read by one.
    pub fn labels(mut self, labels: &[(&str, &str)]) -> Self {
        self.labels = labels.iter().map(|(label, value)| ((*label).to_owned(), (*value).to_owned())).collect();
        self
    }

    /// Opens the gauge; its function is read from the next write on, while
    /// the store is open.
    pub fn open(mut self) -> Result<()> {
        self.opened().map(drop)
    }
}

impl Builder<Timer> {
    pub fn open(mut self) -> Result<Timer> {
        let instrument = self.opened()?;
        let slot = instrument.base()?;
        Ok(Timer { instrument, slot })
    }
}

impl Builder<Histogram> {
    pub fn open(mut self) -> Result<Histogram> {
        let instrument = self.opened()?;
        let slot = instrument.base()?;
        Ok(Histogram { instrument, slot })
    }
}

impl Engine {
    /// Opens an instrument a client declared over the wire, as a Rust one
    /// opens.
    #[cfg(feature = "wire")]
    pub(super) fn open_declared(self: &Arc<Engine>, declared: &Declared, form: Form) -> Result<Arc<Instrument>> {
        let mut builder: Builder<()> = Builder::of(self, declared.name.clone(), form);
        builder.help.clone_from(&declared.help);
        builder.unit.clone_from(&declared.unit);
        builder.keep = declared.keep;
        builder.buckets.clone_from(&declared.buckets);
        builder.opened()
    }

    /// Opens an instrument: its bounds and description checked, the kinds
    /// the file holds of its names, the instrument the store holds of its
    /// name, and its names settled with their keep.
    fn open_instrument<T>(&self, builder: &mut Builder<T>) -> Result<Arc<Instrument>> {
        if !builder.own {
            refuse_the_stores_own(&builder.name)?;
        }
        let bounds = builder.buckets.as_ref().map(Bounds::of).transpose()?;
        check_description(builder)?;
        let keep = builder.keep.map(checked_keep).transpose()?;
        let written = builder.form.written(&builder.name, bounds.is_some());
        for (name, _) in &written {
            Kept::of(name, &Labels::new())?;
        }
        self.check_kinds(&written)?;
        // Settled before the instrument is registered: a keep refused here
        // leaves no instrument claiming the names it would write.
        let keeps = written.iter().map(|(name, _)| (name.as_str(), keep)).collect();
        self.names.settle(&self.file, &keeps)?;
        let instrument = match builder.read.take() {
            Some(read) => {
                self.totals.reader(&builder.name, &builder.labels, read)?;
                Arc::new(Instrument::new(&builder.name, Form::Gauge, None))
            }
            None => self.totals.instrument(&builder.name, builder.form, bounds.clone())?,
        };
        self.describe_written(builder, &written, bounds.as_ref());
        Ok(instrument)
    }

    /// Refuses names the file holds series of another kind under: a name
    /// has one kind while it has series.
    fn check_kinds(&self, written: &[(String, Kind)]) -> Result<()> {
        let names: Vec<&str> = written.iter().map(|(name, _)| name.as_str()).collect();
        let names = serde_json::to_string(&names).map_err(|error| Error::internal("names").with_source(error))?;
        let failed = |error| sql_error("read the kinds of names", error);
        let held: BTreeMap<String, Option<String>> = self.file.read(|connection| {
            let mut select = connection.prepare_cached(KINDS).map_err(failed)?;
            let rows = select.query_map([names], |row| Ok((row.get(0)?, row.get(1)?)));
            rows.and_then(Iterator::collect).map_err(failed)
        })?;
        for (name, kind) in written {
            if let Some(Some(held)) = held.get(name)
                && held != kind.as_str()
            {
                return Err(Error::invalid(format!(
                    "{name}: a {kind}, where the name's series are {held}s: a name has one kind while it has series"
                )));
            }
        }
        Ok(())
    }

    /// Keeps what each name an instrument writes says of itself for the
    /// next write: a timer's sum, longest and buckets in milliseconds.
    fn describe_written<T>(&self, builder: &Builder<T>, written: &[(String, Kind)], bounds: Option<&Bounds>) {
        let unit = match builder.form {
            Form::Timer => Some(MILLISECONDS.to_owned()),
            _ => builder.unit.clone(),
        };
        for (name, kind) in written {
            let counts = builder.form != Form::Counter && *kind == Kind::Counter && name.ends_with("_count");
            let description = Told {
                unit: if counts { None } else { unit.clone() },
                help: builder.help.clone(),
                buckets: (*kind == Kind::Histogram).then(|| bounds.map(Bounds::text)).flatten(),
            };
            if description != Told::default() {
                self.totals.describe(name, description);
            }
        }
    }
}

/// Checks a unit and a help against their bounds.
fn check_description<T>(builder: &Builder<T>) -> Result<()> {
    if builder.unit.as_ref().is_some_and(|unit| unit.len() > MOST_UNIT) {
        return Err(Error::invalid(format!("a unit of {MOST_UNIT} bytes at most")));
    }
    if builder.help.as_ref().is_some_and(|help| help.len() > MOST_HELP) {
        return Err(Error::invalid(format!("a help of {MOST_HELP} bytes at most")));
    }
    Ok(())
}

/// A total that only goes up, which the store keeps.
#[derive(Clone)]
pub struct Counter {
    instrument: Arc<Instrument>,
    slot: Arc<Slot>,
}

impl Counter {
    /// The same counter with more labels, a series of its own; a label
    /// given again takes its new value, and the same labels in any order
    /// are one series.
    pub fn with(&self, labels: &[(&str, &str)]) -> Counter {
        Counter { instrument: Arc::clone(&self.instrument), slot: self.instrument.with(&self.slot, labels) }
    }

    pub fn inc(&self) {
        self.add(1.0);
    }

    /// What a total the program keeps itself grew since the last it gave,
    /// or all of it when it is smaller, a restart's.
    pub(super) fn advance_to(&self, total: u64) -> u64 {
        self.slot.advance_to(total)
    }

    /// Adds `n`, a finite number of 0 or more; `add(0.0)` makes the set of
    /// labels known with a total of zero.
    pub fn add(&self, n: f64) {
        if !(n.is_finite() && n >= 0.0) {
            self.slot.refuse(&format!("a counter's add of {n}: a counter grows by a finite number of 0 or more"));
        } else if !self.slot.add(n) {
            self.slot.refuse("a counter's total past what a float holds");
        }
    }
}

/// A value that goes up and down, written while the process that sets it
/// is connected.
#[derive(Clone)]
pub struct Gauge {
    instrument: Arc<Instrument>,
    slot: Arc<Slot>,
    owner: u64,
}

impl Gauge {
    /// The same gauge with more labels, a series of its own.
    pub fn with(&self, labels: &[(&str, &str)]) -> Gauge {
        Gauge {
            instrument: Arc::clone(&self.instrument),
            slot: self.instrument.with(&self.slot, labels),
            owner: self.owner,
        }
    }

    /// Sets its value, a finite number.
    pub fn set(&self, value: f64) {
        if value.is_finite() {
            self.slot.set(value, self.owner);
        } else {
            self.slot.refuse(&format!("a gauge set to {value}: a gauge is a finite number"));
        }
    }

    /// Moves its value by `delta`, from zero when it was never set.
    pub fn add(&self, delta: f64) {
        if !(delta.is_finite() && self.slot.shift(delta, self.owner)) {
            self.slot.refuse(&format!("a gauge moved by {delta}: a gauge is a finite number"));
        }
    }

    pub fn inc(&self) {
        self.add(1.0);
    }

    pub fn dec(&self) {
        self.add(-1.0);
    }
}

/// How many things took how long: their count, their sum and the longest
/// since the write before, in milliseconds, and their spread over its
/// buckets.
#[derive(Clone)]
pub struct Timer {
    instrument: Arc<Instrument>,
    slot: Arc<Slot>,
}

impl Timer {
    /// The same timer with more labels, a series of its own; `le` is its
    /// buckets'.
    pub fn with(&self, labels: &[(&str, &str)]) -> Timer {
        Timer { instrument: Arc::clone(&self.instrument), slot: self.instrument.with(&self.slot, labels) }
    }

    /// Records a duration measured elsewhere.
    pub fn record(&self, duration: Duration) {
        let milliseconds = duration.as_nanos() as f64 / 1e6;
        self.slot.measure(milliseconds, self.instrument.bounds.as_ref());
    }

    /// A timing that records how long it lived when it drops, for a span
    /// that is no one function: kept in a name, `let _timing =
    /// latency.start()`, since `let _ =` drops it at once.
    pub fn start(&self) -> Timing {
        Timing { timer: self.clone(), started: Instant::now() }
    }

    /// Runs `work` and records how long it took, a panic's unwinding too.
    pub fn measure<T>(&self, work: impl FnOnce() -> T) -> T {
        let _timing = self.start();
        work()
    }
}

/// A duration being measured, recorded when it drops.
#[must_use = "a timing records when it drops: keep it in a name, `let _timing = latency.start()`"]
pub struct Timing {
    timer: Timer,
    started: Instant,
}

impl Drop for Timing {
    fn drop(&mut self) {
        self.timer.record(self.started.elapsed());
    }
}

/// A timer of numbers that are not durations, each 0 or more.
#[derive(Clone)]
pub struct Histogram {
    instrument: Arc<Instrument>,
    slot: Arc<Slot>,
}

impl Histogram {
    /// The same histogram with more labels, a series of its own; `le` is
    /// its buckets'.
    pub fn with(&self, labels: &[(&str, &str)]) -> Histogram {
        Histogram { instrument: Arc::clone(&self.instrument), slot: self.instrument.with(&self.slot, labels) }
    }

    /// Records a value, a finite number of 0 or more.
    pub fn record(&self, value: f64) {
        if value.is_finite() && value >= 0.0 {
            self.slot.measure(value, self.instrument.bounds.as_ref());
        } else {
            self.slot.refuse(&format!("a histogram's value of {value}: a finite number of 0 or more"));
        }
    }
}

/// What `gauge_func` opens: a gauge its function reads at each write.
#[derive(Debug)]
pub struct GaugeFunc {
    _made: (),
}

macro_rules! debug_as_series {
    ($($handle:ty),*) => {$(
        impl fmt::Debug for $handle {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.debug_tuple(stringify!($handle)).field(&self.slot.kept.to_string()).finish()
            }
        }
    )*};
}
debug_as_series!(Counter, Gauge, Timer, Histogram);

impl fmt::Debug for Timing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Timing").field("timer", &self.timer).field("started", &self.started).finish()
    }
}

#[cfg(test)]
#[path = "instruments_tests.rs"]
mod tests;

/// Refuses a name of the store's own, `tinystore_…`, from the program.
pub(super) fn refuse_the_stores_own(name: &str) -> Result<()> {
    if name.starts_with(super::own::PREFIX) {
        return Err(Error::invalid(format!(
            "{name}: a name that starts with {} is the store's own, which it writes when opened with self_metrics",
            super::own::PREFIX
        )));
    }
    Ok(())
}
