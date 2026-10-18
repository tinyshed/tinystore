//! A series as a program writes it and reads it back: a name, its kind, its
//! labels, and its samples.

use std::collections::BTreeMap;
use std::fmt;
use std::time::{Duration, SystemTime};

/// The labels of a series beside its name, `route` → `/notes`: text, a number
/// given as its text.
pub type Labels = BTreeMap<String, String>;

/// What a series' samples are.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    /// A value that goes up and down: a queue's length, a temperature.
    Gauge,
    /// A total that only goes up, and starts again from zero at a reset.
    Counter,
    /// The `_bucket` series of a histogram written elsewhere: a counter of
    /// what fell at most its `le`.
    Histogram,
}

impl Kind {
    /// The kind as the file, the wire and every SDK say it.
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Gauge => "gauge",
            Kind::Counter => "counter",
            Kind::Histogram => "histogram",
        }
    }

    pub(super) fn parse(word: &str) -> Option<Kind> {
        [Kind::Gauge, Kind::Counter, Kind::Histogram].into_iter().find(|kind| kind.as_str() == word)
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A time and a number, kept bit for bit: `-0`, infinities and a NaN's
/// payload come back as they went in. A time is kept to the millisecond.
#[derive(Clone, Copy, Debug)]
pub struct Sample {
    pub at: SystemTime,
    pub value: f64,
}

/// A series: what `ingest` writes, built a label and a sample at a time, and
/// what a read gives back.
///
/// ```
/// # use std::time::{Duration, UNIX_EPOCH};
/// use tinystore::metrics::Series;
/// let at = UNIX_EPOCH + Duration::from_secs(1_800_000_000);
/// let cpu = Series::gauge("cpu").label("host", "web-1").sample(at, 0.42);
/// assert_eq!(cpu.to_string(), r#"cpu{host="web-1"}"#);
/// ```
#[derive(Clone, Debug)]
pub struct Series {
    pub(super) name: String,
    pub(super) kind: Kind,
    pub(super) labels: Labels,
    pub(super) samples: Vec<Sample>,
    pub(super) keep: Option<Duration>,
}

impl Series {
    pub fn gauge(name: impl Into<String>) -> Series {
        Series::of(name.into(), Kind::Gauge)
    }

    pub fn counter(name: impl Into<String>) -> Series {
        Series::of(name.into(), Kind::Counter)
    }

    /// The `_bucket` series of a histogram written elsewhere, one series per
    /// bound, each with its label `le`, beside its `_sum` and `_count`
    /// counters.
    pub fn histogram(name: impl Into<String>) -> Series {
        Series::of(name.into(), Kind::Histogram)
    }

    pub(super) fn of(name: String, kind: Kind) -> Series {
        Series { name, kind, labels: Labels::new(), samples: Vec::new(), keep: None }
    }

    /// One label more; a number or a boolean is kept as its text, `200` as
    /// `"200"`.
    pub fn label(mut self, name: impl Into<String>, value: impl fmt::Display) -> Series {
        self.labels.insert(name.into(), value.to_string());
        self
    }

    pub fn sample(mut self, at: SystemTime, value: f64) -> Series {
        self.samples.push(Sample { at, value });
        self
    }

    /// How long the name's samples stay: the name's own, kept in the file.
    /// Given none, the name keeps what it has, 30 days until one is given;
    /// another than one given before while the store is open is `invalid`.
    pub fn keep(mut self, keep: Duration) -> Series {
        self.keep = Some(keep);
        self
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn kind(&self) -> Kind {
        self.kind
    }

    pub fn labels(&self) -> &Labels {
        &self.labels
    }

    pub fn samples(&self) -> &[Sample] {
        &self.samples
    }
}

impl Extend<Sample> for Series {
    fn extend<I: IntoIterator<Item = Sample>>(&mut self, samples: I) {
        self.samples.extend(samples);
    }
}

impl Extend<(SystemTime, f64)> for Series {
    fn extend<I: IntoIterator<Item = (SystemTime, f64)>>(&mut self, samples: I) {
        self.samples.extend(samples.into_iter().map(|(at, value)| Sample { at, value }));
    }
}

impl fmt::Display for Series {
    /// As Prometheus prints a series: `cpu{host="web-1"}`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        super::labels::format(f, &self.name, self.labels.iter())
    }
}
