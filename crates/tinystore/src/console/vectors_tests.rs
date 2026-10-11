use jiff::tz::TimeZone;
use serde_json::Value;

use super::line::{Line, Look, Time, json, pretty};
use super::redact::{Redactor, SECRETS};

/// The console lines every logger prints, Go's, Bun's, Python's and this one,
/// byte for byte.
const VECTORS: &str = include_str!("../../../../testdata/console/console.json");

/// A vector's fields: each a key and its value's JSON as the file spells it,
/// without the spaces between its tokens, the order of an object's keys kept.
fn pairs(vector: &str, which: &str) -> Vec<(String, String)> {
    #[derive(serde::Deserialize)]
    struct Fields {
        #[serde(default)]
        context: Vec<(String, Box<serde_json::value::RawValue>)>,
        #[serde(default)]
        attrs: Vec<(String, Box<serde_json::value::RawValue>)>,
    }
    let fields: Fields = serde_json::from_str(vector).unwrap();
    let pairs = if which == "context" { fields.context } else { fields.attrs };
    pairs.into_iter().map(|(key, value)| (key, compact(value.get()))).collect()
}

/// JSON without the spaces between its tokens.
fn compact(json: &str) -> String {
    let (mut out, mut quoted, mut escaped) = (String::with_capacity(json.len()), false, false);
    for c in json.chars() {
        if quoted {
            out.push(c);
            (escaped, quoted) = (!escaped && c == '\\', escaped || c != '"');
        } else if !c.is_whitespace() {
            out.push(c);
            quoted = c == '"';
        }
    }
    out
}

fn bytes<const N: usize>(text: Option<&Value>) -> Option<[u8; N]> {
    let text = text?.as_str()?;
    let bytes: Vec<u8> =
        (0..text.len()).step_by(2).map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap()).collect();
    bytes.try_into().ok()
}

#[test]
fn every_line_is_the_vectors_bytes_pretty_and_as_json() {
    let vectors: Value = serde_json::from_str(VECTORS).unwrap();
    let secrets: Vec<&str> = vectors["secrets"].as_array().unwrap().iter().map(|name| name.as_str().unwrap()).collect();
    assert_eq!(secrets, SECRETS, "the secrets are the vectors'");
    #[derive(serde::Deserialize)]
    struct Lines<'a> {
        #[serde(borrow)]
        lines: Vec<&'a serde_json::value::RawValue>,
    }
    let raw: Vec<String> =
        serde_json::from_str::<Lines<'_>>(VECTORS).unwrap().lines.iter().map(|line| line.get().to_owned()).collect();
    for (index, vector) in vectors["lines"].as_array().unwrap().iter().enumerate() {
        let name = vector["name"].as_str().unwrap();
        let mut names: Vec<String> = vector["redact"]
            .as_array()
            .map(|names| names.iter().map(|name| name.as_str().unwrap().to_owned()).collect())
            .unwrap_or_default();
        if vector["redact_secrets"] == Value::Bool(true) {
            names.extend(SECRETS.iter().map(|name| (*name).to_owned()));
        }
        let redactor = Redactor::new(&names, vector["keep_url_passwords"] == Value::Bool(true));
        let mut line = Line {
            at: vector["at"].as_str().unwrap().parse().unwrap(),
            stream: vector["stream"].as_str().unwrap().to_owned(),
            name: vector["event"].as_str().unwrap_or("log").to_owned(),
            level: vector["level"].as_i64().map(|level| level as i32),
            message: vector["msg"].as_str().map(str::to_owned),
            context: pairs(&raw[index], "context"),
            fields: pairs(&raw[index], "attrs"),
            trace: bytes(vector.get("trace_id")),
            span: bytes(vector.get("span_id")),
        };
        redactor.fields(&mut line.context);
        redactor.fields(&mut line.fields);
        assert_eq!(json(&line), vector["json"].as_str().unwrap(), "{name}: json");
        let time = match vector["time"].as_str() {
            Some("full") => Time::Full,
            Some("off") => Time::Off,
            _ => Time::Clock,
        };
        let look = Look {
            color: vector["color"] == Value::Bool(true),
            zone: TimeZone::UTC,
            time,
            hide_stream: vector["hide_stream"] == Value::Bool(true),
        };
        assert_eq!(pretty(&line, &look), vector["pretty"].as_str().unwrap(), "{name}: pretty");
    }
}
