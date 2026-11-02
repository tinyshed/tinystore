//! The tool `logs`, the application's latest lines and events, and an
//! event as the line of JSON that `tinystore logs --json` prints too.

use std::sync::Arc;

use serde_json::{Value, json};

use super::given::span;
use super::store::{Arguments, Opener, count, opened, schema, text};
use super::tool::Tool;
use crate::records::{Event, Level};

pub(super) fn tools(open: &Opener) -> Vec<Tool> {
    let open = Arc::clone(open);
    let takes = json!({
        "since": text("only lines from this long ago, as 30s, 15m, 1h or 2d"),
        "level": text("trace, debug, info, warn or error, and above"),
        "grep": text("only lines that hold this text, in any case"),
        "stream": text("one stream, as api"),
        "limit": count("how many of the last lines; 50 by default, 200 at most"),
    });
    let description = "The application's latest log lines and events, newest last.";
    vec![Tool::reads("logs", description, schema(takes, &[]), move |given| {
        Ok(latest(&open, &Arguments { tool: "logs", given })?)
    })]
}

/// The lines a call asks for, the last `limit` of them, oldest first.
fn latest(open: &Opener, given: &Arguments) -> Result<Value, String> {
    let store = opened(open)?;
    let events = store.all_events().map_err(|error| error.to_string())?;
    let mut list = events.list().newest().limit(given.limit(50));
    if let Some(since) = given.text("since") {
        list = list.since(span(since)?);
    }
    if let Some(level) = given.text("level") {
        let level = level_named(level).ok_or_else(|| format!("no level {level}: trace, debug, info, warn or error"))?;
        list = list.min_level(level);
    }
    if let Some(grep) = given.text("grep") {
        list = list.search(grep);
    }
    if let Some(stream) = given.text("stream") {
        list = list.streams([stream]);
    }
    let mut page = list.page().map_err(|error| error.to_string())?.events;
    page.reverse();
    let lines: Vec<Value> =
        page.iter().map(|event| serde_json::from_str(&json(event)).unwrap_or(Value::Null)).collect();
    Ok(Value::Array(lines))
}

/// The level a person names, in any case: `warn` and `warning` alike.
pub fn level_named(name: &str) -> Option<Level> {
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

/// One JSON object, as the console prints a line of it: time, level,
/// stream, the event's name for a line other than a log line, its id as
/// `event_id`, msg, the context's fields, the event's, the trace and the
/// span. A field keeps its spelling, and a key repeated stays repeated, as
/// the console and Go's `slog` leave it.
pub fn json(event: &Event) -> String {
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
    let quoted = Value::String(text.to_owned()).to_string();
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
    use std::time::{Duration, SystemTime};

    use super::*;
    use crate::records::{Fields, SpanId, TraceId};

    /// The fields a vector's `[key, value]` pairs give, each value as JSON writes it.
    fn fields(pairs: Option<&Value>) -> Fields {
        let pairs = pairs.and_then(Value::as_array).cloned().unwrap_or_default();
        let written: Vec<String> = pairs.iter().map(|pair| format!("{}:{}", pair[0], pair[1])).collect();
        Fields::read(&format!("{{{}}}", written.join(","))).unwrap()
    }

    fn bytes<const N: usize>(hex: &str) -> [u8; N] {
        std::array::from_fn(|at| u8::from_str_radix(&hex[at * 2..at * 2 + 2], 16).unwrap())
    }

    #[test]
    fn a_json_line_is_the_consoles_byte_for_byte() {
        let vectors: Value = serde_json::from_str(include_str!("../../../../testdata/console/console.json")).unwrap();
        let secrets: Vec<&str> = vectors["secrets"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
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
                at: SystemTime::UNIX_EPOCH + Duration::from_nanos(nanos),
                stream: line["stream"].as_str().unwrap().to_owned(),
                id: None,
                name: line["event"].as_str().unwrap_or("log").to_owned(),
                level: line["level"].as_i64().map(|number| Level::of(number as i32)),
                message: line["msg"].as_str().map(str::to_owned),
                fields: fields(line.get("attrs")),
                context: fields(line.get("context")),
                trace_id: line["trace_id"].as_str().map(|hex| TraceId(bytes(hex))),
                span_id: line["span_id"].as_str().map(|hex| SpanId(bytes(hex))),
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
            fields: Fields::read(r#"{"id":42}"#).unwrap(),
            context: Fields::default(),
            trace_id: None,
            span_id: None,
        };
        let line = json(&event);
        assert!(line.contains(r#""event":"push","event_id":"d-1","id":42"#), "{line}");
    }
}
