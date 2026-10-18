//! A logger as a `tracing` layer: each event is printed as it is logged and,
//! when a store's logger keeps it, handed to the store. An event's fields are
//! its fields, its spans' fields its context, outermost first, and a span's
//! `trace_id` and `span_id` its trace.
//!
//! ```text
//! tracing::info!(user = 42, "signed in")   →  11:02:11.123 INFO  api  signed in  user=42
//! ```
//!
//! A logger's level is its own: a line under it is neither printed nor kept,
//! and the subscriber's other layers still see it, as their own filters say.

use std::fmt;
use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::SystemTime;

use jiff::tz::TimeZone;
use serde::Serialize;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::{LookupSpan, Registry, Scope};

use super::Format;
#[cfg(all(feature = "records", feature = "config"))]
use super::env::Said;
use super::env::{Environment, colors, first_said, forced};
use super::line::{LOG, Line, Look, SOURCE, Time, json, object_pairs, pretty, quoted};
use super::redact::Redactor;
use crate::unix_nanos;

/// The target and the stream of the store's own lines, which a logger prints
/// from info up and never keeps, so that a line that fails to be kept is not
/// kept again.
const OWN: &str = "tinystore";
/// slog's number of info.
const INFO: i32 = 0;
/// slog's number of trace, below which `tracing` has no level.
const TRACE: i32 = -8;

/// What changes each field before it is hidden and kept: given its key and
/// its JSON, the value to keep, or none to drop the field.
pub type Replace = Arc<dyn Fn(&str, &str) -> Option<String> + Send + Sync>;

/// Where a store's logger keeps the lines its layer hands it.
pub(crate) trait Keep: Send + Sync {
    /// Holds a line for the store, never waiting: a line it has no room for
    /// is dropped and counted.
    fn keep(&self, line: Line);
    /// The store's time, in nanoseconds since 1970.
    fn now(&self) -> i64;
    /// Writes what it holds, and returns once it is written.
    fn flush(&self);
    /// The lines dropped since the logger began.
    fn dropped(&self) -> u64;
}

/// What a logger is told: the least level, how and where its lines print,
/// what it hides, and which variables of the environment it reads.
pub(crate) struct Told {
    pub(crate) level: Option<Level>,
    pub(crate) format: Option<Format>,
    pub(crate) time: Time,
    pub(crate) hide_stream: bool,
    pub(crate) to: Option<Box<dyn Write + Send>>,
    pub(crate) redact: Vec<String>,
    pub(crate) keep_url_passwords: bool,
    pub(crate) replace: Option<Replace>,
    pub(crate) source: bool,
    pub(crate) env: Option<Environment>,
}

impl Default for Told {
    fn default() -> Told {
        Told {
            level: None,
            format: None,
            time: Time::Clock,
            hide_stream: false,
            to: None,
            redact: Vec::new(),
            keep_url_passwords: false,
            replace: None,
            source: false,
            env: Some(Environment::default()),
        }
    }
}

/// Where lines print, and how.
struct Printer {
    out: Mutex<Box<dyn Write + Send>>,
    pretty: bool,
    look: Look,
}

impl Printer {
    /// Prints a line whole, so that the lines of many threads never
    /// interleave; a console that refuses it loses that line, never what the
    /// store keeps.
    fn print(&self, line: &Line) {
        let text = if self.pretty { pretty(line, &self.look) } else { json(line) };
        let mut out = self.out.lock().unwrap_or_else(PoisonError::into_inner);
        let _ = out.write_all(text.as_bytes());
    }

    fn flush(&self) {
        let _ = self.out.lock().unwrap_or_else(PoisonError::into_inner).flush();
    }
}

/// What a logger does with a line before it is kept: whether its level is
/// wanted, its fields changed and hidden, and where and how it prints.
pub(crate) struct Console {
    stream: String,
    /// slog's number of the least level printed and kept, which the config
    /// `log` changes while the logger runs; trace takes every level.
    least: AtomicI32,
    printer: Option<Printer>,
    redactor: Redactor,
    replace: Option<Replace>,
    source: bool,
    /// Events refused before they could be kept: no name, or fields that
    /// are no object.
    refused: AtomicU64,
    #[cfg(all(feature = "records", feature = "config"))]
    declared: Declared,
}

/// What a store's logger declares its config `log` with: the code's options
/// as its defaults, trace for a logger told no level, and the variables of
/// the environment it read, which go over them.
#[cfg(all(feature = "records", feature = "config"))]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Declared {
    pub(crate) level: &'static str,
    /// The code's format, or the one its terminal chose.
    pub(crate) format: &'static str,
    pub(crate) time: &'static str,
    /// The variables' prefix and those that said what a logger can mean;
    /// none for a logger that reads no variable.
    pub(crate) env: Option<(String, Vec<(String, String)>)>,
}

impl Console {
    /// A logger's console, as the environment and then the code tell it; a
    /// variable that says what none can mean is said once.
    pub(crate) fn new(stream: &str, told: Told) -> Console {
        let said = told.env.as_ref().map(Environment::said).unwrap_or_default();
        let least = said.level.or(told.level.map(number));
        // the environment never turns on a console the code turned off
        let format = if told.format == Some(Format::Off) { Some(Format::Off) } else { said.format.or(told.format) };
        // colours are the terminal's, whatever variables of its own a logger reads
        let lookup: fn(&str) -> Option<String> = told.env.as_ref().map_or(real, |env| env.lookup);
        let terminal = told.to.is_none() && std::io::stderr().is_terminal();
        // whether a console told no format prints pretty lines
        let chosen = terminal || forced(lookup);
        #[cfg(all(feature = "records", feature = "config"))]
        let declared = Declared::of(&told, &said, chosen);
        let printer = (format != Some(Format::Off)).then(|| {
            let pretty = format == Some(Format::Pretty) || format.is_none() && chosen;
            let look = Look {
                color: pretty && colors(terminal, lookup),
                zone: TimeZone::system(),
                time: said.time.unwrap_or(told.time),
                hide_stream: told.hide_stream,
            };
            let out = told.to.unwrap_or_else(|| Box::new(std::io::stderr()));
            Printer { out: Mutex::new(out), pretty, look }
        });
        let console = Console {
            stream: stream.to_owned(),
            least: AtomicI32::new(least.unwrap_or(TRACE)),
            printer,
            redactor: Redactor::new(&told.redact, told.keep_url_passwords),
            replace: told.replace,
            source: told.source,
            refused: AtomicU64::new(0),
            #[cfg(all(feature = "records", feature = "config"))]
            declared,
        };
        for (name, value, expected) in said.ignored {
            if first_said(&name, &value) {
                console.say(&Line {
                    level: Some(4),
                    message: Some(format!("{name} is ignored")),
                    fields: vec![("value".to_owned(), quoted(&value)), ("expected".to_owned(), quoted(expected))],
                    ..own_line(unix_nanos(SystemTime::now()))
                });
            }
        }
        console
    }

    /// Prints a line, whether it is kept or not.
    pub(crate) fn say(&self, line: &Line) {
        if let Some(printer) = &self.printer {
            printer.print(line);
        }
    }

    /// slog's number of the least level wanted; none when every one is.
    fn least(&self) -> Option<i32> {
        let least = self.least.load(Ordering::Relaxed);
        (least > TRACE).then_some(least)
    }

    fn wants(&self, level: i32) -> bool {
        self.least().is_none_or(|least| level >= least)
    }

    /// Takes the level the config `log` gives, a name of `LEVELS`, for every
    /// line logged after.
    #[cfg(all(feature = "records", feature = "config"))]
    pub(crate) fn set_level(&self, name: &str) {
        if let Some(least) = super::env::level(name) {
            self.least.store(least, Ordering::Relaxed);
        }
    }

    #[cfg(all(feature = "records", feature = "config"))]
    pub(crate) fn declared(&self) -> &Declared {
        &self.declared
    }

    /// Fields as they are kept: changed or dropped by `replace`, then hidden.
    fn fields(&self, mut fields: Vec<(String, String)>) -> Vec<(String, String)> {
        if let Some(replace) = &self.replace {
            let replaced = |(key, json): (String, String)| {
                let value = replace(&key, &json)?;
                // what is no JSON is kept as text
                let json =
                    if serde_json::from_str::<serde::de::IgnoredAny>(&value).is_ok() { value } else { quoted(&value) };
                Some((key, json))
            };
            fields = fields.into_iter().filter_map(replaced).collect();
        }
        self.redactor.fields(&mut fields);
        fields
    }
}

#[cfg(all(feature = "records", feature = "config"))]
impl Declared {
    /// What the code and the environment told a logger, `pretty` whether a
    /// console told no format prints pretty lines.
    fn of(told: &Told, said: &Said, pretty: bool) -> Declared {
        use super::env::{FORMATS, LEVELS, TIMES, spelled};
        let format = told.format.unwrap_or(if pretty { Format::Pretty } else { Format::Json });
        let console_off = told.format == Some(Format::Off);
        Declared {
            level: told.level.and_then(|level| spelled(&LEVELS, number(level))).unwrap_or("trace"),
            format: spelled(&FORMATS, format).unwrap_or("json"),
            time: spelled(&TIMES, told.time).unwrap_or("clock"),
            env: told.env.as_ref().map(|env| env.as_config(said, console_off)),
        }
    }
}

/// A line of the store's own at `at`, which a logger prints and never keeps.
pub(crate) fn own_line(at: i64) -> Line {
    Line { at, stream: OWN.to_owned(), name: LOG.to_owned(), ..Line::default() }
}

pub(super) fn real(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// A logger's layer: what `tracing` hands it is printed, and kept by a
/// store's logger. A clone is the same logger, for `flush` and `dropped` once
/// the subscriber holds the layer.
#[derive(Clone)]
pub struct Layer {
    console: Arc<Console>,
    keep: Option<Arc<dyn Keep>>,
    /// The config `log` a store's logger follows, whose watch sets its
    /// level, held while any clone of the logger is.
    #[cfg(all(feature = "records", feature = "config"))]
    #[expect(dead_code, reason = "held for its drop, which ends the watch")]
    following: Option<Arc<dyn std::any::Any + Send + Sync>>,
}

impl fmt::Debug for Layer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Layer").field("stream", &self.console.stream).field("least", &self.console.least()).finish()
    }
}

impl Layer {
    pub(crate) fn new(console: Arc<Console>, keep: Option<Arc<dyn Keep>>) -> Layer {
        Layer {
            console,
            keep,
            #[cfg(all(feature = "records", feature = "config"))]
            following: None,
        }
    }

    /// The layer holding what keeps its console's level the config's.
    #[cfg(all(feature = "records", feature = "config"))]
    pub(crate) fn following(self, following: Arc<dyn std::any::Any + Send + Sync>) -> Layer {
        Layer { following: Some(following), ..self }
    }

    /// Writes what the logger holds and returns once it is written, as the
    /// store does every second and when it closes; then flushes the console.
    pub fn flush(&self) {
        if let Some(keep) = &self.keep {
            keep.flush();
        }
        if let Some(printer) = &self.console.printer {
            printer.flush();
        }
    }

    /// The lines dropped since the logger began: a full buffer, a line the
    /// file refused, an event without a name or whose fields are no object.
    pub fn dropped(&self) -> u64 {
        self.console.refused.load(Ordering::Relaxed) + self.keep.as_ref().map_or(0, |keep| keep.dropped())
    }

    /// Writes an event named `name` with `fields`, a struct or a map, and
    /// neither a level nor a message, as a line is written: at once, and
    /// dropped under load. Its context is the current span's.
    pub fn event(&self, name: &str, fields: &impl Serialize) {
        let pairs = serde_json::to_string(fields).ok().and_then(|object| object_pairs(&object).ok());
        let Some(pairs) = pairs.filter(|_| !name.is_empty()) else {
            self.console.refused.fetch_add(1, Ordering::Relaxed);
            return;
        };
        let mut line = Line {
            at: self.now(),
            stream: self.console.stream.clone(),
            name: name.to_owned(),
            fields: pairs,
            ..Line::default()
        };
        tracing::Span::current().with_subscriber(|(id, dispatch)| {
            if let Some(span) = dispatch.downcast_ref::<Registry>().and_then(|registry| registry.span(id)) {
                self.add_scope(&mut line, span.scope());
            }
        });
        self.write(line, true);
    }

    fn now(&self) -> i64 {
        self.keep.as_ref().map_or_else(|| unix_nanos(SystemTime::now()), |keep| keep.now())
    }

    /// The context and the trace of the spans a line was logged in, from the
    /// outermost: a span under the logger's level gives none.
    fn add_scope<'a, R: LookupSpan<'a>>(&self, line: &mut Line, scope: Scope<'a, R>) {
        for span in scope.from_root() {
            if !self.console.wants(number(*span.metadata().level())) {
                continue;
            }
            if let Some(spanned) = span.extensions().get::<Spanned>() {
                line.context.extend(spanned.fields.iter().cloned());
                line.trace = spanned.trace.or(line.trace);
                line.span = spanned.span.or(line.span);
            }
        }
    }

    /// Prints a line with its fields changed and hidden, and keeps it unless
    /// it is the store's own.
    fn write(&self, mut line: Line, kept: bool) {
        line.context = self.console.fields(std::mem::take(&mut line.context));
        line.fields = self.console.fields(std::mem::take(&mut line.fields));
        self.console.say(&line);
        if kept && let Some(keep) = &self.keep {
            keep.keep(line);
        }
    }

    /// Which of the subscriber's layers of this kind records a span's fields:
    /// the first to see it, so that two loggers never record a field twice.
    fn recorder(&self) -> usize {
        Arc::as_ptr(&self.console).addr()
    }
}

/// slog's number of a level: trace below debug, as Python's levels map.
fn number(level: Level) -> i32 {
    match level {
        Level::TRACE => -8,
        Level::DEBUG => -4,
        Level::INFO => 0,
        Level::WARN => 4,
        Level::ERROR => 8,
    }
}

/// A span's fields as a logger keeps them, and the trace it carries.
#[derive(Default)]
struct Spanned {
    recorder: usize,
    fields: Vec<(String, String)>,
    trace: Option<[u8; 16]>,
    span: Option<[u8; 8]>,
}

impl<S> tracing_subscriber::Layer<S> for Layer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attributes: &Attributes<'_>, id: &Id, context: Context<'_, S>) {
        let Some(span) = context.span(id) else { return };
        let mut extensions = span.extensions_mut();
        if extensions.get_mut::<Spanned>().is_some() {
            return;
        }
        let mut spanned = Spanned { recorder: self.recorder(), ..Spanned::default() };
        attributes.record(&mut Fields { into: &mut spanned, message: None });
        extensions.insert(spanned);
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, context: Context<'_, S>) {
        let Some(span) = context.span(id) else { return };
        let mut extensions = span.extensions_mut();
        if let Some(spanned) = extensions.get_mut::<Spanned>()
            && spanned.recorder == self.recorder()
        {
            values.record(&mut Fields { into: spanned, message: None });
        }
    }

    fn on_event(&self, event: &Event<'_>, context: Context<'_, S>) {
        let metadata = event.metadata();
        let (level, own) = (number(*metadata.level()), metadata.target() == OWN);
        // the store's own lines show from info up, or from the logger's level when it has one, as Go's
        let shown = if own && self.console.least().is_none() { level >= INFO } else { self.console.wants(level) };
        if !shown {
            return;
        }
        let (mut called, mut message) = (Spanned::default(), None);
        event.record(&mut Fields { into: &mut called, message: Some(&mut message) });
        let mut line = Line {
            at: self.now(),
            stream: if own { OWN.to_owned() } else { self.console.stream.clone() },
            name: LOG.to_owned(),
            level: Some(level),
            message: Some(message.unwrap_or_default()),
            ..Line::default()
        };
        if let Some(scope) = context.event_scope(event) {
            self.add_scope(&mut line, scope);
        }
        line.trace = called.trace.or(line.trace);
        line.span = called.span.or(line.span);
        if self.console.source
            && let (Some(file), Some(number)) = (metadata.file(), metadata.line())
        {
            let function = metadata.module_path().unwrap_or_default();
            let spelled = serde_json::json!({ "function": function, "file": file, "line": number }).to_string();
            called.fields.push((SOURCE.to_owned(), spelled));
        }
        line.fields = called.fields;
        self.write(line, !own);
    }
}

/// What a `tracing` event or span gives: each field's JSON as Go's and
/// Bun's loggers spell it, and an event's message apart.
struct Fields<'a> {
    into: &'a mut Spanned,
    message: Option<&'a mut Option<String>>,
}

impl Fields<'_> {
    fn add(&mut self, field: &Field, json: String) {
        self.into.fields.push((field.name().to_owned(), json));
    }

    fn text(&mut self, field: &Field, text: &str) {
        let name = field.name();
        if name == "message"
            && let Some(message) = self.message.as_mut()
        {
            **message = Some(text.to_owned());
        } else if name == "trace_id"
            && let Some(trace) = from_hex(text)
        {
            self.into.trace = Some(trace);
        } else if name == "span_id"
            && let Some(span) = from_hex(text)
        {
            self.into.span = Some(span);
        } else {
            self.add(field, quoted(text));
        }
    }
}

impl Visit for Fields<'_> {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.text(field, value);
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.text(field, &format!("{value:?}"));
    }

    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        self.text(field, &value.to_string());
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.add(field, value.to_string());
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.add(field, value.to_string());
    }

    fn record_i128(&mut self, field: &Field, value: i128) {
        self.add(field, value.to_string());
    }

    fn record_u128(&mut self, field: &Field, value: u128) {
        self.add(field, value.to_string());
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.add(field, value.to_string());
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        self.add(field, float(value));
    }
}

/// A float as Go's loggers write one, strconv's shortest `'g'`: the fewest
/// digits that read back as it, in an exponent's form below 1e-4 and from
/// 1e+06; NaN and the infinities, which JSON has no number for, as their
/// names' strings.
///
/// ```text
/// 0.5 → 0.5    1234567 → 1.234567e+06    0.00001 → 1e-05    NaN → "NaN"
/// ```
pub(crate) fn float(value: f64) -> String {
    if value.is_nan() {
        return quoted("NaN");
    }
    if value.is_infinite() {
        return quoted(if value > 0.0 { "+Inf" } else { "-Inf" });
    }
    let scientific = format!("{value:e}");
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    if value != 0.0 && !(-4..6).contains(&exponent) {
        let sign = if exponent < 0 { '-' } else { '+' };
        return format!("{mantissa}e{sign}{:02}", exponent.abs());
    }
    format!("{value}")
}

/// Hex of `N` bytes, the case ignored; all zeros is none, as OpenTelemetry
/// reads an id.
fn from_hex<const N: usize>(text: &str) -> Option<[u8; N]> {
    // `from_str_radix` takes a leading `+`, which is no hex digit of an id.
    if text.len() != 2 * N || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let bytes: Option<Vec<u8>> = (0..N).map(|at| u8::from_str_radix(text.get(2 * at..2 * at + 2)?, 16).ok()).collect();
    bytes?.try_into().ok().filter(|bytes: &[u8; N]| bytes.iter().any(|byte| *byte != 0))
}

#[cfg(test)]
#[path = "layer_tests.rs"]
mod tests;
