//! Counts: how many events a read finds, the most frequent values of a key,
//! and rows of counts and of what fields' numbers add up to, a row a span of
//! time and a value of the keys asked; and what a store holds, its streams.
//!
//! ```text
//! api.list().since(DAY).count()                                   → 42
//! api.list().since(HOUR).top("route")                             → "/notes" 12, "/login" 7, …
//! api.list().since(DAY).aggregate(&Aggregate::new().every(HOUR).by(["level"]))
//!                                                                 → 13:00 error 3, 13:00 warn 41, …
//! ```
//!
//! Each walks every event the read finds, a page at a time, so that what it
//! holds is its answer and never the events.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::params;
use serde_json::Value;

use super::engine::{Records, nanos};
use super::event::Event;
use super::events::AllEvents;
use super::read::List;
use crate::sqlite::sql_error;
use crate::{Error, Result, unix_nanos};

/// The values `top` gives unless its read's limit says, and at most.
const TOP: usize = 10;
const MOST_TOP: usize = 1000;
/// The rows `aggregate` gives at most.
const MOST_ROWS: usize = 10_000;
/// What the different values a count holds may weigh at most: of the key
/// `top` counts, or of every field `distinct` names in every row, each its
/// JSON and what its map spends on it.
const MOST_HELD: usize = 64 << 20;
const HELD_EACH: usize = 64;

/// A value of a key and how many events hold it.
#[derive(Clone, Debug, PartialEq)]
pub struct Top {
    pub value: Value,
    pub count: u64,
}

/// A number a field holds: an integer exactly, however large a sum of them
/// grows, or a float.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Number {
    Integer(i128),
    Float(f64),
}

impl Number {
    pub fn as_f64(self) -> f64 {
        match self {
            Number::Integer(integer) => integer as f64,
            Number::Float(float) => float,
        }
    }
}

/// What a row says of the numbers a field holds; a field with no number in
/// a row has none there.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stats {
    /// The row's events whose value there is a number, which `avg` divides
    /// `sum` by.
    pub samples: u64,
    pub sum: Number,
    pub min: Number,
    pub max: Number,
    pub avg: f64,
}

/// A row of `aggregate`: a span of time and a value of each key `by` named.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    /// Where the span begins, counted from 1970 in UTC.
    pub at: SystemTime,
    pub count: u64,
    /// Each key `by` named, as it was written, with its value; null for
    /// events without it.
    pub by: BTreeMap<String, Value>,
    pub stats: BTreeMap<String, Stats>,
    /// How many different values each field `distinct` named has.
    pub distinct: BTreeMap<String, u64>,
}

/// What `aggregate` makes rows of: spans of `every`, the values of the keys
/// in `by`, and of the fields in `stats` and `distinct`.
#[derive(Clone, Debug, Default)]
pub struct Aggregate {
    every: Option<Duration>,
    by: Vec<String>,
    stats: Vec<String>,
    distinct: Vec<String>,
}

impl Aggregate {
    pub fn new() -> Aggregate {
        Aggregate::default()
    }

    /// A row a span this long, each beginning at a whole number of them
    /// since 1970; the whole range is one span without it.
    pub fn every(mut self, span: Duration) -> Self {
        self.every = Some(span);
        self
    }

    /// A row a value of each key: a field's, or `stream`, `name` or `level`,
    /// which are the event's own before a field of that name.
    pub fn by<S: Into<String>>(mut self, keys: impl IntoIterator<Item = S>) -> Self {
        self.by.extend(keys.into_iter().map(Into::into));
        self
    }

    /// The count, sum, least, most and mean of each field's numbers.
    pub fn stats<S: Into<String>>(mut self, keys: impl IntoIterator<Item = S>) -> Self {
        self.stats.extend(keys.into_iter().map(Into::into));
        self
    }

    /// How many different values each field has.
    pub fn distinct<S: Into<String>>(mut self, keys: impl IntoIterator<Item = S>) -> Self {
        self.distinct.extend(keys.into_iter().map(Into::into));
        self
    }
}

/// What a store holds of a stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamInfo {
    pub name: String,
    /// How long it keeps its events.
    pub keep: Duration,
    /// The times of its oldest and newest event it keeps.
    pub oldest: SystemTime,
    pub newest: SystemTime,
}

impl List {
    /// How many events the read finds.
    pub fn count(&self) -> Result<u64> {
        let mut found = 0;
        for event in self.walk("count")? {
            event?;
            found += 1;
        }
        Ok(found)
    }

    /// The most frequent values of `key` and how many events hold each, the
    /// most first and those as frequent in the order of their JSON: ten
    /// unless the read's limit says, 1000 at most. The key is a field's or a
    /// context's, or `stream`, `name` or `level`.
    pub fn top(&self, key: &str) -> Result<Vec<Top>> {
        let limit = self.given_limit().unwrap_or(TOP);
        if !(1..=MOST_TOP).contains(&limit) {
            return Err(Error::invalid(format!("top {limit} values: it gives 1 to {MOST_TOP}")));
        }
        let (mut counts, mut held): (HashMap<String, Top>, Held) = (HashMap::new(), Held::default());
        for event in self.walk("top")? {
            let Some(value) = value_of(&event?, key) else { continue };
            let json = value.to_string();
            held.add(&json, !counts.contains_key(&json)).map_err(|error| error.within(key))?;
            counts.entry(json).or_insert(Top { value, count: 0 }).count += 1;
        }
        let mut top: Vec<Top> = counts.into_values().collect();
        top.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.value.to_string().cmp(&b.value.to_string())));
        top.truncate(limit);
        Ok(top)
    }

    /// A row for each span and each value of the keys `by` names, in the
    /// order of their spans: an answer whole, or `limit` past 10,000 rows.
    pub fn aggregate(&self, aggregate: &Aggregate) -> Result<Vec<Row>> {
        let every = match aggregate.every {
            Some(span) if span.is_zero() => return Err(Error::invalid("every: a span longer than none")),
            span => span.map(nanos),
        };
        let (first, _) = self.span()?;
        let mut rows = Rows { asked: aggregate, rows: HashMap::new(), held: Held::default() };
        for event in self.walk("aggregate")? {
            let event = event?;
            let at = every.map_or(first, |every| unix_nanos(event.at).div_euclid(every) * every);
            rows.add(at, &event)?;
        }
        Ok(rows.finish())
    }
}

impl AllEvents {
    /// Every stream with events, by name: what it keeps, and the times of
    /// its oldest and newest event it keeps.
    pub fn streams(&self) -> Result<Vec<StreamInfo>> {
        self.records.streams_held()
    }
}

const TIMES_HELD: &str = "select min(first_at), max(last_at) from
    (select first_at, last_at from heads where stream = ?1 and last_at >= ?2
     union all select first_at, last_at from segments where stream = ?1 and holder is null and last_at >= ?2)";

impl Records {
    fn streams_held(&self) -> Result<Vec<StreamInfo>> {
        let now = self.now();
        let mut held = Vec::new();
        for stream in self.every_stream() {
            let cutoff = now.saturating_sub(nanos(stream.keeps()));
            if let Some((oldest, newest)) = self.times_held(stream.id, cutoff)? {
                let time = |at: i64| UNIX_EPOCH + Duration::from_nanos(at.max(0) as u64);
                let info = StreamInfo {
                    name: stream.name.to_string(),
                    keep: stream.keeps(),
                    oldest: time(oldest.max(cutoff)),
                    newest: time(newest),
                };
                held.push(info);
            }
        }
        held.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(held)
    }

    /// The times of a stream's oldest and newest rows that reach past
    /// `cutoff`; none when it has none.
    fn times_held(&self, stream: i64, cutoff: i64) -> Result<Option<(i64, i64)>> {
        let failed = |error| sql_error("read the times a stream holds", error);
        self.file.read(|connection| {
            let held = connection.prepare_cached(TIMES_HELD).and_then(|mut select| {
                select.query_row(params![stream, cutoff], |row| Ok((row.get::<_, Option<i64>>(0)?, row.get(1)?)))
            });
            let (oldest, newest) = held.map_err(failed)?;
            Ok(oldest.zip(newest))
        })
    }
}

/// What the different values a count holds weigh, and the most they may.
struct Held {
    bytes: usize,
    most: usize,
}

impl Default for Held {
    fn default() -> Held {
        Held { bytes: 0, most: MOST_HELD }
    }
}

impl Held {
    /// Counts a value's JSON in when it is a new one: past the most a count
    /// may hold, `limit`.
    fn add(&mut self, json: &str, new: bool) -> Result<()> {
        self.bytes += if new { json.len() + HELD_EACH } else { 0 };
        if self.bytes > self.most {
            return Err(Error::limit(format!("past the {} bytes of different values one count holds", self.most)));
        }
        Ok(())
    }
}

/// The value of a key an event holds: its stream's name, its name or its
/// level's, or a field's, else its context's.
fn value_of(event: &Event, key: &str) -> Option<Value> {
    match key {
        "stream" => Some(Value::String(event.stream.clone())),
        "name" => Some(Value::String(event.name.clone())),
        "level" => event.level.map(|level| Value::String(level.name().to_owned())),
        key => {
            event.fields.json(key).or_else(|| event.context.json(key)).and_then(|json| serde_json::from_str(json).ok())
        }
    }
}

/// The number a value is, an integer while it is one.
fn number(value: &Value) -> Option<Number> {
    let Value::Number(number) = value else { return None };
    match (number.as_i64(), number.as_u64()) {
        (Some(integer), _) => Some(Number::Integer(i128::from(integer))),
        (_, Some(integer)) => Some(Number::Integer(i128::from(integer))),
        _ => number.as_f64().map(Number::Float),
    }
}

/// The rows of an aggregate as they fill.
struct Rows<'a> {
    asked: &'a Aggregate,
    /// By the start of a row's span and its keys' values as JSON.
    rows: HashMap<(i64, String), Filling>,
    held: Held,
}

/// A row being filled.
struct Filling {
    by: Vec<Value>,
    count: u64,
    stats: Vec<Summing>,
    distinct: Vec<HashSet<String>>,
}

/// A field's numbers in a row, summed as they come.
#[derive(Default)]
struct Summing {
    samples: u64,
    integers: i128,
    floats: f64,
    float: bool,
    least: Option<Number>,
    most: Option<Number>,
}

impl Rows<'_> {
    fn add(&mut self, at: i64, event: &Event) -> Result<()> {
        let by: Vec<Value> = self.asked.by.iter().map(|key| value_of(event, key).unwrap_or(Value::Null)).collect();
        let key = (at, Value::Array(by.clone()).to_string());
        if !self.rows.contains_key(&key) && self.rows.len() == MOST_ROWS {
            return Err(Error::limit(format!(
                "past the {MOST_ROWS} rows an aggregate gives: ask a shorter range or a wider every"
            )));
        }
        let asked = self.asked;
        let row = self.rows.entry(key).or_insert_with(|| Filling {
            by,
            count: 0,
            stats: asked.stats.iter().map(|_| Summing::default()).collect(),
            distinct: asked.distinct.iter().map(|_| HashSet::new()).collect(),
        });
        row.count += 1;
        for (summing, key) in row.stats.iter_mut().zip(&asked.stats) {
            if let Some(number) = value_of(event, key).as_ref().and_then(number) {
                summing.add(number).map_err(|error| error.within(key.as_str()))?;
            }
        }
        for (values, key) in row.distinct.iter_mut().zip(&asked.distinct) {
            if let Some(value) = value_of(event, key) {
                let json = value.to_string();
                self.held.add(&json, !values.contains(&json)).map_err(|error| error.within(key.as_str()))?;
                values.insert(json);
            }
        }
        Ok(())
    }

    fn finish(self) -> Vec<Row> {
        let asked = self.asked;
        let mut rows: Vec<((i64, String), Filling)> = self.rows.into_iter().collect();
        rows.sort_by(|(a, _), (b, _)| a.cmp(b));
        let row = |((at, _), filling): ((i64, String), Filling)| Row {
            at: UNIX_EPOCH + Duration::from_nanos(at.max(0) as u64),
            count: filling.count,
            by: asked.by.iter().cloned().zip(filling.by).collect(),
            stats: asked
                .stats
                .iter()
                .cloned()
                .zip(filling.stats)
                .filter_map(|(key, summing)| Some((key, summing.stats()?)))
                .collect(),
            distinct: asked
                .distinct
                .iter()
                .cloned()
                .zip(filling.distinct.iter().map(|values| values.len() as u64))
                .collect(),
        };
        rows.into_iter().map(row).collect()
    }
}

impl Summing {
    /// Adds a number; a sum of integers past what 128 bits hold is `limit`,
    /// never one less exact.
    fn add(&mut self, number: Number) -> Result<()> {
        self.samples += 1;
        match number {
            Number::Integer(integer) => {
                let sum = self.integers.checked_add(integer);
                self.integers = sum.ok_or_else(|| Error::limit("a sum of integers past what 128 bits hold"))?;
            }
            Number::Float(float) => (self.floats, self.float) = (self.floats + float, true),
        }
        let less = |a: Number, b: Number| match (a, b) {
            (Number::Integer(a), Number::Integer(b)) => a < b,
            (a, b) => a.as_f64() < b.as_f64(),
        };
        if self.least.is_none_or(|least| less(number, least)) {
            self.least = Some(number);
        }
        if self.most.is_none_or(|most| less(most, number)) {
            self.most = Some(number);
        }
        Ok(())
    }

    /// What the numbers add up to; none when there were none.
    fn stats(self) -> Option<Stats> {
        let (min, max) = (self.least?, self.most?);
        let sum =
            if self.float { Number::Float(self.integers as f64 + self.floats) } else { Number::Integer(self.integers) };
        Some(Stats { samples: self.samples, sum, min, max, avg: sum.as_f64() / self.samples as f64 })
    }
}

#[cfg(test)]
#[path = "count_tests.rs"]
mod tests;
