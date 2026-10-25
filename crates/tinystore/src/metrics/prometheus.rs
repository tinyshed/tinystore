//! Prometheus' text format and OpenMetrics', as an exporter serves them, read
//! into the series `ingest` writes: each sample line a series of one sample,
//! of the kind its family's `# TYPE` gives it.
//!
//! ```text
//! # TYPE rpc_seconds histogram
//! rpc_seconds_bucket{le="0.1"} 240    histogram  rpc_seconds_bucket{le="0.1"}
//! rpc_seconds_sum 41.7                counter    rpc_seconds_sum
//! rpc_seconds_count 320               counter    rpc_seconds_count
//! ```
//!
//! A text whose last line is `# EOF` is OpenMetrics', whose timestamps are
//! seconds; any other is Prometheus', whose timestamps are milliseconds. The
//! text is read whole before anything is written, so a line it cannot read
//! refuses all of it, naming the line, and it is written whole, in one write
//! of up to 100,000 samples, ten ingests' worth. A family's `# HELP` and
//! `# UNIT` give its names a help and a unit where they have none.
//!
//! A name past the classic characters comes quoted, as Prometheus 3 writes
//! it, and is kept as it is:
//!
//! ```text
//! # TYPE "http.server.duration" histogram
//! {"http.server.duration_bucket", "url.scheme"="https", le="0.1"} 7
//! ```

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt;
use std::time::{Duration, SystemTime};

use super::Metrics;
use super::describe::Description;
use super::ingest::{
    Checked, Gathering, NAME_HOLDS, ONE_TEXT, PAIR_HOLDS, POINT_HOLDS, SAMPLE_BYTES, SERIES_HOLDS, within,
};
use super::instruments::{MOST_HELP, MOST_UNIT};
use super::labels::NAME;
use super::series::{Kind, Labels, Sample, Series};
use crate::clock::from_unix_millis;
use crate::memory::PATIENCE;
use crate::{Error, Reservation, Result, unix_millis};

/// A text an exporter serves, in Prometheus' format or OpenMetrics', and the
/// labels, the time and the keep its ingest gives every series of it. It is
/// kept for each scrape of one target, `ingest` taking each scrape's text.
///
/// ```no_run
/// # fn main() -> tinystore::Result<()> {
/// # let store = tinystore::Store::open("data", Default::default())?;
/// # let body = String::new();
/// let stats = store.metrics()?;
/// let web = stats.prometheus().label("instance", "web-1");
/// web.ingest(&body)?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Debug)]
#[must_use = "a text is written by ingest"]
pub struct Prometheus {
    metrics: Metrics,
    labels: Labels,
    at: Option<SystemTime>,
    keep: Option<Duration>,
}

impl Metrics {
    /// What ingests the text an exporter serves, in Prometheus' format or
    /// OpenMetrics': `stats.prometheus().label("instance", "web-1").ingest(&body)`.
    pub fn prometheus(&self) -> Prometheus {
        Prometheus { metrics: self.clone(), labels: Labels::new(), at: None, keep: None }
    }
}

impl Prometheus {
    /// A label every series of the text gets beside its own, such as the
    /// `instance` scraped; a text that gives it too is `invalid`, naming the
    /// line.
    pub fn label(mut self, name: impl Into<String>, value: impl fmt::Display) -> Prometheus {
        self.labels.insert(name.into(), value.to_string());
        self
    }

    /// The time of every sample the text gives none: the store's clock at
    /// `ingest` unless given.
    pub fn at(mut self, at: SystemTime) -> Prometheus {
        self.at = Some(at);
        self
    }

    /// How long every name of the text keeps its samples, as a series' own
    /// `keep`.
    pub fn keep(mut self, keep: Duration) -> Prometheus {
        self.keep = Some(keep);
        self
    }

    /// Reads the text and writes its series, every one or none, as `ingest`
    /// does: a line it cannot read is `invalid`, naming the line, and a text
    /// past 100,000 samples or 32 MiB is a `limit`.
    pub fn ingest(&self, text: impl AsRef<[u8]>) -> Result<()> {
        let text = text.as_ref();
        let held = self.metrics.engine.memory.reserve(self.holds(text), "metrics: a text", PATIENCE)?;
        self.ingest_held(text, held)
    }

    /// What reading the text and writing its series hold of the store's
    /// memory at most, reserved before a byte of it is read.
    pub(crate) fn holds(&self, text: &[u8]) -> u64 {
        holds(text, &self.labels)
    }

    /// Reads the text and writes its series, `_held` the memory `holds` said.
    pub(crate) fn ingest_held(&self, text: &[u8], held: Reservation) -> Result<()> {
        self.metrics.engine.ingest_checked(self.checked(text)?, held)
    }

    /// The same text with its time fixed now from the store's clock, unless
    /// one was given: an awaited ingest takes it in its first poll, as a
    /// query's `since` does, not where a thread of the pool parses the text.
    #[cfg(feature = "async")]
    pub(crate) fn timed(mut self) -> Prometheus {
        if self.at.is_none() {
            self.at = Some(crate::clock::from_unix_millis(self.metrics.engine.now()));
        }
        self
    }

    /// The series of each sample line, with the labels, the time and the keep
    /// given, checked and gathered as each line is read, so that a line's
    /// series is let go of before the next is read; and what the text
    /// describes of their names.
    fn checked(&self, text: &[u8]) -> Result<Checked> {
        let engine = &self.metrics.engine;
        let at = self.at.map_or_else(|| engine.now(), unix_millis);
        let mut gathering = Gathering::new(ONE_TEXT);
        let described = read(text, &self.labels, at, &mut |mut series| {
            series.keep = self.keep;
            gathering.add(&series)
        })?;
        Ok(engine.checked(gathering).described(described))
    }
}

/// What reading `text` and writing its series hold of the heap at most: each
/// sample line a series, with a pair for each `=` it holds, each label given
/// and its name, and its bytes in two copies; a line that may name a name
/// the line before did not, that name's places among the call's names; and
/// each comment's bytes in five, a family's help being copied to each of its
/// names.
///
/// ```text
/// # HELP up Is it up.    five times its 19 bytes, and a family's place
/// up{shard="7"} 1        a series of three pairs, two copies of its 15 bytes, a name
/// up{shard="8"} 1        a series of three pairs, two copies of its 15 bytes
/// ```
fn holds(text: &[u8], given: &Labels) -> u64 {
    let given_bytes: usize = NAME.len() + given.iter().map(|(label, value)| label.len() + value.len()).sum::<usize>();
    let (mut holds, mut before) = (0, None);
    for line in text.split(|byte| *byte == b'\n').map(<[u8]>::trim_ascii).filter(|line| !line.is_empty()) {
        if line.starts_with(b"#") {
            holds += 5 * line.len() + FAMILY_HOLDS;
            continue;
        }
        let pairs = given.len() + 1 + line.iter().filter(|byte| **byte == b'=').count();
        holds += SERIES_HOLDS + POINT_HOLDS + PAIR_HOLDS * pairs + 2 * line.len() + given_bytes;
        // a quoted name is told from the line before's only by reading it
        let name = line.split(|byte| matches!(byte, b'{' | b' ' | b'\t')).next().filter(|name| !name.is_empty());
        if name.is_none() || name != before {
            holds += NAME_HOLDS + 4 * name.map_or(line.len(), <[u8]>::len);
        }
        before = name;
    }
    holds as u64
}

/// What a family holds of the heap beside its name and its help: its type,
/// its description and its place among the families.
const FAMILY_HOLDS: usize = 128;

/// Blanks between a line's parts.
const BLANK: [char; 2] = [' ', '\t'];
/// The line OpenMetrics ends its text with.
const EOF: &str = "# EOF";
/// The bytes of a line an error quotes.
const QUOTED: usize = 64;

/// The series of each sample line of `text`, the `given` labels beside its
/// own, at `at` unless the line gives its time.
#[cfg(any(test, feature = "fuzzing"))]
pub(super) fn parse(text: &[u8], given: &Labels, at: i64) -> Result<Vec<Series>> {
    let mut series = Vec::new();
    read(text, given, at, &mut |one| {
        series.push(one);
        Ok(())
    })?;
    Ok(series)
}

/// Hands `sink` the series of each sample line of `text` as it is read, and
/// gives what the text's families describe of the names it wrote. A line the
/// sink refuses refuses the text, naming the line.
fn read(text: &[u8], given: &Labels, at: i64, sink: &mut dyn FnMut(Series) -> Result<()>) -> Result<Described> {
    let text = text.strip_prefix(b"\xef\xbb\xbf").unwrap_or(text);
    let mut reader = Reader::new(given, at, is_open_metrics(text), sink);
    for (index, line) in text.split(|byte| *byte == b'\n').enumerate() {
        let number = index + 1;
        let line =
            std::str::from_utf8(line).map_err(|_| Error::invalid(format!("metrics: line {number}: not UTF-8")))?;
        let line = line.trim_ascii();
        reader.line(line).map_err(|error| error.within(format!("metrics: line {number}, `{}`", excerpt(line))))?;
    }
    Ok(reader.described())
}

/// Each name a text wrote that its family describes, with what it gives it.
type Described = Vec<(String, Description)>;

/// Whether the text is OpenMetrics': its last line that is not blank is
/// `# EOF`.
fn is_open_metrics(text: &[u8]) -> bool {
    let last = text.split(|byte| *byte == b'\n').map(<[u8]>::trim_ascii).rfind(|line| !line.is_empty());
    last == Some(EOF.as_bytes())
}

/// What a family's `# TYPE` says it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Type {
    Counter,
    Gauge,
    Histogram,
    GaugeHistogram,
    Summary,
    Info,
    StateSet,
    Unknown,
}

/// Each type by its word; `untyped` is Prometheus' and `unknown`
/// OpenMetrics' for one type.
const TYPES: [(&str, Type); 9] = [
    ("counter", Type::Counter),
    ("gauge", Type::Gauge),
    ("histogram", Type::Histogram),
    ("gaugehistogram", Type::GaugeHistogram),
    ("summary", Type::Summary),
    ("info", Type::Info),
    ("stateset", Type::StateSet),
    ("untyped", Type::Unknown),
    ("unknown", Type::Unknown),
];

/// What a sample's name may add to its family's.
const SUFFIXES: [&str; 8] = ["_total", "_created", "_bucket", "_sum", "_count", "_info", "_gsum", "_gcount"];

/// What a family makes of a sample whose name goes on from the family's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Written(Kind),
    /// A counter's, a histogram's or a summary's `_created`: the time it
    /// started, which no series of the store keeps.
    LeftOut,
    /// A name that is not one of the family's samples.
    Stranger,
}

impl Type {
    /// The role of the family's sample whose name goes on from the family's
    /// by `suffix`:
    ///
    /// ```text
    /// counter         x, x_total                 counter     x_created left out
    /// gauge           x                          gauge
    /// histogram       x_bucket                   histogram   x_sum, x_count counters; x_created left out
    /// gaugehistogram  x_bucket, x_gsum, x_gcount gauge
    /// summary         x, with its quantile       gauge       x_sum, x_count counters; x_created left out
    /// info            x_info                     gauge
    /// stateset        x                          gauge
    /// untyped         x                          gauge
    /// ```
    fn role(self, suffix: &str) -> Role {
        use Type::{Counter, Gauge, GaugeHistogram, Histogram, Info, StateSet, Summary, Unknown};
        match (self, suffix) {
            (Counter | Histogram | Summary, "_created") => Role::LeftOut,
            (Counter, "" | "_total") | (Histogram | Summary, "_sum" | "_count") => Role::Written(Kind::Counter),
            (Histogram, "_bucket") => Role::Written(Kind::Histogram),
            (Gauge | Summary | StateSet | Unknown, "")
            | (Info, "" | "_info")
            | (GaugeHistogram, "_bucket" | "_gsum" | "_gcount") => Role::Written(Kind::Gauge),
            _ => Role::Stranger,
        }
    }

    fn word(self) -> &'static str {
        TYPES.iter().find(|(_, given)| *given == self).map_or("unknown", |(word, _)| word)
    }
}

/// A text as it is read, a line at a time, each sample line's series handed
/// to `sink`.
struct Reader<'a> {
    given: &'a Labels,
    at: i64,
    open_metrics: bool,
    families: HashMap<String, Type>,
    /// Each family's `# HELP` and `# UNIT`, by its name.
    descriptions: HashMap<String, Description>,
    /// The names of the samples read before any `# TYPE` of their family.
    untyped: HashSet<String>,
    sink: &'a mut dyn FnMut(Series) -> Result<()>,
    /// The series read, against one text's samples.
    series: usize,
    /// The names the series were written under.
    names: BTreeSet<String>,
    /// What the series weigh against one text's bytes.
    bytes: usize,
    /// Whether `# EOF` was read, after which no line may follow.
    ended: bool,
}

impl<'a> Reader<'a> {
    fn new(given: &'a Labels, at: i64, open_metrics: bool, sink: &'a mut dyn FnMut(Series) -> Result<()>) -> Self {
        let (families, descriptions, untyped) = (HashMap::new(), HashMap::new(), HashSet::new());
        let (series, names, bytes, ended) = (0, BTreeSet::new(), 0, false);
        Reader { given, at, open_metrics, families, descriptions, untyped, sink, series, names, bytes, ended }
    }

    fn line(&mut self, line: &str) -> Result<()> {
        if line.is_empty() {
            return Ok(());
        }
        if self.ended {
            return Err(Error::invalid("a line after `# EOF`, which ends the text"));
        }
        match line.strip_prefix('#') {
            Some(_) if line == EOF => {
                self.ended = true;
                Ok(())
            }
            Some(comment) => self.comment(comment),
            None => self.sample(line),
        }
    }

    /// A `# TYPE` line declares a family, and `# HELP` and `# UNIT` describe
    /// its names; any other comment says nothing a series keeps.
    fn comment(&mut self, comment: &str) -> Result<()> {
        let (keyword, rest) = comment.trim_start_matches(BLANK).split_once(BLANK).unwrap_or((comment.trim(), ""));
        match keyword {
            "TYPE" => self.type_line(rest),
            "HELP" | "UNIT" => {
                self.describe(keyword, rest);
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn type_line(&mut self, rest: &str) -> Result<()> {
        let line = || Error::invalid("a type's line is `# TYPE`, a metric's name and its type");
        let (name, rest) = metric_name(rest).map_err(|error| if rest.trim().is_empty() { line() } else { error })?;
        let mut words = rest.split(BLANK).filter(|word| !word.is_empty());
        let (Some(word), None) = (words.next(), words.next()) else { return Err(line()) };
        self.declare(&name, word)
    }

    /// Keeps a family's help or unit for the names it writes. A line that
    /// does not read, or a help or a unit past what a name keeps, is left
    /// out rather than refusing the samples of the text.
    fn describe(&mut self, keyword: &str, rest: &str) {
        let Ok((name, text)) = metric_name(rest) else { return };
        let text = text.trim_matches(BLANK);
        let description = self.descriptions.entry(name).or_default();
        if keyword == "HELP" {
            let help = unescaped(text);
            if help.len() <= MOST_HELP {
                description.help = help;
            }
        } else if text.len() <= MOST_UNIT && !text.contains(BLANK) {
            description.unit = text.to_owned();
        }
    }

    /// Each name the text writes that its family describes, with its
    /// family's help, and its unit where the name's values are in it:
    ///
    /// ```text
    /// # UNIT rpc_seconds seconds    rpc_seconds_sum  seconds
    ///                               rpc_seconds_bucket, rpc_seconds_count  none: they count observations
    /// ```
    fn described(&self) -> Described {
        let of_family = |name: &str| {
            let family = self.family_of(name).map_or(name, |(family, _)| family);
            let given = self.descriptions.get(family)?;
            let in_unit = matches!(&name[family.len()..], "" | "_total" | "_sum" | "_gsum");
            let unit = if in_unit { given.unit.clone() } else { String::new() };
            let described = Description { unit, help: given.help.clone() };
            (described != Description::default()).then(|| (name.to_owned(), described))
        };
        self.names.iter().map(String::as_str).filter_map(of_family).collect()
    }

    fn declare(&mut self, name: &str, word: &str) -> Result<()> {
        let family = TYPES.iter().find(|(given, _)| *given == word).map(|(_, family)| *family).ok_or_else(|| {
            Error::invalid(format!(
                "a type {word:?}: counter, gauge, histogram, summary or untyped, \
                 or OpenMetrics' gaugehistogram, info, stateset or unknown"
            ))
        })?;
        if self.families.contains_key(name) {
            return Err(Error::invalid(format!("a second `# TYPE` of {name}")));
        }
        let written_before =
            |suffix: &&&str| family.role(suffix) != Role::Stranger && self.untyped.contains(&format!("{name}{suffix}"));
        if let Some(suffix) = [""].iter().chain(&SUFFIXES).find(written_before) {
            return Err(Error::invalid(format!("the `# TYPE` of {name} after its sample {name}{suffix}")));
        }
        self.families.insert(name.to_owned(), family);
        Ok(())
    }

    fn sample(&mut self, line: &str) -> Result<()> {
        let (classic, rest) = name_at(line, true);
        let (quoted, labels, rest) = labels_of(rest)?;
        let name = match (classic, quoted) {
            ("", Some(quoted)) => quoted,
            ("", None) => {
                return Err(Error::invalid(format!("a sample's line begins with its metric's name: {NAMED}")));
            }
            (classic, None) => classic.to_owned(),
            (classic, Some(quoted)) => {
                return Err(Error::invalid(format!("a sample of two names, {classic} and {quoted:?}")));
            }
        };
        let name = name.as_str();
        let (value, at) = self.value_and_time(rest)?;
        let Some(kind) = self.kind_of(name)? else { return Ok(()) };
        let labels = self.with_given(labels)?;
        self.bytes += NAME.len() + name.len() + SAMPLE_BYTES;
        self.bytes += labels.iter().map(|(label, value)| label.len() + value.len()).sum::<usize>();
        within(ONE_TEXT, "a text", self.series + 1, self.bytes)?;
        self.series += 1;
        if !self.names.contains(name) {
            self.names.insert(name.to_owned());
        }
        let samples = vec![Sample { at: from_unix_millis(at), value }];
        (self.sink)(Series { name: name.to_owned(), kind, labels, samples, keep: None })
    }

    /// The kind of the series a sample called `name` writes, of the family
    /// its name belongs to, a gauge when it belongs to none; none for a
    /// sample left out.
    fn kind_of(&mut self, name: &str) -> Result<Option<Kind>> {
        match (self.family_of(name).map(|(_, role)| role), self.families.get(name)) {
            (Some(Role::Written(kind)), _) => Ok(Some(kind)),
            (Some(_), _) => Ok(None),
            // only a histogram's family has no sample of its own name
            (None, Some(&family)) => {
                let (sum, count) = if family == Type::Histogram { ("_sum", "_count") } else { ("_gsum", "_gcount") };
                let word = family.word();
                Err(Error::invalid(format!(
                    "{name} is a {word}, whose samples are {name}_bucket, {name}{sum} and {name}{count}"
                )))
            }
            (None, None) => {
                if !self.untyped.contains(name) {
                    self.untyped.insert(name.to_owned());
                }
                Ok(Some(Kind::Gauge))
            }
        }
    }

    /// The family a sample called `name` belongs to, and what the family
    /// makes of it, when it belongs to one.
    fn family_of<'n>(&self, name: &'n str) -> Option<(&'n str, Role)> {
        [""].iter().chain(&SUFFIXES).find_map(|suffix| {
            let family = name.strip_suffix(suffix)?;
            let role = self.families.get(family)?.role(suffix);
            (role != Role::Stranger).then_some((family, role))
        })
    }

    /// A sample's labels with the call's beside them; one given by both is
    /// refused rather than chosen between.
    fn with_given(&self, mut labels: Labels) -> Result<Labels> {
        for (label, value) in self.given {
            if labels.contains_key(label) {
                return Err(Error::invalid(format!("label {label}, which the call gives every series")));
            }
            labels.insert(label.clone(), value.clone());
        }
        Ok(labels)
    }

    /// A sample's value and its time, the call's when the line gives none;
    /// an exemplar after them, OpenMetrics' `# {…} 0.5`, is left out.
    fn value_and_time(&self, rest: &str) -> Result<(f64, i64)> {
        if !rest.is_empty() && !rest.starts_with(BLANK) {
            return Err(Error::invalid(format!("a space between the name and the value, where {NAMED}")));
        }
        let mut words = rest.split(BLANK).filter(|word| !word.is_empty()).take_while(|word| !word.starts_with('#'));
        let value = value_of(words.next().ok_or_else(|| Error::invalid("a sample without its value"))?)?;
        let at = words.next().map_or(Ok(self.at), |word| self.time_of(word))?;
        if let Some(word) = words.next() {
            return Err(Error::invalid(format!("{word:?} after the sample's time")));
        }
        Ok((value, at))
    }

    /// A sample's time: milliseconds in Prometheus' text, seconds in
    /// OpenMetrics', rounded to the millisecond.
    fn time_of(&self, word: &str) -> Result<i64> {
        if !self.open_metrics {
            return word.parse().map_err(|_| {
                Error::invalid(format!(
                    "a time {word:?}: milliseconds, an integer; OpenMetrics' seconds come in a text that ends `# EOF`"
                ))
            });
        }
        let millis = word.parse::<f64>().map(|seconds| (seconds * 1000.0).round());
        // the cast saturates, so a time past what an i64 holds is refused before it
        millis
            .ok()
            .filter(|millis| millis.abs() < 9.2e18)
            .map(|millis| millis as i64)
            .ok_or_else(|| Error::invalid(format!("a time {word:?}: OpenMetrics' seconds since 1970, a number")))
    }
}

/// What a name holds.
const NAMED: &str = "a name holds letters, digits, `_` and a metric's `:`, and begins with no digit, or is quoted";

/// The metric's name at the start of `text`, past blanks, quoted or not,
/// and what follows it.
fn metric_name(text: &str) -> Result<(String, &str)> {
    let text = text.trim_start_matches(BLANK);
    if text.starts_with('"') {
        let (name, rest) = quoted(text, "a metric's name")?;
        if name.is_empty() {
            return Err(Error::invalid("a metric's name of no character"));
        }
        return Ok((name, rest));
    }
    let (name, rest) = name_at(text, true);
    if name.is_empty() || !(rest.is_empty() || rest.starts_with(BLANK)) {
        let word = text.split(BLANK).next().unwrap_or(text);
        return Err(Error::invalid(format!("a metric's name {word:?}: {NAMED}")));
    }
    Ok((name.to_owned(), rest))
}

/// The name at the start of `text` and what follows it.
fn name_at(text: &str, metric: bool) -> (&str, &str) {
    let named = |(at, byte): &(usize, u8)| {
        byte.is_ascii_alphabetic() || *byte == b'_' || (metric && *byte == b':') || (*at > 0 && byte.is_ascii_digit())
    };
    let end = text.bytes().enumerate().find(|pair| !named(pair)).map_or(text.len(), |(at, _)| at);
    text.split_at(end)
}

/// The labels between braces after a sample's name, when it has them, and
/// what follows them; a name past the classic characters comes first
/// between them, quoted and with no `=`. A label's own name may be quoted
/// too. A label whose value is empty is no label, as Prometheus reads it.
fn labels_of(rest: &str) -> Result<(Option<String>, Labels, &str)> {
    let Some(mut rest) = rest.trim_start_matches(BLANK).strip_prefix('{') else {
        return Ok((None, Labels::new(), rest));
    };
    let (mut name, mut labels, mut empty) = (None, Labels::new(), Vec::new());
    let mut first = true;
    loop {
        rest = rest.trim_start_matches(BLANK);
        if let Some(after) = rest.strip_prefix('}') {
            return Ok((name, labels, after));
        }
        let (label, after) = label_name(rest)?;
        let after = after.trim_start_matches(BLANK);
        let Some(after) = after.strip_prefix('=') else {
            if !(first && rest.starts_with('"')) {
                return Err(Error::invalid(format!("label {label} without `=` and its value")));
            }
            name = Some(label);
            (rest, first) = (next_in_braces(after, "the metric's quoted name")?, false);
            continue;
        };
        first = false;
        let (value, after) = quoted(after.trim_start_matches(BLANK), &format!("label {label}"))?;
        if labels.contains_key(&label) || empty.contains(&label) {
            return Err(Error::invalid(format!("label {label} twice")));
        }
        let what = format!("label {label}'s value");
        if value.is_empty() {
            empty.push(label);
        } else {
            labels.insert(label, value);
        }
        rest = next_in_braces(after, &what)?;
    }
}

/// A label's name at the start of `rest`, quoted or not, and what follows it.
fn label_name(rest: &str) -> Result<(String, &str)> {
    if rest.starts_with('"') {
        let (name, after) = quoted(rest, "a quoted name")?;
        if name.is_empty() {
            return Err(Error::invalid("a quoted name of no character"));
        }
        return Ok((name, after));
    }
    let (label, after) = name_at(rest, false);
    if label.is_empty() {
        return Err(Error::invalid(format!("a label's name, where {NAMED}")));
    }
    Ok((label.to_owned(), after))
}

/// What follows `what` between braces: past its `,`, or at the `}` that
/// ends them.
fn next_in_braces<'a>(rest: &'a str, what: &str) -> Result<&'a str> {
    let rest = rest.trim_start_matches(BLANK);
    match rest.strip_prefix(',') {
        Some(after) => Ok(after),
        None if rest.starts_with('}') => Ok(rest),
        None => Err(Error::invalid(format!("{what} followed by neither `,` nor `}}`"))),
    }
}

/// A text between double quotes, a label's value or a quoted name, `\\`, `\"`
/// and `\n` read as what they stand for, and what follows it. Any other
/// backslash is kept as it is, as Prometheus keeps it, rather than refusing
/// the scrape it is in.
fn quoted<'a>(rest: &'a str, what: &str) -> Result<(String, &'a str)> {
    let refused = |problem| Error::invalid(format!("{what}: {problem}"));
    let mut rest = rest.strip_prefix('"').ok_or_else(|| refused("a value between double quotes"))?;
    let mut value = String::new();
    while let Some(at) = rest.find(['"', '\\']) {
        value.push_str(&rest[..at]);
        if rest.as_bytes()[at] == b'"' {
            return Ok((value, &rest[at + 1..]));
        }
        let mut escaped = rest[at + 1..].chars();
        match escaped.next() {
            Some('n') => value.push('\n'),
            Some(character @ ('\\' | '"')) => value.push(character),
            Some(other) => value.extend(['\\', other]),
            None => break,
        }
        rest = escaped.as_str();
    }
    Err(refused("a value without its closing quote"))
}

/// A help as `# HELP` writes it, `\\`, `\n` and OpenMetrics' `\"` read as
/// what they stand for; any other backslash is kept.
fn unescaped(text: &str) -> String {
    let mut help = String::with_capacity(text.len());
    let mut characters = text.chars();
    while let Some(character) = characters.next() {
        match (character, characters.clone().next()) {
            ('\\', Some(escaped @ ('\\' | '"'))) => help.push(escaped),
            ('\\', Some('n')) => help.push('\n'),
            _ => {
                help.push(character);
                continue;
            }
        }
        characters.next();
    }
    help
}

/// A sample's value, kept as the text spells it: `-0`, `NaN` and the
/// infinities among them. A number past the largest float is refused, not
/// made an infinity the text did not say.
fn value_of(word: &str) -> Result<f64> {
    let refused = |what: &str| Error::invalid(format!("a value {word:?}: {what}"));
    let value: f64 = word.parse().map_err(|_| refused("a number, +Inf, -Inf or NaN"))?;
    let infinity =
        word.trim_start_matches(['+', '-']).as_bytes().first().is_some_and(|byte| byte.eq_ignore_ascii_case(&b'i'));
    if value.is_infinite() && !infinity {
        return Err(refused("past the largest float"));
    }
    Ok(value)
}

/// The start of a line, for an error to quote.
fn excerpt(line: &str) -> String {
    if line.len() <= QUOTED {
        return line.to_owned();
    }
    let end = (0..=QUOTED).rev().find(|end| line.is_char_boundary(*end)).unwrap_or(0);
    format!("{}…", &line[..end])
}

#[cfg(test)]
#[path = "prometheus_tests.rs"]
mod tests;
