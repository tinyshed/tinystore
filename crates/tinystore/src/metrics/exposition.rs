//! The newest sample of every series, written as Prometheus' text format,
//! 0.0.4, which a Prometheus scrapes: a family's `# HELP` and `# TYPE` from
//! what the store keeps of its name, then a line a series.
//!
//! ```no_run
//! # fn f(stats: tinystore::metrics::Metrics) -> tinystore::Result<()> {
//! let mut body = Vec::new();
//! let written = stats.exposition().prefix("shop_").write(&mut body)?;
//! println!("{} series of {} names", written.series, written.names);
//! # Ok(())
//! # }
//! ```
//!
//! ```text
//! # HELP http_requests_total Requests served.
//! # TYPE http_requests_total counter
//! http_requests_total{route="/notes",status="200"} 1027 1732622400000
//! # TYPE rpc_seconds histogram
//! rpc_seconds_bucket{le="0.1"} 240 1732622400000
//! rpc_seconds_sum 41.7 1732622400000
//! rpc_seconds_count 320 1732622400000
//! ```
//!
//! A counter and a gauge are families of their own. A histogram is what the
//! store keeps of one, its `_bucket` series, the `_sum` and `_count` beside
//! them written in its family; a timer writes those three and `_max`, which
//! is a gauge of its own. The store keeps no quantile of a summary: what an
//! ingest made of one, gauges and counters, is written as those. A `_bucket`
//! whose family's name the store holds as a name of its own is `untyped`,
//! since one family cannot be two, and its `_sum` and `_count` counters.
//!
//! Every series is written with its newest sample's value and, unless
//! `timestamps(false)` leaves them out, its time; the value spelt so that a
//! reader parses the same float back: `-0`, `+Inf`, `-Inf` and `NaN`, whose
//! payload no text spells. A unit goes on a `# UNIT` line, which Prometheus'
//! own reader passes over as a comment and the store's reads; a histogram's
//! family carries its `_sum`'s, the one part a family's unit is read back
//! onto. A unit of more than one word is left out, since the line has no
//! escapes.
//!
//! The 0.0.4 format spells a name with letters, digits, `_` and `:` alone,
//! and a label without the `:`; a reader of it refuses the whole text past
//! them. Any other character is written `_`, and a leading digit gets a `_`
//! before it, as OpenTelemetry's exporter writes them:
//!
//! ```text
//! app.load{url.scheme="https"}     app_load{url_scheme="https"}
//! 2xx_total                        _2xx_total
//! ```
//!
//! A name so spelt that the store holds, or that another name spelt so wrote
//! first, would be one family twice, and a series whose labels so spelt are
//! two of one name would be refused: neither is written, and `skipped` counts
//! their series.
//!
//! The exposition is not one moment of the file: names are read a page at a
//! time and each name's series a thousand at a time, each part one snapshot,
//! so that it holds a page of names and one part of a name's series however
//! many the store keeps; the newest time it reads up to is the clock once,
//! when it starts. A series past every bound of a read is a `limit` error,
//! never a shorter text.

use std::collections::{BTreeSet, HashSet};
use std::fmt::Write as _;
use std::io::Write;
use std::sync::Arc;
use std::time::Duration;

use rusqlite::params;

use super::Metrics;
use super::engine::Engine;
use super::labels::NAME;
use super::latest::Latest;
use super::listing::NameInfo;
use super::query::{Asked, Condition};
use super::series::Kind;
use crate::sqlite::sql_error;
use crate::{Error, Result, unix_millis};

/// The names a page of the walk lists.
const NAMES_AT_ONCE: usize = 1000;

/// Whether the store holds a name.
const HOLDS: &str = "select 1 from names where name = ?1";

/// The kinds and units of a histogram's family name, and of its `_sum` and
/// `_count`.
const PARTS: &str =
    "select name, kind, unit from names where kind is not null and name in (?1, ?1 || '_sum', ?1 || '_count')";

/// What writes the newest sample of every series the store holds, its
/// options before its `write`.
#[derive(Clone)]
#[must_use = "an exposition writes nothing until its write"]
pub struct Exposition {
    metrics: Metrics,
    prefix: String,
    since: Option<Duration>,
    timestamps: bool,
}

impl std::fmt::Debug for Exposition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut shown = f.debug_struct("Exposition");
        shown.field("prefix", &self.prefix).field("since", &self.since).field("timestamps", &self.timestamps);
        shown.finish()
    }
}

/// What an exposition wrote: the store's names with a series in the text,
/// and their series; and the series it could not spell apart from another's,
/// which it left out.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Written {
    pub names: u64,
    pub series: u64,
    pub skipped: u64,
}

impl Metrics {
    /// The newest sample of every series the store holds, as Prometheus'
    /// text format: `stats.exposition().write(&mut out)`.
    pub fn exposition(&self) -> Exposition {
        Exposition { metrics: self.clone(), prefix: String::new(), since: None, timestamps: true }
    }
}

impl Exposition {
    /// Only the names that start with this text.
    pub fn prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefix = prefix.into();
        self
    }

    /// Only the series whose newest sample is at most this old; without it
    /// every series the store still keeps, however long it has been silent.
    pub fn since(mut self, span: Duration) -> Self {
        self.since = Some(span);
        self
    }

    /// Whether each line carries its sample's time, as it does unless this
    /// turns them off: node_exporter's textfile collector and the
    /// Pushgateway refuse a sample with a time of its own, and a Prometheus
    /// drops a silent series' old one as out of bounds.
    pub fn timestamps(mut self, on: bool) -> Self {
        self.timestamps = on;
        self
    }

    /// Writes the text to `out`, a part of a name's series at a time, and says
    /// how many names and series it wrote. Every series is written or the
    /// call fails, `out` then holding the lines before the failure.
    pub fn write(&self, out: &mut impl Write) -> Result<Written> {
        self.walk(out).map_err(|error| error.within("metrics: an exposition"))
    }

    fn walk(&self, out: &mut impl Write) -> Result<Written> {
        let engine = &self.metrics.engine;
        engine.write_before_read(None)?;
        let now = engine.now();
        let from = self.since.map_or(i64::MIN, |span| now.saturating_sub(crate::clock::millis(span)));
        let mut writing = Writing::new(Arc::clone(engine), out, self, (from, now));
        let mut after: Option<String> = None;
        loop {
            let list = self.metrics.names().prefix(&self.prefix).limit(NAMES_AT_ONCE);
            let page = after.map_or(list.clone(), |after| list.after(after)).page()?;
            for info in &page.names {
                writing.name(info)?;
            }
            match page.next {
                Some(next) => after = Some(next),
                None => break,
            }
        }
        writing.out.flush().map_err(failed)?;
        Ok(writing.written)
    }
}

fn failed(error: std::io::Error) -> Error {
    Error::io("write the text", error)
}

/// What a family's `# TYPE` says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Type {
    Counter,
    Gauge,
    Histogram,
    Untyped,
}

impl Type {
    fn word(self) -> &'static str {
        match self {
            Type::Counter => "counter",
            Type::Gauge => "gauge",
            Type::Histogram => "histogram",
            Type::Untyped => "untyped",
        }
    }
}

/// A family whose lines have not begun: its comments are written before its
/// first series, so that a family without one writes nothing.
struct Family<'a> {
    name: &'a str,
    of: Type,
    help: &'a str,
    unit: &'a str,
}

/// An exposition being written: the lines of the part of a name's series it
/// is at, the names of histograms' parts already written in their family,
/// and the spellings of names written other than they are.
struct Writing<'o, W> {
    engine: Arc<Engine>,
    out: &'o mut W,
    prefix: String,
    timestamps: bool,
    from: i64,
    now: i64,
    text: String,
    parts: BTreeSet<String>,
    respelt: HashSet<String>,
    written: Written,
}

impl<'o, W: Write> Writing<'o, W> {
    fn new(engine: Arc<Engine>, out: &'o mut W, asked: &Exposition, (from, now): (i64, i64)) -> Writing<'o, W> {
        let (prefix, timestamps) = (asked.prefix.clone(), asked.timestamps);
        let (text, parts, respelt, written) = (String::new(), BTreeSet::new(), HashSet::new(), Written::default());
        Writing { engine, out, prefix, timestamps, from, now, text, parts, respelt, written }
    }

    /// A name's family and its series, unless a histogram's family wrote it.
    fn name(&mut self, info: &NameInfo) -> Result<()> {
        if self.parts.remove(&info.name) {
            return Ok(());
        }
        let own = |of| Family { name: &info.name, of, help: &info.help, unit: &info.unit };
        match info.kind {
            Kind::Counter => self.family(own(Type::Counter), &[&info.name]),
            Kind::Gauge => self.family(own(Type::Gauge), &[&info.name]),
            Kind::Histogram => self.histogram(info),
        }
    }

    /// A `_bucket` name's family, with its `_sum` and `_count` counters when
    /// the store holds them, and the unit of the `_sum`; `untyped` when its
    /// family's name is a name of the store's own, or it has none.
    fn histogram(&mut self, info: &NameInfo) -> Result<()> {
        let stem = info.name.strip_suffix("_bucket").filter(|stem| !stem.is_empty());
        let held = match stem {
            Some(stem) => self.held_parts(stem)?,
            None => None,
        };
        let (Some(stem), Some((parts, unit))) = (stem, held) else {
            let untyped = Family { name: &info.name, of: Type::Untyped, help: &info.help, unit: &info.unit };
            return self.family(untyped, &[&info.name]);
        };
        let mut names = vec![info.name.as_str()];
        names.extend(parts.iter().filter(|part| part.starts_with(&self.prefix)).map(String::as_str));
        let family = Family { name: stem, of: Type::Histogram, help: &info.help, unit: &unit };
        self.family(family, &names)?;
        self.parts.extend(names[1..].iter().map(|part| (*part).to_owned()));
        Ok(())
    }

    /// The `_sum` and `_count` counters of a histogram's family the store
    /// holds, in that order, and the unit of its `_sum`, which the family's
    /// `# UNIT` gives back; none when the store holds the family's name
    /// itself.
    fn held_parts(&self, stem: &str) -> Result<Option<(Vec<String>, String)>> {
        let failed = |error| sql_error("read a histogram's names", error);
        let held: Vec<(String, String, String)> = self.engine.file.read(|connection| {
            let mut select = connection.prepare_cached(PARTS).map_err(failed)?;
            let rows = select.query_map(params![stem], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)));
            rows.and_then(Iterator::collect).map_err(failed)
        })?;
        if held.iter().any(|(name, _, _)| name == stem) {
            return Ok(None);
        }
        let is_counter = |part: &str| held.iter().find(|(name, kind, _)| name == part && kind == "counter");
        let (sum, count) = (format!("{stem}_sum"), format!("{stem}_count"));
        let unit = is_counter(&sum).map(|(_, _, unit)| unit.clone()).unwrap_or_default();
        let parts = [sum, count].into_iter().filter(|part| is_counter(part).is_some()).collect();
        Ok(Some((parts, unit)))
    }

    /// The series of each of `names`, after the family's comments once one
    /// of them has a series; a name whose spelling another's has, skipped.
    fn family(&mut self, family: Family<'_>, names: &[&str]) -> Result<()> {
        let mut begun = false;
        for name in names {
            let writes = self.spelt_apart(name)?;
            let mut from_id = i64::MIN;
            let mut any = false;
            loop {
                let (page, next) = self.engine.latest_from(&self.asked(name), from_id)?;
                if !writes {
                    self.written.skipped += page.len() as u64;
                } else {
                    for latest in &page {
                        // the family's comments go before its first line, and a series skipped writes none
                        let before = self.text.len();
                        if !begun {
                            comments(&mut self.text, &family);
                        }
                        let wrote = line(&mut self.text, latest, self.timestamps);
                        if !wrote {
                            self.text.truncate(before);
                        }
                        *(if wrote { &mut self.written.series } else { &mut self.written.skipped }) += 1;
                        (begun, any) = (begun || wrote, any || wrote);
                    }
                    self.out.write_all(self.text.as_bytes()).map_err(failed)?;
                    self.text.clear();
                }
                let Some(next) = next else { break };
                from_id = next;
            }
            self.written.names += u64::from(any);
        }
        Ok(())
    }

    /// Whether a name's spelling is its own: the name itself, or a spelling
    /// no name the store holds is and no name written before took.
    fn spelt_apart(&mut self, name: &str) -> Result<bool> {
        let spelt = respelt(name, true);
        if spelt == name {
            return Ok(true);
        }
        let failed = |error| sql_error("read whether a name is held", error);
        let held = self.engine.file.read(|connection| {
            let mut select = connection.prepare_cached(HOLDS).map_err(failed)?;
            select.exists(params![spelt]).map_err(failed)
        })?;
        Ok(!held && self.respelt.insert(spelt))
    }

    /// The query of one name's series, over the exposition's range.
    fn asked(&self, name: &str) -> Asked {
        let conditions = vec![(NAME.to_owned(), Condition::OneOf(vec![name.to_owned()]))];
        let to = self.now.saturating_add(1).max(self.from);
        Asked { name: Some(name.to_owned()), conditions, from: self.from, to, now: self.now }
    }
}

/// A family's `# HELP`, `# UNIT` and `# TYPE`; a unit of more than one
/// word, which the line could not hold apart from what follows, left out.
fn comments(text: &mut String, family: &Family<'_>) {
    let name = respelt(family.name, true);
    if !family.help.is_empty() {
        let help = family.help.replace('\\', r"\\").replace('\n', r"\n");
        let _ = writeln!(text, "# HELP {name} {help}");
    }
    if !family.unit.is_empty() && !family.unit.chars().any(|c| c.is_whitespace() || c.is_control()) {
        let _ = writeln!(text, "# UNIT {name} {}", family.unit);
    }
    let _ = writeln!(text, "# TYPE {name} {}", family.of.word());
}

/// A series' line: its name and labels as the format spells them, its value
/// and, with `timestamps`, its time in milliseconds. A series whose labels
/// so spelt are two of one name writes nothing and says so.
fn line(text: &mut String, latest: &Latest, timestamps: bool) -> bool {
    let labels: Vec<String> = latest.labels.keys().map(|label| respelt(label, false)).collect();
    if labels.iter().collect::<HashSet<_>>().len() < labels.len() {
        return false;
    }
    text.push_str(&respelt(&latest.name, true));
    if !labels.is_empty() {
        text.push('{');
        for (index, (label, (_, value))) in labels.iter().zip(latest.labels.iter()).enumerate() {
            if index > 0 {
                text.push(',');
            }
            text.push_str(label);
            text.push('=');
            quoted(text, value);
        }
        text.push('}');
    }
    let _ = write!(text, " {}", spelt(latest.value));
    if timestamps {
        let _ = write!(text, " {}", unix_millis(latest.at));
    }
    text.push('\n');
    true
}

/// A name as the 0.0.4 format spells it: each character past letters,
/// digits and `_`, a metric's `:` too, written `_`, and a `_` before a
/// leading digit.
fn respelt(name: &str, metric: bool) -> String {
    let kept = |c: char| c.is_ascii_alphanumeric() || c == '_' || (metric && c == ':');
    let mut spelt = String::with_capacity(name.len() + 1);
    if name.starts_with(|c: char| c.is_ascii_digit()) {
        spelt.push('_');
    }
    spelt.extend(name.chars().map(|c| if kept(c) { c } else { '_' }));
    spelt
}

/// A text between double quotes, its `\`, `"` and newlines escaped.
fn quoted(text: &mut String, value: &str) {
    text.push('"');
    for character in value.chars() {
        match character {
            '\\' => text.push_str(r"\\"),
            '"' => text.push_str("\\\""),
            '\n' => text.push_str(r"\n"),
            other => text.push(other),
        }
    }
    text.push('"');
}

/// A value spelt so that a reader parses the same float back: a whole
/// number as one, any other in the shortest form that reads back, and the
/// three that are no number as the format names them.
///
/// ```text
/// 3 → 3    -0 → -0    0.1 → 0.1    1e300 → 1e300    ∞ → +Inf    NaN → NaN
/// ```
fn spelt(value: f64) -> String {
    match value {
        _ if value.is_nan() => "NaN".to_owned(),
        f64::INFINITY => "+Inf".to_owned(),
        f64::NEG_INFINITY => "-Inf".to_owned(),
        _ if value == value.trunc() && value.abs() < 1e15 => format!("{value}"),
        _ => format!("{value:?}"),
    }
}

#[cfg(test)]
#[path = "exposition_tests.rs"]
mod tests;
