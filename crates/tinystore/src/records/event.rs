//! An event as a program writes it and as a read gives it back.

use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::console::object_pairs;
use crate::{Error, Result};

/// The keys an event's fields hold at most, and its context's.
pub(super) const MAX_KEYS: usize = 128;
/// The bytes one event weighs at most.
pub(super) const MAX_EVENT: usize = 256 << 10;

/// How grave an event is: one of six names, or a number between two of them
/// as Go's `slog` counts, which reads as the name under it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Level(i32);

impl Level {
    pub const TRACE: Level = Level(-8);
    pub const DEBUG: Level = Level(-4);
    pub const INFO: Level = Level(0);
    pub const WARN: Level = Level(4);
    pub const ERROR: Level = Level(8);
    pub const FATAL: Level = Level(12);

    /// The level of `slog`'s number: -4 debug, 0 info, 4 warn, 8 error.
    pub fn of(number: i32) -> Level {
        Level(number)
    }

    pub fn number(self) -> i32 {
        self.0
    }

    pub fn name(self) -> &'static str {
        ["trace", "debug", "info", "warn", "error", "fatal"][self.step()]
    }

    /// Which of the six names the level stands at or over.
    fn step(self) -> usize {
        (self.0.div_euclid(4) + 2).clamp(0, 5) as usize
    }

    /// The bit a row's `levels` has for an event of this level.
    pub(super) fn bit(self) -> i64 {
        1 << self.step()
    }

    /// The bits of every level a row may hold that is this one or worse.
    pub(super) fn bits_from(self) -> i64 {
        0b11_1111 & !(self.bit() - 1)
    }
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A trace's id: sixteen bytes, written as 32 hex digits.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct TraceId(pub [u8; 16]);

/// A span's id: eight bytes, written as 16 hex digits.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SpanId(pub [u8; 8]);

fn hex(bytes: &[u8], f: &mut fmt::Formatter<'_>) -> fmt::Result {
    bytes.iter().try_for_each(|byte| write!(f, "{byte:02x}"))
}

impl fmt::Display for TraceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        hex(&self.0, f)
    }
}

impl fmt::Debug for TraceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        hex(&self.0, f)
    }
}

impl fmt::Display for SpanId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        hex(&self.0, f)
    }
}

impl fmt::Debug for SpanId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        hex(&self.0, f)
    }
}

/// An event's fields, or its context: keys in the order they were written,
/// each value the JSON it was written as, so that `1.2300`, an integer past
/// 2⁵³ and a key written twice come back as they went in.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Fields(Vec<(String, String)>);

impl Fields {
    /// The fields of a struct or a map, in the order it writes them; `()`
    /// is none.
    pub fn of(value: &impl Serialize) -> Result<Fields> {
        let object = serde_json::to_string(value)
            .map_err(|error| Error::invalid("fields JSON cannot write").with_source(error))?;
        Fields::read(&object)
    }

    /// The fields of a JSON object's text; `null` is none.
    pub fn read(object: &str) -> Result<Fields> {
        let pairs = object_pairs(object);
        pairs.map(Fields).map_err(|error| Error::invalid("fields that are no JSON object").with_source(error))
    }

    pub(super) fn from_pairs(pairs: Vec<(String, String)>) -> Fields {
        Fields(pairs)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Each key with the JSON of its value, in the order they were written.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().map(|(key, json)| (key.as_str(), json.as_str()))
    }

    /// The JSON a key's value was written as; the last, of a key written
    /// twice.
    pub fn json(&self, key: &str) -> Option<&str> {
        self.0.iter().rev().find(|(name, _)| name == key).map(|(_, json)| json.as_str())
    }

    /// A key's value, read as `T`; `None` without the key.
    pub fn get<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        let Some(json) = self.json(key) else { return Ok(None) };
        let read = serde_json::from_str(json);
        read.map(Some).map_err(|error| Error::invalid(format!("field {key:?} does not read so")).with_source(error))
    }

    /// The fields read as a struct or a map.
    pub fn to<T: DeserializeOwned>(&self) -> Result<T> {
        let mut object = String::from("{");
        for (index, (key, json)) in self.iter().enumerate() {
            if index > 0 {
                object.push(',');
            }
            object.push_str(&serde_json::to_string(key).unwrap_or_default());
            object.push(':');
            object.push_str(json);
        }
        object.push('}');
        serde_json::from_str(&object).map_err(|error| Error::invalid("fields do not read so").with_source(error))
    }

    /// What the fields weigh in an event: more than their row spends on them.
    pub(super) fn weight(&self) -> usize {
        self.0.iter().map(|(key, json)| key.len() + json.len() + 20).sum::<usize>() + 10
    }
}

/// An event as a read gives it.
#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    /// Its time, which the file keeps to the nanosecond.
    pub at: SystemTime,
    pub stream: String,
    /// What happened; `log` for a logger's line.
    pub name: String,
    pub level: Option<Level>,
    pub message: Option<String>,
    /// What the call gave.
    pub fields: Fields,
    /// What it happened in: a logger's `with`, an append's `context`.
    pub context: Fields,
    pub trace_id: Option<TraceId>,
    pub span_id: Option<SpanId>,
}

/// An event as the file keeps it: its time in nanoseconds since 1970, and no
/// stream, which is its row's.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Kept {
    pub(super) at: i64,
    pub(super) name: String,
    pub(super) level: Option<Level>,
    pub(super) message: Option<String>,
    pub(super) fields: Fields,
    pub(super) context: Fields,
    pub(super) trace_id: Option<TraceId>,
    pub(super) span_id: Option<SpanId>,
}

impl Kept {
    /// What the event weighs against the bounds of an append, a row and a
    /// segment: more than its bytes in any of them.
    pub(super) fn weight(&self) -> usize {
        let message = self.message.as_ref().map_or(0, String::len);
        64 + self.name.len() + message + self.fields.weight() + self.context.weight()
    }

    pub(super) fn into_event(self, stream: &str) -> Event {
        Event {
            at: UNIX_EPOCH + Duration::from_nanos(self.at.max(0) as u64),
            stream: stream.to_owned(),
            name: self.name,
            level: self.level,
            message: self.message,
            fields: self.fields,
            context: self.context,
            trace_id: self.trace_id,
            span_id: self.span_id,
        }
    }
}

/// An event to write: its name, and what it carries beside its fields.
#[derive(Clone, Debug)]
pub struct NewEvent {
    pub(super) name: String,
    pub(super) at: Option<SystemTime>,
    pub(super) level: Option<Level>,
    pub(super) message: Option<String>,
    pub(super) fields: Fields,
    pub(super) context: Fields,
    pub(super) trace_id: Option<TraceId>,
    pub(super) span_id: Option<SpanId>,
    /// Why its fields or its context could not be taken, for the append that
    /// refuses it.
    pub(super) refused: Option<String>,
}

impl NewEvent {
    pub fn new(name: &str) -> NewEvent {
        NewEvent {
            name: name.to_owned(),
            at: None,
            level: None,
            message: None,
            fields: Fields::default(),
            context: Fields::default(),
            trace_id: None,
            span_id: None,
            refused: None,
        }
    }

    /// Its time: when it is appended, unless given.
    pub fn at(mut self, at: SystemTime) -> Self {
        self.at = Some(at);
        self
    }

    pub fn level(mut self, level: Level) -> Self {
        self.level = Some(level);
        self
    }

    pub fn message(mut self, message: &str) -> Self {
        self.message = Some(message.to_owned());
        self
    }

    /// What happened: a struct or a map, kept as the JSON it writes.
    pub fn fields(mut self, fields: &impl Serialize) -> Self {
        match Fields::of(fields) {
            Ok(fields) => self.fields = fields,
            Err(error) => self.refused = Some(error.to_string()),
        }
        self
    }

    /// What it happened in: who asked, which request.
    pub fn context(mut self, context: &impl Serialize) -> Self {
        match Fields::of(context) {
            Ok(context) => self.context = context,
            Err(error) => self.refused = Some(error.to_string()),
        }
        self
    }

    pub fn trace(mut self, trace_id: TraceId, span_id: SpanId) -> Self {
        (self.trace_id, self.span_id) = (Some(trace_id), Some(span_id));
        self
    }

    /// The event as the file keeps it, at `at`.
    pub(super) fn kept(self, at: i64) -> Kept {
        Kept {
            at,
            name: self.name,
            level: self.level,
            message: self.message,
            fields: self.fields,
            context: self.context,
            trace_id: self.trace_id,
            span_id: self.span_id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_keep_their_order_and_the_spelling_of_each_value() {
        let fields = Fields::read(r#"{"b": 1.2300, "a": 9007199254740993, "b": {"x": [1, "y"]}}"#).unwrap();
        let read: Vec<(&str, &str)> = fields.iter().collect();
        assert_eq!(read, [("b", "1.2300"), ("a", "9007199254740993"), ("b", r#"{"x": [1, "y"]}"#)]);
        assert_eq!(fields.json("b"), Some(r#"{"x": [1, "y"]}"#));
        assert_eq!(fields.get::<u64>("a").unwrap(), Some(9_007_199_254_740_993));
        assert_eq!(fields.get::<u64>("none").unwrap(), None);
    }

    #[test]
    fn fields_are_an_object_or_none() {
        assert!(Fields::of(&()).unwrap().is_empty());
        assert_eq!(Fields::of(&serde_json::json!({ "user": 42 })).unwrap().json("user"), Some("42"));
        for refused in ["[1]", "42", r#""text""#, "{", r#"{"a": 1} 2"#] {
            assert_eq!(Fields::read(refused).unwrap_err().kind(), crate::ErrorKind::Invalid, "{refused}");
        }
    }

    #[test]
    fn a_level_between_two_names_reads_as_the_one_under_it() {
        assert_eq!(Level::of(2).name(), "info");
        assert_eq!(Level::of(-100).name(), "trace");
        assert_eq!(Level::of(100).name(), "fatal");
        assert!(Level::WARN < Level::ERROR);
        assert_eq!(Level::WARN.bits_from(), Level::WARN.bit() | Level::ERROR.bit() | Level::FATAL.bit());
    }
}
