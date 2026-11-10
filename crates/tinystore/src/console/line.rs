//! A console line: pretty for a person at a terminal, one JSON object a line
//! for a collector, the same bytes the Go, Bun and Python loggers print, as
//! `testdata/console/console.json` keeps them.
//!
//! ```text
//! 11:02:11.123 WARN  api  slow request  ms=1200
//! {"time":"2026-10-02T11:02:11.123Z","level":"WARN","stream":"api","msg":"slow request","ms":1200}
//! ```

use std::fmt::{self, Write as _};

use jiff::tz::TimeZone;
use jiff::{Timestamp, Zoned};
use serde::de::{Deserializer as _, MapAccess, Visitor};
use serde_json::value::RawValue;

/// A log line's event name; a line of any other name is an event's.
pub(crate) const LOG: &str = "log";
/// The field where a line says where it was logged:
/// `{"function":"main.run","file":"/app/main.go","line":42}`.
pub(crate) const SOURCE: &str = "source";

/// What a console prints of an event.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Line {
    /// Nanoseconds since 1970.
    pub(crate) at: i64,
    pub(crate) stream: String,
    pub(crate) name: String,
    /// slog's number: -4 debug, 0 info, 4 warn, 8 error; an event has none.
    pub(crate) level: Option<i32>,
    pub(crate) message: Option<String>,
    /// Each a key and the JSON of its value: what the line was logged in,
    /// and what the call gave.
    pub(crate) context: Vec<(String, String)>,
    pub(crate) fields: Vec<(String, String)>,
    pub(crate) trace: Option<[u8; 16]>,
    pub(crate) span: Option<[u8; 8]>,
}

/// How a pretty line's time shows; a JSON line always has its own.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Time {
    /// `11:02:11.123`.
    #[default]
    Clock,
    /// `2026-10-02 11:02:11.123 +03:00`.
    Full,
    /// None, where docker or journald stamp each line.
    Off,
}

/// How a pretty line shows.
#[derive(Clone, Debug)]
pub(crate) struct Look {
    pub(crate) color: bool,
    pub(crate) zone: TimeZone,
    pub(crate) time: Time,
    pub(crate) hide_stream: bool,
}

// the colours of a pretty line, the terminal's own sixteen so that its theme chooses the shades
const RESET: &str = "\x1b[0m";
const DIM: &str = "\x1b[2m";
const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const BLUE: &str = "\x1b[34m";
const MAGENTA: &str = "\x1b[35m";
const CYAN: &str = "\x1b[36m";

/// A line as one JSON object and a newline, its keys in this order: time,
/// level, stream, the event's name for a line other than a log line, msg,
/// the context's fields, the call's, the trace and the span. The time is UTC
/// to the millisecond, as JavaScript's Date gives it; a field keeps its
/// spelling, and a key repeated stays repeated.
pub(crate) fn json(line: &Line) -> String {
    let mut out = String::from("{\"time\":\"");
    out.push_str(&stamp(line.at, &TimeZone::UTC, true));
    out.push('"');
    if let Some(level) = line.level {
        out.push_str(",\"level\":");
        quote(&mut out, &level_name(level));
    }
    out.push_str(",\"stream\":");
    quote(&mut out, &line.stream);
    if line.name != LOG {
        out.push_str(",\"event\":");
        quote(&mut out, &line.name);
    }
    if let Some(message) = &line.message {
        out.push_str(",\"msg\":");
        quote(&mut out, message);
    }
    for (key, value) in line.context.iter().chain(&line.fields) {
        out.push(',');
        quote(&mut out, key);
        out.push(':');
        out.push_str(value);
    }
    if let Some(trace) = line.trace {
        let _ = write!(out, ",\"trace_id\":\"{}\"", crate::text::hex(&trace));
    }
    if let Some(span) = line.span {
        let _ = write!(out, ",\"span_id\":\"{}\"", crate::text::hex(&span));
    }
    out.push_str("}\n");
    out
}

/// A line as a person reads it: the time of day, the level padded to five,
/// the stream, the message, the context's fields and the call's, and the
/// trace's first eight digits. A message's further lines, and then a value of
/// several lines, a stack or a traceback, follow the line, indented, so that
/// none of them starts where a line does:
///
/// ```text
/// 11:02:11.123 ERROR api  charge failed  userId=42
///     err: Error: card declined
///         at charge (pay.ts:12:9)
/// ```
///
/// A key, and a string, shows without its quotes when it is one word with no
/// `=`, quote or backslash; a source shows as its file and line; any other
/// value shows as its JSON. What a program logged shows no control character
/// but a tab: the message, the event's name, a value below the line and a
/// source each show it as its escape.
pub(crate) fn pretty(line: &Line, look: &Look) -> String {
    let paint = Painter(look.color);
    let mut out = String::new();
    let time = match look.time {
        Time::Off => None,
        Time::Full => Some(stamp(line.at, &look.zone, false)),
        Time::Clock => Some(clock(line.at, &look.zone)),
    };
    if let Some(time) = time {
        paint.text(&mut out, DIM, &time);
        out.push(' ');
    }
    let (label, hue) = label_of(line.level);
    paint.text(&mut out, hue, &label);
    // the level is padded to five; the columns after it are two spaces apart
    let mut separator = " ".repeat(5_usize.saturating_sub(label.len()) + 1);
    if !look.hide_stream {
        out.push_str(&separator);
        paint.text(&mut out, CYAN, &line.stream);
        separator = "  ".to_owned();
    }
    let (message, further) = message_of(line);
    if !message.is_empty() {
        out.push_str(&separator);
        out.push_str(&message);
        separator = "  ".to_owned();
    }
    let mut below = Vec::new();
    let mut field = |out: &mut String, key: &str, text: &str| {
        out.push_str(&separator);
        separator = " ".to_owned();
        let key = if bare(key) { format!("{key}=") } else { quoted(key) + "=" };
        paint.text(out, DIM, &key);
        out.push_str(text);
    };
    for (key, value) in line.context.iter().chain(&line.fields) {
        match shown(key, value) {
            (text, true) => below.push((key.as_str(), text)),
            (text, false) => field(&mut out, key, &text),
        }
    }
    if let Some(trace) = line.trace {
        field(&mut out, "trace", &crate::text::hex(&trace[..4]));
    }
    for part in further {
        out.push_str("\n    ");
        out.push_str(&escaped(part));
    }
    for (key, text) in below {
        push_below(&mut out, paint, key, &text);
    }
    out.push('\n');
    out
}

/// A level's label and its colour; an event's is `EVENT`.
fn label_of(level: Option<i32>) -> (String, &'static str) {
    match level {
        None => ("EVENT".to_owned(), MAGENTA),
        Some(level) if level < 0 => (level_name(level), BLUE),
        Some(level) if level < 4 => (level_name(level), GREEN),
        Some(level) if level < 8 => (level_name(level), YELLOW),
        Some(level) => (level_name(level), RED),
    }
}

/// A value of more than one line, below its line: each line of it indented,
/// the first after its key.
fn push_below(out: &mut String, paint: Painter, key: &str, text: &str) {
    for (place, part) in lines_of(text).enumerate() {
        out.push_str("\n    ");
        let part = if place == 0 { escaped(&format!("{key}: {part}")) } else { escaped(part) };
        paint.text(out, DIM, &part);
    }
}

/// A log line's message, or an event's name and the message it may carry,
/// as the line shows them; and the further lines of a message of several,
/// which go below the line as a value's do.
fn message_of(line: &Line) -> (String, Vec<&str>) {
    let mut lines = lines_of(line.message.as_deref().unwrap_or_default());
    let first = escaped(lines.next().unwrap_or_default());
    let message = match (line.name == LOG, first.is_empty()) {
        (true, _) => first,
        (false, true) => escaped(&line.name),
        (false, false) => format!("{}  {first}", escaped(&line.name)),
    };
    (message, lines.collect())
}

/// The lines of a text of several, without the line ends after the last,
/// and each without the carriage return before its newline.
///
/// ```text
/// "line one\r\nline two\r\n"  →  line one, line two
/// ```
fn lines_of(text: &str) -> impl Iterator<Item = &str> {
    text.trim_end_matches(['\r', '\n']).split('\n').map(|part| part.strip_suffix('\r').unwrap_or(part))
}

/// Text a program logged as a pretty line shows it: each control character
/// but a tab, and DEL, as its JSON escape, so that none reaches the terminal
/// as an escape sequence, a carriage return or a line of its own. The
/// colours the console paints with are its own, never in this text.
///
/// ```text
/// "\x1b[2J"  →  \u001b[2J    "a\rb"  →  a\rb    "\x7f"  →  \u007f
/// ```
fn escaped(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if (c < ' ' && c != '\t') || c == '\x7f' {
            push_escape(&mut out, c);
        } else {
            out.push(c);
        }
    }
    out
}

/// How a field shows, and whether it is text of several lines, which goes
/// under the line instead.
fn shown(key: &str, value: &str) -> (String, bool) {
    if key == SOURCE
        && let Some(place) = short_source(value)
    {
        return (place, false);
    }
    if !value.starts_with('"') {
        return (value.to_owned(), false);
    }
    let Ok(text) = serde_json::from_str::<String>(value) else { return (value.to_owned(), false) };
    if text.contains('\n') {
        (text, true)
    } else if bare(&text) {
        (text, false)
    } else {
        (value.to_owned(), false)
    }
}

/// A source as a pretty line shows it, its file's directory and name and its
/// line, as zap's console does:
///
/// ```text
/// {"function":"main.run","file":"/home/ann/app/server/main.go","line":42}  →  server/main.go:42
/// ```
fn short_source(spelled: &str) -> Option<String> {
    let source: serde_json::Value = spelled.starts_with('{').then(|| serde_json::from_str(spelled).ok()).flatten()?;
    let file = source.get("file").map_or(Some(""), serde_json::Value::as_str)?;
    let line = source.get("line").map_or(Some(0), serde_json::Value::as_i64)?;
    if file.is_empty() {
        return None;
    }
    let parts: Vec<&str> = file.split(['/', '\\']).filter(|part| !part.is_empty()).collect();
    Some(escaped(&format!("{}:{line}", parts[parts.len().saturating_sub(2)..].join("/"))))
}

/// Text a person reads without its quotes: not empty, and no character a
/// space or below, DEL, `"`, `=` or `\`.
fn bare(text: &str) -> bool {
    !text.is_empty() && !text.chars().any(|c| c <= ' ' || c == '\x7f' || c == '"' || c == '=' || c == '\\')
}

/// A level as slog spells it: `INFO`, `WARN+2`, `DEBUG-4`.
pub(crate) fn level_name(level: i32) -> String {
    let (base, at) = match level {
        level if level < 0 => ("DEBUG", -4),
        level if level < 4 => ("INFO", 0),
        level if level < 8 => ("WARN", 4),
        _ => ("ERROR", 8),
    };
    match level - at {
        0 => base.to_owned(),
        off => format!("{base}{off:+}"),
    }
}

/// A time in a zone to the millisecond: `2026-10-02T11:02:11.123Z` as JSON
/// writes it in UTC, `2026-10-02 11:02:11.123 +03:00` as a full pretty line.
fn stamp(at: i64, zone: &TimeZone, json: bool) -> String {
    let zoned = zoned(at, zone);
    let (date, time) = (zoned.date(), time_of(&zoned));
    let day = format!("{:04}-{:02}-{:02}", date.year(), date.month(), date.day());
    if json {
        return format!("{day}T{time}Z");
    }
    let offset = zoned.offset().seconds();
    let sign = if offset < 0 { '-' } else { '+' };
    format!("{day} {time} {sign}{:02}:{:02}", offset.abs() / 3600, offset.abs() % 3600 / 60)
}

/// The time of day in a zone, to the millisecond: `11:02:11.123`.
fn clock(at: i64, zone: &TimeZone) -> String {
    time_of(&zoned(at, zone))
}

/// `at`, nanoseconds since 1970, in `zone`.
fn zoned(at: i64, zone: &TimeZone) -> Zoned {
    Timestamp::from_nanosecond(i128::from(at)).unwrap_or(Timestamp::UNIX_EPOCH).to_zoned(zone.clone())
}

fn time_of(zoned: &Zoned) -> String {
    let millis = zoned.subsec_nanosecond() / 1_000_000;
    format!("{:02}:{:02}:{:02}.{millis:03}", zoned.hour(), zoned.minute(), zoned.second())
}

/// Appends `text` as a JSON string, as Go's encoding/json writes one without
/// escaping HTML: control characters and the line and paragraph separators
/// escaped.
pub(crate) fn quote(out: &mut String, text: &str) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c < ' ' || c == '\u{2028}' || c == '\u{2029}' => push_escape(out, c),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Appends a character's JSON escape, the short one where JSON has one:
/// `\n`, `\t`, `\u001b`.
fn push_escape(out: &mut String, c: char) {
    match c {
        '\n' => out.push_str("\\n"),
        '\r' => out.push_str("\\r"),
        '\t' => out.push_str("\\t"),
        '\u{8}' => out.push_str("\\b"),
        '\u{c}' => out.push_str("\\f"),
        c => {
            let _ = write!(out, "\\u{:04x}", u32::from(c));
        }
    }
}

pub(crate) fn quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    quote(&mut out, text);
    out
}

/// A JSON object's entries in their order, each value as it is spelled, so
/// that `1.2300`, an integer past 2⁵³ and a key written twice stay as they
/// were; `null` is none.
pub(crate) fn object_pairs(object: &str) -> serde_json::Result<Vec<(String, String)>> {
    let mut reading = serde_json::Deserializer::from_str(object);
    let pairs = reading.deserialize_any(InOrder)?;
    reading.end()?;
    Ok(pairs)
}

struct InOrder;

impl<'de> Visitor<'de> for InOrder {
    type Value = Vec<(String, String)>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("an object")
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(Vec::new())
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut pairs = Vec::new();
        while let Some((key, value)) = map.next_entry::<String, Box<RawValue>>()? {
            pairs.push((key, value.get().to_owned()));
        }
        Ok(pairs)
    }
}

/// Colours text when its line goes to a terminal that shows them.
#[derive(Clone, Copy)]
struct Painter(bool);

impl Painter {
    fn text(self, out: &mut String, hue: &str, text: &str) {
        if self.0 {
            out.push_str(hue);
            out.push_str(text);
            out.push_str(RESET);
        } else {
            out.push_str(text);
        }
    }
}

#[cfg(test)]
#[path = "line_tests.rs"]
mod tests;
