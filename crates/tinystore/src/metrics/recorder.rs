//! The `metrics` crate's recorder: its macros count into the store's
//! instruments, as the logger's layer makes `tracing`'s macros log.
//!
//! ```no_run
//! # fn f(store: tinystore::Store) -> tinystore::Result<()> {
//! let recorder = store.metrics()?.recorder().buckets("http_request_ms", &[5.0, 25.0, 100.0]);
//! metrics::set_global_recorder(recorder).expect("one global recorder a process");
//! metrics::counter!("http_requests_total", "route" => "/notes").increment(1);
//! metrics::gauge!("queue_depth").set(12.0);
//! metrics::histogram!("http_request_ms").record(42.0);
//! # Ok(())
//! # }
//! ```
//!
//! A counter is a counter, a gauge a gauge, and a histogram a histogram of
//! the unit `describe_histogram!` gives, with the buckets the recorder was
//! given for its name. A macro returns nothing: a name the store refuses,
//! one of another kind, is said once and counts nowhere.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use ::metrics::{
    Counter as Counted, CounterFn, Gauge as Gauged, GaugeFn, Histogram as Spread, HistogramFn, Key, KeyName, Metadata,
    SharedString, Unit,
};

use super::Metrics;
use super::buckets::Buckets;
use super::engine::lock;
use super::instruments::{Counter, Gauge, Histogram, MOST_HELP, MOST_UNIT};
use super::totals::Told;

/// A builder told what `describe_*!` said of its name, what passes a
/// name's bounds left out rather than refusing the name.
macro_rules! told {
    ($builder:expr, $said:expr) => {{
        let said: Told = $said;
        let mut builder = $builder;
        if let Some(help) = said.help.filter(|help| help.len() <= MOST_HELP) {
            builder = builder.help(help);
        }
        if let Some(unit) = said.unit.filter(|unit| unit.len() <= MOST_UNIT) {
            builder = builder.unit(unit);
        }
        builder
    }};
}

/// The `metrics` crate's recorder for a store's metrics.
pub struct Recorder {
    stats: Metrics,
    buckets: HashMap<String, Buckets>,
    opened: Mutex<HashMap<String, Opened>>,
    /// The names a macro of another kind was called on, said once each.
    mixed: Mutex<std::collections::HashSet<String>>,
    /// What `describe_*!` said of a name before it was opened.
    described: Mutex<HashMap<String, Told>>,
}

impl std::fmt::Debug for Recorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Recorder").field("buckets", &self.buckets).finish_non_exhaustive()
    }
}

/// A name the recorder opened, or that the store refused.
enum Opened {
    Counter(Counter),
    Gauge(Gauge),
    Histogram(Histogram),
    Refused,
}

impl Metrics {
    /// A recorder for the `metrics` crate, whose macros then count into
    /// these metrics.
    pub fn recorder(&self) -> Recorder {
        Recorder {
            stats: self.clone(),
            buckets: HashMap::new(),
            opened: Mutex::default(),
            mixed: Mutex::default(),
            described: Mutex::default(),
        }
    }
}

impl Recorder {
    /// The buckets of the histogram of `name`, as a timer's are.
    #[must_use]
    pub fn buckets(mut self, name: impl Into<String>, buckets: impl Into<Buckets>) -> Recorder {
        self.buckets.insert(name.into(), buckets.into());
        self
    }

    /// The instrument of `name`, opened the first time as `open` makes it;
    /// one the store refuses, or of another kind, is said once and none.
    fn opened<T>(
        &self,
        name: &str,
        open: impl FnOnce(Told) -> crate::Result<Opened>,
        pick: impl Fn(&Opened) -> Option<T>,
    ) -> Option<T> {
        let mut opened = lock(&self.opened);
        if let Some(held) = opened.get(name) {
            let picked = pick(held);
            // The handle stays: only the call of the other kind counts nowhere.
            if picked.is_none() && !matches!(held, Opened::Refused) && lock(&self.mixed).insert(name.to_owned()) {
                let why = "a macro of another kind than it was opened as";
                tracing::warn!(target: "tinystore", "metrics {name}: {why}; said once");
            }
            return picked;
        }
        let description = lock(&self.described).remove(name).unwrap_or_default();
        let made = open(description).unwrap_or_else(|error| {
            tracing::warn!(target: "tinystore", %error, "metrics {name}: the recorder counts it nowhere; said once");
            Opened::Refused
        });
        let picked = pick(&made);
        opened.insert(name.to_owned(), made);
        picked
    }

    fn counter(&self, name: &str) -> Option<Counter> {
        let open = |said: Told| told!(self.stats.counter(name), said).open().map(Opened::Counter);
        self.opened(name, open, |held| match held {
            Opened::Counter(counter) => Some(counter.clone()),
            _ => None,
        })
    }

    fn gauge(&self, name: &str) -> Option<Gauge> {
        let open = |said: Told| told!(self.stats.gauge(name), said).open().map(Opened::Gauge);
        self.opened(name, open, |held| match held {
            Opened::Gauge(gauge) => Some(gauge.clone()),
            _ => None,
        })
    }

    fn histogram(&self, name: &str) -> Option<Histogram> {
        let open = |said: Told| {
            let mut builder = told!(self.stats.histogram(name), said);
            if let Some(buckets) = self.buckets.get(name) {
                builder = builder.buckets(buckets.clone());
            }
            builder.open().map(Opened::Histogram)
        };
        self.opened(name, open, |held| match held {
            Opened::Histogram(histogram) => Some(histogram.clone()),
            _ => None,
        })
    }

    /// Keeps what `describe_*!` says of a name: for its opening, or for
    /// the next write once it is open.
    fn describe(&self, name: &KeyName, unit: Option<Unit>, help: &SharedString) {
        let description = Told {
            unit: unit.and_then(unit_text).map(str::to_owned),
            help: Some(help.to_string()).filter(|help| !help.is_empty()),
            buckets: None,
        };
        let name = name.as_str();
        let opened = lock(&self.opened);
        let written = match opened.get(name) {
            None => {
                lock(&self.described).insert(name.to_owned(), description);
                return;
            }
            Some(Opened::Histogram(_)) => {
                // a count is no unit: it takes the help alone
                let count = Told { unit: None, ..description.clone() };
                self.stats.engine.totals.describe(&format!("{name}_count"), count);
                vec![format!("{name}_sum"), format!("{name}_max"), format!("{name}_bucket")]
            }
            Some(_) => vec![name.to_owned()],
        };
        for name in written {
            self.stats.engine.totals.describe(&name, description.clone());
        }
    }
}

/// A unit as a name keeps it: what programs write, `ms`, `%`, `bytes`; a
/// count is no unit.
fn unit_text(unit: Unit) -> Option<&'static str> {
    Some(match unit {
        Unit::Count => return None,
        Unit::Percent => "%",
        Unit::Seconds => "s",
        Unit::Milliseconds => "ms",
        Unit::Microseconds => "us",
        Unit::Nanoseconds => "ns",
        other => other.as_str(),
    })
}

/// A key's labels as an instrument's `with` takes them.
fn labels(key: &Key) -> Vec<(&str, &str)> {
    key.labels().map(|label| (label.key(), label.value())).collect()
}

impl ::metrics::Recorder for Recorder {
    fn describe_counter(&self, key: KeyName, unit: Option<Unit>, description: SharedString) {
        self.describe(&key, unit, &description);
    }

    fn describe_gauge(&self, key: KeyName, unit: Option<Unit>, description: SharedString) {
        self.describe(&key, unit, &description);
    }

    fn describe_histogram(&self, key: KeyName, unit: Option<Unit>, description: SharedString) {
        self.describe(&key, unit, &description);
    }

    fn register_counter(&self, key: &Key, _: &Metadata<'_>) -> Counted {
        let Some(counter) = self.counter(key.name()) else { return Counted::noop() };
        Counted::from_arc(Arc::new(Recorded(counter.with(&labels(key)))))
    }

    fn register_gauge(&self, key: &Key, _: &Metadata<'_>) -> Gauged {
        let Some(gauge) = self.gauge(key.name()) else { return Gauged::noop() };
        Gauged::from_arc(Arc::new(Recorded(gauge.with(&labels(key)))))
    }

    fn register_histogram(&self, key: &Key, _: &Metadata<'_>) -> Spread {
        let Some(histogram) = self.histogram(key.name()) else { return Spread::noop() };
        Spread::from_arc(Arc::new(Recorded(histogram.with(&labels(key)))))
    }
}

/// An instrument as the `metrics` crate's handles call it.
struct Recorded<T>(T);

impl CounterFn for Recorded<Counter> {
    fn increment(&self, value: u64) {
        self.0.add(value as f64);
    }

    /// A total the program keeps itself: what it grew since the last is
    /// added, and a smaller one, a restart's, is added whole, as a reset
    /// counts.
    fn absolute(&self, value: u64) {
        self.0.add(self.0.advance_to(value) as f64);
    }
}

impl GaugeFn for Recorded<Gauge> {
    fn increment(&self, value: f64) {
        self.0.add(value);
    }

    fn decrement(&self, value: f64) {
        self.0.add(-value);
    }

    fn set(&self, value: f64) {
        self.0.set(value);
    }
}

impl HistogramFn for Recorded<Histogram> {
    fn record(&self, value: f64) {
        self.0.record(value);
    }
}

#[cfg(test)]
#[path = "recorder_tests.rs"]
mod tests;
