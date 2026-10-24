//! How the server's log lines read: short and quietly coloured in a person's
//! terminal, one JSON object a line in a file or a pipe, one line an event.
//!
//! ```text
//! terminal  11:02:11  linked     us.internal:7444
//!           11:02:14  client     bun 1.4 · 1 connected
//! file      {"time":"2026-10-18T11:02:11.123Z","level":"INFO","stream":"tinystore",
//!            "msg":"a node is linked to the node it copies","from":"us.internal:7444"}     one line
//! ```
//!
//! A person's terminal names a known event by a word, `LABELS`, and shows
//! the fields that matter; any other line is its level, its message and its
//! fields, as the file has them.

use std::fmt::{self, Write as _};
use std::io::{self, IsTerminal};

use jiff::{Timestamp, Zoned};
use nu_ansi_term::{Color, Style};
use tracing::field::{Field, Visit};
use tracing::level_filters::LevelFilter;
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields, FormattedFields};
use tracing_subscriber::registry::LookupSpan;

/// Whether a terminal's lines are coloured, by the conventions every tool
/// keeps, and whether there is a terminal at all.
///
/// | `NO_COLOR` | `FORCE_COLOR` or `CLICOLOR_FORCE` | `TERM=dumb` | a terminal | colours |
/// |------------|-----------------------------------|-------------|------------|---------|
/// | set        | any                               | any         | any        | no      |
/// | unset      | set                               | any         | any        | yes     |
/// | unset      | unset                             | yes         | any        | no      |
/// | unset      | unset                             | no          | yes        | yes     |
#[derive(Clone, Copy, Debug)]
pub(crate) struct Terminal {
    no_color: bool,
    force_color: bool,
    dumb: bool,
    /// Whether the stream is a terminal a person reads.
    pub(crate) terminal: bool,
}

impl Terminal {
    pub(crate) fn of_stderr() -> Terminal {
        Self::of(io::stderr().is_terminal())
    }

    pub(crate) fn of_stdout() -> Terminal {
        Self::of(io::stdout().is_terminal())
    }

    fn of(terminal: bool) -> Terminal {
        let set = |name: &str| std::env::var_os(name).is_some_and(|value| !value.is_empty() && value != "0");
        Terminal {
            no_color: set("NO_COLOR"),
            force_color: set("FORCE_COLOR") || set("CLICOLOR_FORCE"),
            dumb: std::env::var_os("TERM").is_some_and(|term| term == "dumb"),
            terminal,
        }
    }

    pub(crate) fn colours(self) -> bool {
        !self.no_color && (self.force_color || (!self.dumb && self.terminal)) && enable_ansi()
    }
}

/// A Windows console shows ANSI colours once a program asks it to; elsewhere
/// a terminal does.
fn enable_ansi() -> bool {
    #[cfg(windows)]
    {
        nu_ansi_term::enable_ansi_support().is_ok()
    }
    #[cfg(not(windows))]
    {
        true
    }
}

/// The least an event must be to be logged: `LOG_LEVEL`, info when unset.
pub(crate) fn level() -> LevelFilter {
    level_named(std::env::var("LOG_LEVEL").ok().as_deref())
}

/// A level by its name, info for none or one unknown: tracing's own parse
/// reads an empty name as error, which would hide every line but errors.
fn level_named(name: Option<&str>) -> LevelFilter {
    match name.map(str::trim) {
        Some(name) if !name.is_empty() => name.parse().unwrap_or(LevelFilter::INFO),
        _ => LevelFilter::INFO,
    }
}

/// The least an event must be to reach stderr, where `logged` is the least
/// logged at all: `LOG_CONSOLE_LEVEL`, else warnings for `--quiet`, else
/// `logged`.
pub(crate) fn stderr_level(quiet: bool, logged: LevelFilter) -> LevelFilter {
    stderr_level_named(std::env::var("LOG_CONSOLE_LEVEL").ok().as_deref(), quiet, logged)
}

/// The level on stderr by its name, none or one unknown leaving it to
/// `--quiet` and `logged`.
fn stderr_level_named(name: Option<&str>, quiet: bool, logged: LevelFilter) -> LevelFilter {
    let named = name.map(str::trim).filter(|name| !name.is_empty()).and_then(|name| name.parse().ok());
    match named {
        Some(level) => level,
        None if quiet => logged.min(LevelFilter::WARN),
        None => logged,
    }
}

/// The events a person's terminal names by a word: an event's message, its
/// word, and what follows it, each `{field}` its value and a part between
/// ` · ` left out when a field it names is missing. A word of `""` is the
/// event's level.
pub(crate) const LABELS: [(&str, &str, &str); 14] = [
    ("a client connected", "client", "{client} · {clients} connected"),
    ("a client left", "client", "left · {client} · {clients} connected"),
    ("a node serves its files", "node", "listening on {address}"),
    ("a node is linked to the node it copies", "linked", "{from}"),
    ("a node holds a whole copy of a file", "copied", "{file} · caught up"),
    ("a node's link to the node it copies", "", "{from} · {error}"),
    ("a copy starts again from a snapshot", "", "{file} starts again from a snapshot · {error}"),
    ("a file's home moved to this node", "home", "{file} · epoch {epoch}"),
    ("a node handed a file's home over, and copies it", "handed", "{file} to {to}"),
    ("a copy took a file over, its lease granted", "took over", "{file} · epoch {epoch}"),
    ("a witness granted a file's lease", "lease", "{file} to {holder} · epoch {epoch}"),
    ("a node's link to its witness", "", "witness {witness} · {error}"),
    ("stopping", "stopping", "{why} · {clients} clients"),
    ("stopped", "stopped", "in {took}"),
];

/// A log line's format: a person's terminal's, or a file's JSON.
pub(crate) struct Lines {
    pub(crate) colours: bool,
    /// Local time to the second and the words of `LABELS`, as a person reads
    /// them; otherwise one JSON object, its time UTC to the millisecond.
    pub(crate) person: bool,
}

impl<S, N> FormatEvent<S, N> for Lines
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(&self, context: &FmtContext<'_, S, N>, mut writer: Writer<'_>, event: &Event<'_>) -> fmt::Result {
        let mut fields = Fields::default();
        event.record(&mut fields);
        let level = *event.metadata().level();
        if self.person {
            return writeln!(writer, "{}", self.person_line(level, &fields));
        }
        let mut spans = Vec::new();
        if let Some(scope) = context.event_scope() {
            for span in scope.from_root() {
                if let Some(formatted) = span.extensions().get::<FormattedFields<N>>() {
                    spans.extend(logfmt_pairs(formatted.as_str()));
                }
            }
        }
        writeln!(writer, "{}", json_line(Timestamp::now(), level, &fields, &spans))
    }
}

impl Lines {
    /// `11:02:11  linked     us.internal:7444`, or the level, message and
    /// fields of an event `LABELS` does not name.
    fn person_line(&self, level: Level, fields: &Fields) -> String {
        let paint = |style: Style, text: &str| paint(self.colours, style, text);
        let time = Zoned::now().strftime("%H:%M:%S").to_string();
        let labelled = LABELS.iter().find(|(message, ..)| *message == fields.message);
        let (word, said) = match labelled {
            Some((_, word, shape)) => (*word, filled(shape, fields)),
            None => ("", None),
        };
        let word = if word.is_empty() { name(level).trim_end() } else { word };
        let shade = match level {
            Level::ERROR => Color::Red.bold(),
            Level::WARN => Color::Yellow.normal(),
            _ if labelled.is_some() => Color::Green.normal(),
            Level::INFO => Color::Cyan.normal(),
            Level::DEBUG | Level::TRACE => Color::Purple.dimmed(),
        };
        let mut line = format!("{}  {} ", paint(dim(), &time), paint(shade, &format!("{word:<10}")));
        match said {
            Some(said) => line.push_str(&said),
            None => {
                line.push_str(&fields.message);
                for (key, value) in &fields.named {
                    let _ = write!(line, " {}{}", paint(dim(), &format!("{key}=")), logfmt(value));
                }
            }
        }
        line
    }
}

/// A label's shape filled from an event's fields, its parts that name a
/// missing field left out; none when no part is left.
fn filled(shape: &str, fields: &Fields) -> Option<String> {
    let parts: Vec<String> = shape
        .split(" · ")
        .filter_map(|part| {
            let mut out = String::new();
            let mut rest = part;
            while let Some(open) = rest.find('{') {
                let close = open + rest[open..].find('}')?;
                out.push_str(&rest[..open]);
                let name = &rest[open + 1..close];
                let (_, value) = fields.named.iter().find(|(key, _)| *key == name)?;
                out.push_str(&value.text());
                rest = &rest[close + 1..];
            }
            out.push_str(rest);
            Some(out)
        })
        .collect();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// One JSON object: time, level, stream, msg, the spans' fields, then the
/// event's, as the console's JSON line has them.
fn json_line(at: Timestamp, level: Level, fields: &Fields, spans: &[(String, String)]) -> String {
    let level = name(level).trim_end().to_ascii_uppercase();
    let mut pairs: Vec<(&str, serde_json::Value)> = vec![
        ("time", utc(at).into()),
        ("level", level.into()),
        ("stream", "tinystore".into()),
        ("msg", fields.message.clone().into()),
    ];
    pairs.extend(spans.iter().map(|(key, value)| (key.as_str(), value.clone().into())));
    pairs.extend(fields.named.iter().map(|(key, value)| (*key, value.json())));
    // written by hand, so that its keys keep this order: a JSON map sorts them
    let written: Vec<String> =
        pairs.iter().map(|(key, value)| format!("{}:{value}", serde_json::Value::String((*key).to_owned()))).collect();
    format!("{{{}}}", written.join(","))
}

/// The `key=value` pairs a span's fields were formatted as.
fn logfmt_pairs(formatted: &str) -> Vec<(String, String)> {
    formatted
        .split_whitespace()
        .filter_map(|pair| pair.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.trim_matches('"').to_owned()))
        .collect()
}

/// A file's time: UTC to the millisecond, every line as wide.
fn utc(at: Timestamp) -> String {
    format!("{}.{:03}Z", at.strftime("%Y-%m-%dT%H:%M:%S"), at.subsec_millisecond())
}

/// A level's word, padded so that messages line up.
fn name(level: Level) -> &'static str {
    match level {
        Level::ERROR => "error",
        Level::WARN => "warn ",
        Level::INFO => "info ",
        Level::DEBUG => "debug",
        Level::TRACE => "trace",
    }
}

fn dim() -> Style {
    Style::new().dimmed()
}

pub(crate) fn paint(colours: bool, style: Style, text: impl fmt::Display) -> String {
    if colours { style.paint(text.to_string()).to_string() } else { text.to_string() }
}

/// A field's value as the event gave it.
#[derive(Clone, Debug, PartialEq)]
enum Value {
    Text(String),
    Number(serde_json::Number),
    Bool(bool),
}

impl Value {
    fn text(&self) -> String {
        match self {
            Value::Text(text) => text.clone(),
            Value::Number(number) => number.to_string(),
            Value::Bool(bool) => bool.to_string(),
        }
    }

    fn json(&self) -> serde_json::Value {
        match self {
            Value::Text(text) => text.clone().into(),
            Value::Number(number) => serde_json::Value::Number(number.clone()),
            Value::Bool(bool) => (*bool).into(),
        }
    }
}

/// A value as logfmt writes one: quoted when it holds a space, a quote or an
/// `=`.
fn logfmt(value: &Value) -> String {
    let text = value.text();
    if matches!(value, Value::Text(_)) && (text.contains([' ', '"', '=']) || text.is_empty()) {
        format!("{text:?}")
    } else {
        text
    }
}

/// An event's message and its fields.
#[derive(Default)]
struct Fields {
    message: String,
    named: Vec<(&'static str, Value)>,
}

impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.keep(field, Value::Text(value.to_owned()));
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.keep(field, Value::Number(value.into()));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.keep(field, Value::Number(value.into()));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.keep(field, Value::Bool(value));
    }

    // Without it tracing hands a float to `record_debug`, and JSON gets a string.
    fn record_f64(&mut self, field: &Field, value: f64) {
        let number = serde_json::Number::from_f64(value);
        self.keep(field, number.map_or_else(|| Value::Text(value.to_string()), Value::Number));
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.keep(field, Value::Text(format!("{value:?}")));
    }
}

impl Fields {
    fn keep(&mut self, field: &Field, value: Value) {
        match (field.name(), value) {
            ("message", value) => self.message = value.text(),
            (name, value) => self.named.push((name, value)),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    #[test]
    fn a_terminal_shows_colours_unless_someone_said_otherwise() {
        let terminal = |no_color, force_color, dumb, terminal| Terminal { no_color, force_color, dumb, terminal };
        assert!(terminal(false, false, false, true).colours());
        assert!(!terminal(false, false, false, false).colours(), "a pipe or a file");
        assert!(!terminal(true, true, false, true).colours(), "NO_COLOR wins");
        assert!(terminal(false, true, true, false).colours(), "FORCE_COLOR, through a pipe too");
        assert!(!terminal(false, false, true, true).colours(), "TERM=dumb");
    }

    /// What `lines` print of the events `log` logs.
    fn printed(lines: Lines, log: impl FnOnce()) -> String {
        let written = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&written);
        let subscriber =
            tracing_subscriber::fmt().event_format(lines).with_writer(move || Sink(Arc::clone(&sink))).finish();
        tracing::subscriber::with_default(subscriber, log);
        String::from_utf8(written.lock().unwrap().clone()).unwrap()
    }

    #[test]
    fn a_files_line_is_one_json_object_with_its_fields_as_they_were_given() {
        let line = printed(Lines { colours: false, person: false }, || {
            tracing::info!(dir = r"C:\data", clients = 2, why = "a client said stop", "stopping");
        });
        let object: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(object["msg"], "stopping");
        assert_eq!(object["level"], "INFO");
        assert_eq!(object["dir"], r"C:\data");
        assert_eq!(object["clients"], 2, "a number stays one");
        assert!(object["time"].as_str().unwrap().ends_with('Z'));
    }

    #[test]
    fn a_persons_terminal_names_a_known_event_by_its_word() {
        let lines = printed(Lines { colours: false, person: true }, || {
            tracing::info!(client = "tinystore-bun/0.1.0", clients = 1, "a client connected");
            tracing::warn!(
                from = "us:7444",
                error = "the node said nothing for too long",
                "a node's link to the node it copies"
            );
            tracing::info!(why = "interrupted", "stopping");
            tracing::info!(work = "kv", "background work recovered");
        });
        let shown: Vec<&str> = lines.lines().map(|line| &line[10..]).collect();
        assert_eq!(
            shown,
            [
                "client     tinystore-bun/0.1.0 · 1 connected",
                "warn       us:7444 · the node said nothing for too long",
                "stopping   interrupted",
                "info       background work recovered work=kv",
            ]
        );
    }

    #[test]
    fn every_labelled_event_is_one_the_server_or_the_library_logs() {
        let sources = [
            include_str!("serve.rs"),
            include_str!("../../tinystore/src/replication/follow.rs"),
            include_str!("../../tinystore/src/replication/serve.rs"),
            include_str!("../../tinystore/src/replication/handover.rs"),
            include_str!("../../tinystore/src/replication/judge.rs"),
            include_str!("../../tinystore/src/replication/witness.rs"),
        ];
        for (message, ..) in LABELS {
            let logged = format!("\"{message}\"");
            assert!(sources.iter().any(|source| source.contains(&logged)), "nothing logs {logged}");
        }
    }

    #[test]
    fn an_unset_or_unknown_level_logs_info() {
        assert_eq!(level_named(None), LevelFilter::INFO);
        assert_eq!(level_named(Some("")), LevelFilter::INFO, "not tracing's error");
        assert_eq!(level_named(Some("loud")), LevelFilter::INFO);
        assert_eq!(level_named(Some("DEBUG")), LevelFilter::DEBUG);
        assert_eq!(level_named(Some(" warn ")), LevelFilter::WARN);
    }

    #[test]
    fn stderr_shows_from_log_console_level_else_warnings_when_quiet_else_what_is_logged() {
        let logged = LevelFilter::INFO;
        assert_eq!(stderr_level_named(Some("debug"), true, logged), LevelFilter::DEBUG, "over --quiet");
        assert_eq!(stderr_level_named(Some(" error "), false, logged), LevelFilter::ERROR);
        assert_eq!(stderr_level_named(None, true, logged), LevelFilter::WARN);
        assert_eq!(stderr_level_named(Some("loud"), true, LevelFilter::ERROR), LevelFilter::ERROR);
        assert_eq!(stderr_level_named(Some(""), false, logged), LevelFilter::INFO, "not tracing's error");
    }

    #[test]
    fn a_files_time_is_utc_to_the_millisecond() {
        let at = Timestamp::from_millisecond(1_791_547_200_007).unwrap();
        assert_eq!(utc(at), "2026-10-09T12:00:00.007Z");
    }

    struct Sink(Arc<Mutex<Vec<u8>>>);

    impl io::Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
}
