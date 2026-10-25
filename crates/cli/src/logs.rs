//! `tinystore logs`: the application's logs and events from its store's
//! `records.db`, printed as its logger prints them, and with `-f` the new
//! ones as they arrive. It reads beside the program or server that holds the
//! directory and writes nothing.
//!
//! ```text
//! 11:02:14.502 WARN  api  slow request  requestId=7f3a ms=1200
//! 11:02:15.911 ERROR api  payment failed  orderId=981 reason="card declined"
//! ```
//!
//! Following reads again every half second from a little before the newest
//! line printed, since another process's line may commit late with an
//! earlier time, and skips the lines it printed already.

use std::collections::HashMap;
use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use jiff::Zoned;
use nu_ansi_term::{Color, Style};
use tinystore::records::{AllEvents, Event, Level, List};
use tinystore::{ErrorKind, Store};

use crate::asked::{self, Asked, Done, failed, wrong};
use crate::help;
use crate::lines::{Terminal, paint};

/// How many of the last lines print without `-n` or `--since`.
const LAST: usize = 20;
/// How often following reads again.
const EVERY: Duration = Duration::from_millis(500);
/// How far before the newest line printed following reads again: a line
/// another process logged may commit up to a second late, its buffer's.
const LATE: Duration = Duration::from_secs(10);

pub(crate) fn run(args: &[String]) -> ExitCode {
    asked::run(&help::LOGS, args, logs)
}

/// What `logs` was asked.
struct Wanted {
    last: Option<usize>,
    level: Option<Level>,
    since: Option<Duration>,
    grep: Option<String>,
    stream: Option<String>,
    follow: bool,
    json: bool,
}

fn logs(asked: Asked) -> Done {
    let (dir, rest) = asked.dir_or_here(0)?;
    if !rest.is_empty() {
        return Err(wrong(format!("one directory, not {} too", rest.join(" "))));
    }
    let wanted = Wanted {
        last: asked
            .value("-n")
            .map(|count| count.parse().map_err(|_| wrong(format!("-n {count}: a count"))))
            .transpose()?,
        level: asked.value("--level").map(level).transpose()?,
        since: asked.value("--since").map(asked::span).transpose().map_err(wrong)?,
        grep: asked.value("--grep").map(str::to_owned),
        stream: asked.value("--stream").map(str::to_owned),
        follow: asked.has("-f"),
        json: asked.has("--json"),
    };
    let printer = Printer { colours: Terminal::of_stdout().colours(), json: wanted.json, today: today() };
    let mut printed = Printed::default();
    match read(&dir, |events| first(&narrowed(events.list(), &wanted, None), &wanted))? {
        Some(events) => {
            for event in events {
                printer.print(&event);
                printed.add(&event);
            }
        }
        None => println!(
            "{} holds no logs yet: a program's logger keeps them, as store.logger(\"api\") does",
            dir.display()
        ),
    }
    if wanted.follow {
        follow(&dir, &wanted, &printer, printed)?;
    }
    Ok(())
}

/// Prints the lines that arrive from now on, until Ctrl+C. The first read
/// again meets lines of before the first page that it left out, and passes
/// over those as old as the newest it printed.
fn follow(dir: &Path, wanted: &Wanted, printer: &Printer, mut printed: Printed) -> Done {
    let started = printed.newest.unwrap_or_else(SystemTime::now);
    let mut first = true;
    loop {
        let from = printed.newest.unwrap_or(started) - LATE;
        let found = read(dir, |events| {
            narrowed(events.list(), wanted, Some(from)).all().collect::<tinystore::Result<Vec<_>>>()
        })?;
        for event in found.unwrap_or_default() {
            if printed.add(&event) && (!first || event.at > started) {
                printer.print(&event);
            }
        }
        first = false;
        printed.forget_before(from);
        std::thread::sleep(EVERY);
    }
}

/// What `read` makes of the store's events, the store opened again beside
/// whoever holds it, so that it meets the streams made since; none while the
/// store keeps no events.
fn read<T>(dir: &Path, read: impl FnOnce(&AllEvents) -> tinystore::Result<T>) -> Result<Option<T>, asked::Refused> {
    let store = Store::open_beside(dir).map_err(|error| match error.kind() {
        ErrorKind::NotFound => failed(format!("{}: no such directory", dir.display())),
        _ => error.into(),
    })?;
    let events = match store.all_events() {
        Ok(events) => events,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    Ok(Some(read(&events)?))
}

fn level(name: &str) -> Result<Level, asked::Refused> {
    level_named(name).ok_or_else(|| wrong(format!("--level {name}: trace, debug, info, warn or error")))
}

/// The level a person names, in any case: `warn` and `warning` alike.
pub(crate) fn level_named(name: &str) -> Option<Level> {
    match name.to_ascii_lowercase().as_str() {
        "trace" => Some(Level::TRACE),
        "debug" => Some(Level::DEBUG),
        "info" => Some(Level::INFO),
        "warn" | "warning" => Some(Level::WARN),
        "error" => Some(Level::ERROR),
        "fatal" => Some(Level::FATAL),
        _ => None,
    }
}

/// The read every page of `logs` makes, from `from` on when following.
fn narrowed(mut list: List, wanted: &Wanted, from: Option<SystemTime>) -> List {
    if let Some(level) = wanted.level {
        list = list.min_level(level);
    }
    if let Some(stream) = &wanted.stream {
        list = list.streams([stream.as_str()]);
    }
    if let Some(grep) = &wanted.grep {
        list = list.search(grep);
    }
    match (from, wanted.since) {
        (Some(from), _) => list.from(from),
        (None, Some(since)) => list.since(since),
        (None, None) => list,
    }
}

/// The lines printed first, oldest first: every line of `--since` unless
/// `-n` says how many of the last, or the last twenty.
fn first(list: &List, wanted: &Wanted) -> tinystore::Result<Vec<Event>> {
    let last = match (wanted.last, wanted.since) {
        (Some(last), _) => last,
        (None, Some(_)) => return list.clone().all().collect(),
        (None, None) => LAST,
    };
    let mut events: Vec<Event> = list.clone().newest().all().take(last).collect::<Result<_, _>>()?;
    events.reverse();
    Ok(events)
}

/// The lines printed lately, so that a read again prints each once: by what
/// each holds, counted, so that two alike both print.
#[derive(Default)]
struct Printed {
    seen: HashMap<String, usize>,
    /// What this read met of each, counted again from none at every read.
    met: HashMap<String, usize>,
    newest: Option<SystemTime>,
}

impl Printed {
    /// Counts the line met; true when it was not printed before.
    fn add(&mut self, event: &Event) -> bool {
        let key = identity(event);
        let met = self.met.entry(key.clone()).or_default();
        *met += 1;
        let seen = self.seen.entry(key).or_default();
        let new = *met > *seen;
        if new {
            *seen = *met;
            self.newest = self.newest.max(Some(event.at));
        }
        new
    }

    /// Lets go of the lines before the next read's start, and starts the
    /// next read's counts.
    fn forget_before(&mut self, from: SystemTime) {
        let from = nanos(from);
        self.seen.retain(|key, _| {
            key.split_once(' ').and_then(|(at, _)| at.parse::<u128>().ok()).is_some_and(|at| at >= from)
        });
        self.met.clear();
    }
}

/// What tells two lines apart: their time, stream, name, message and fields.
fn identity(event: &Event) -> String {
    let fields: Vec<String> = event.fields.iter().map(|(key, json)| format!("{key}={json}")).collect();
    format!(
        "{} {}\u{0}{}\u{0}{}\u{0}{}",
        nanos(event.at),
        event.stream,
        event.name,
        event.message.as_deref().unwrap_or_default(),
        fields.join("\u{0}")
    )
}

fn nanos(at: SystemTime) -> u128 {
    at.duration_since(UNIX_EPOCH).map_or(0, |since| since.as_nanos())
}

fn today() -> String {
    Zoned::now().strftime("%Y-%m-%d").to_string()
}

/// How a line prints: as the console's pretty line, or one JSON object.
struct Printer {
    colours: bool,
    json: bool,
    /// A line of another day says its date.
    today: String,
}

impl Printer {
    fn print(&self, event: &Event) {
        if self.json {
            println!("{}", json(event));
        } else {
            println!("{}", self.pretty(event));
        }
    }

    /// `11:02:14.502 WARN  api  slow request  requestId=7f3a ms=1200`
    fn pretty(&self, event: &Event) -> String {
        let paint = |style: Style, text: &str| paint(self.colours, style, text);
        let dim = Style::new().dimmed();
        let at = jiff::Timestamp::try_from(event.at).map(|at| at.to_zoned(jiff::tz::TimeZone::system()));
        let time = match at {
            Ok(at) if at.strftime("%Y-%m-%d").to_string() == self.today => at.strftime("%H:%M:%S%.3f").to_string(),
            Ok(at) => at.strftime("%Y-%m-%d %H:%M:%S%.3f").to_string(),
            Err(_) => "?".to_owned(),
        };
        let level = event.level.map_or_else(|| "EVENT".to_owned(), |level| level.name().to_ascii_uppercase());
        let shade = match event.level {
            Some(level) if level >= Level::ERROR => Color::Red.bold(),
            Some(level) if level >= Level::WARN => Color::Yellow.normal(),
            Some(level) if level >= Level::INFO => Color::Cyan.normal(),
            Some(_) => Color::Purple.dimmed(),
            None => Color::Green.normal(),
        };
        let said = event.message.as_deref().filter(|message| !message.is_empty()).unwrap_or(&event.name);
        let mut line = format!(
            "{} {} {}  {}",
            paint(dim, &time),
            paint(shade, &format!("{level:<5}")),
            paint(Style::new().bold(), &event.stream),
            said
        );
        let fields: Vec<String> = event
            .fields
            .iter()
            .chain(event.context.iter())
            .map(|(key, json)| format!("{}{}", paint(dim, &format!("{key}=")), value(json)))
            .collect();
        if !fields.is_empty() {
            line.push_str("  ");
            line.push_str(&fields.join(" "));
        }
        line
    }
}

/// A field's value as a pretty line shows it: a string bare unless it holds
/// a space, a quote or an `=`, anything else as its JSON.
fn value(json: &str) -> String {
    match serde_json::from_str::<String>(json) {
        Ok(text) if !text.is_empty() && !text.contains([' ', '"', '=']) => text,
        _ => json.to_owned(),
    }
}

/// One JSON object, as the console prints a line of it: time, level,
/// stream, the event's name for a line other than a log line, its id as
/// `event_id`, msg, the context's fields, the event's, the trace and the
/// span. A field keeps its spelling, and a key repeated stays repeated, as
/// the console and Go's `slog` leave it.
pub(crate) fn json(event: &Event) -> String {
    let at = jiff::Timestamp::try_from(event.at)
        .map_or_else(|_| String::new(), |at| at.strftime("%Y-%m-%dT%H:%M:%S%.3fZ").to_string());
    let mut pairs = vec![("time".to_owned(), quoted(&at))];
    if let Some(level) = event.level {
        pairs.push(("level".to_owned(), quoted(&level_name(level.number()))));
    }
    pairs.push(("stream".to_owned(), quoted(&event.stream)));
    if event.name != "log" {
        pairs.push(("event".to_owned(), quoted(&event.name)));
    }
    if let Some(id) = &event.id {
        pairs.push(("event_id".to_owned(), quoted(id)));
    }
    if let Some(message) = &event.message {
        pairs.push(("msg".to_owned(), quoted(message)));
    }
    pairs.extend(event.context.iter().chain(event.fields.iter()).map(|(key, json)| (key.to_owned(), json.to_owned())));
    if let Some(trace) = event.trace_id {
        pairs.push(("trace_id".to_owned(), quoted(&trace.to_string())));
    }
    if let Some(span) = event.span_id {
        pairs.push(("span_id".to_owned(), quoted(&span.to_string())));
    }
    // written by hand, so that its keys keep this order: a JSON map sorts them
    let written: Vec<String> = pairs.iter().map(|(key, json)| format!("{}:{json}", quoted(key))).collect();
    format!("{{{}}}", written.join(","))
}

/// A text as the console quotes it, as Go's encoding/json does without
/// escaping HTML: the line and paragraph separators escaped too.
fn quoted(text: &str) -> String {
    let quoted = serde_json::Value::String(text.to_owned()).to_string();
    quoted.replace('\u{2028}', "\\u2028").replace('\u{2029}', "\\u2029")
}

/// A level as the console names it: one of four names, and how far past it
/// a level between two lies, `INFO+2`.
fn level_name(number: i32) -> String {
    let (base, at) = match number {
        number if number < 0 => ("DEBUG", -4),
        number if number < 4 => ("INFO", 0),
        number if number < 8 => ("WARN", 4),
        _ => ("ERROR", 8),
    };
    match number - at {
        0 => base.to_owned(),
        off => format!("{base}{off:+}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tinystore::Options;

    fn wanted() -> Wanted {
        Wanted { last: None, level: None, since: None, grep: None, stream: None, follow: false, json: false }
    }

    #[test]
    fn a_line_prints_as_the_loggers_pretty_line() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), Options { background: false, ..Options::default() }).unwrap();
        let api = store.events("api").open().unwrap();
        api.event("log")
            .level(Level::WARN)
            .message("slow request")
            .append(&serde_json::json!({ "requestId": "7f3a", "ms": 1200, "why": "a b" }))
            .unwrap();
        let beside = Store::open_beside(dir.path()).unwrap();
        let events = first(&narrowed(beside.all_events().unwrap().list(), &wanted(), None), &wanted()).unwrap();
        assert_eq!(events.len(), 1);
        let printer = Printer { colours: false, json: false, today: today() };
        let line = printer.pretty(&events[0]);
        assert!(line.ends_with(" WARN  api  slow request  ms=1200 requestId=7f3a why=\"a b\""), "{line}");
        let object: serde_json::Value = serde_json::from_str(&json(&events[0])).unwrap();
        assert_eq!((object["level"].as_str(), object["ms"].as_u64()), (Some("WARN"), Some(1200)));
    }

    /// The fields a vector's `[key, value]` pairs give, each value as JSON writes it.
    fn fields(pairs: Option<&serde_json::Value>) -> tinystore::records::Fields {
        let pairs = pairs.and_then(serde_json::Value::as_array).cloned().unwrap_or_default();
        let written: Vec<String> = pairs.iter().map(|pair| format!("{}:{}", pair[0], pair[1])).collect();
        tinystore::records::Fields::read(&format!("{{{}}}", written.join(","))).unwrap()
    }

    fn bytes<const N: usize>(hex: &str) -> [u8; N] {
        std::array::from_fn(|at| u8::from_str_radix(&hex[at * 2..at * 2 + 2], 16).unwrap())
    }

    #[test]
    fn a_json_line_is_the_consoles_byte_for_byte() {
        let vectors: serde_json::Value =
            serde_json::from_str(include_str!("../../../testdata/console/console.json")).unwrap();
        let secrets: Vec<&str> =
            vectors["secrets"].as_array().unwrap().iter().filter_map(|word| word.as_str()).collect();
        let mut compared = 0;
        for line in vectors["lines"].as_array().unwrap() {
            // what the console hides of a secret is hidden before an event is kept
            let pairs = [&line["context"], &line["attrs"]].map(|pairs| pairs.as_array().cloned().unwrap_or_default());
            let hides = pairs.iter().flatten().any(|pair| {
                let key = pair[0].as_str().unwrap_or_default().to_ascii_lowercase();
                secrets.iter().any(|secret| key.contains(&secret.replace(' ', "")))
                    || pair[1].to_string().contains("://")
            });
            // a nested object read back through serde_json comes in another key order
            let nested = pairs.iter().flatten().any(|pair| pair[1].is_object());
            if hides || nested || line.get("redact").is_some() || line.get("redact_secrets").is_some() {
                continue;
            }
            let nanos: u64 = line["at"].as_str().unwrap().parse().unwrap();
            let event = Event {
                at: SystemTime::UNIX_EPOCH + std::time::Duration::from_nanos(nanos),
                stream: line["stream"].as_str().unwrap().to_owned(),
                id: None,
                name: line["event"].as_str().unwrap_or("log").to_owned(),
                level: line["level"].as_i64().map(|number| Level::of(number as i32)),
                message: line["msg"].as_str().map(str::to_owned),
                fields: fields(line.get("attrs")),
                context: fields(line.get("context")),
                trace_id: line["trace_id"].as_str().map(|hex| tinystore::records::TraceId(bytes(hex))),
                span_id: line["span_id"].as_str().map(|hex| tinystore::records::SpanId(bytes(hex))),
            };
            assert_eq!(json(&event) + "\n", line["json"].as_str().unwrap(), "{}", line["name"]);
            compared += 1;
        }
        assert!(compared > 20, "only {compared} vectors compared");
    }

    #[test]
    fn an_events_id_is_printed_after_its_name_apart_from_a_field_of_that_name() {
        let event = Event {
            at: SystemTime::UNIX_EPOCH,
            stream: "deploys".to_owned(),
            id: Some("d-1".to_owned()),
            name: "push".to_owned(),
            level: None,
            message: None,
            fields: tinystore::records::Fields::read(r#"{"id":42}"#).unwrap(),
            context: tinystore::records::Fields::default(),
            trace_id: None,
            span_id: None,
        };
        let line = json(&event);
        assert!(line.contains(r#""event":"push","event_id":"d-1","id":42"#), "{line}");
    }

    #[test]
    fn following_prints_each_line_once_and_two_alike_twice() {
        let at = SystemTime::now();
        let event = Event {
            at,
            stream: "api".to_owned(),
            id: None,
            name: "log".to_owned(),
            level: Some(Level::INFO),
            message: Some("tick".to_owned()),
            fields: tinystore::records::Fields::default(),
            context: tinystore::records::Fields::default(),
            trace_id: None,
            span_id: None,
        };
        let mut printed = Printed::default();
        assert!(printed.add(&event));
        assert!(printed.add(&event), "a second alike in the same read");
        printed.forget_before(at - LATE);
        assert!(!printed.add(&event) && !printed.add(&event), "both printed already");
        assert!(printed.add(&event), "a third came");
    }
}
