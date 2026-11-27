//! What a read's `where` asks of a key's value, by the JSON a value writes:
//!
//! ```text
//! filter("op", &"drafts.get")              the value written as "drafts.get"
//! filter("user", &[7, 12, 31])             one of 7, 12 and 31
//! filter("tags", &[["a", "b"]])            the list ["a","b"] itself
//! filter("env", Not("test"))               anything but "test", or no env at all
//! filter("route", Prefix("/admin"))        a text that starts with /admin
//! ```
//!
//! A value matches as records compare values, by the JSON it is written as:
//! `42` finds `42`, and neither `"42"` nor `42.0`. A key an event holds in its
//! fields and in its context matches by either.
//!
//! A statement of a database, `filter("user", db.statement(sql!("select id
//! from teachers")))`, is a list read when the query starts, on a reader of
//! its database: its column's values become the list's, each as `json_of`
//! writes it.

use std::collections::HashSet;
use std::sync::Arc;

use rusqlite::types::Value as SqlValue;
use serde::Serialize;
use serde_json::value::RawValue;

use super::event::Fields;
use super::values::integer;
use crate::limits::{FILTER_ROWS, WHERE_VALUE_BYTES};
use crate::memory::{Memory, PATIENCE};
use crate::{Error, Not, Prefix, Reservation, Result};

/// The values a list of a condition holds at most, as in metrics.
const MOST_VALUES: usize = 1000;
/// What a value of a list, or a prefix, weighs at most, as a metrics label's.
const MOST_VALUE_BYTES: usize = WHERE_VALUE_BYTES.bound as usize;
/// The rows a statement of a `where` gives at most, as a table's index's.
const MOST_ROWS: usize = FILTER_ROWS.bound as usize;
/// What a set of a statement's values holds of the store's memory for each
/// value, beside its bytes, which it keeps twice: as JSON and inside quotes.
const VALUE_HELD: u64 = 96;

/// What a key's value must be for a read to find an event: one of some JSON
/// values, none of them, or a text that starts with a prefix. A value, a
/// [`Not`], a [`Prefix`] or a statement of a database converts into one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Condition(std::result::Result<Matching, String>);

/// A condition taken: a list's values in a set, made once, to look a value
/// up by its hash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Matching {
    OneOf(Arc<Among>),
    /// None of them, or the key absent.
    NoneOf(Arc<Among>),
    Prefix(String),
    /// A statement's column, read into `OneOf`, or `NoneOf` when it is
    /// `not`, as the query starts.
    #[cfg_attr(not(feature = "sql"), expect(dead_code, reason = "only a statement of a database makes one"))]
    Rows {
        rows: Rows,
        not: bool,
    },
}

/// How a statement's one column is read, on a reader of its database, which
/// lends it: `most` values and one more at most, so that the read can say its
/// bound was passed.
pub(crate) type ReadColumn = Arc<dyn Fn(usize) -> Result<Vec<SqlValue>> + Send + Sync>;

/// A statement whose one column's values a condition holds: how an error
/// names it, and how its values are read.
#[derive(Clone)]
pub(super) struct Rows {
    described: String,
    read: ReadColumn,
}

/// A statement's condition equals itself and its clones: what its closure
/// reads has no equality of its own.
impl PartialEq for Rows {
    fn eq(&self, other: &Rows) -> bool {
        self.described == other.described && Arc::ptr_eq(&self.read, &other.read)
    }
}

impl Eq for Rows {}

impl std::fmt::Debug for Rows {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.described)
    }
}

/// The values a list or a statement names, each as every place a read looks
/// one up holds it: an event's JSON, and a block's column, which may keep
/// its texts without their quotes and its integers as numbers.
#[derive(Debug, Default)]
pub(super) struct Among {
    jsons: HashSet<Box<[u8]>>,
    /// What is inside the quotes of each value that has them, escapes as
    /// they are.
    inners: HashSet<Box<[u8]>>,
    /// The JSON integers, spelled as an integer spells itself, and those
    /// spelled so inside quotes.
    integers: HashSet<i64>,
    quoted_integers: HashSet<i64>,
    /// The store's memory a statement's values hold while the query lasts.
    held: Held,
}

/// Two sets are equal by their values, which the other sets are made from.
impl PartialEq for Among {
    fn eq(&self, other: &Among) -> bool {
        self.jsons == other.jsons
    }
}

impl Eq for Among {}

/// What a set of values holds of the store's memory: nothing for a list a
/// program wrote, whose values it holds already.
#[derive(Default)]
struct Held(Option<Reservation>);

impl std::fmt::Debug for Held {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} bytes held", self.0.as_ref().map_or(0, Reservation::bytes))
    }
}

impl Condition {
    /// The value written as `json`, itself, a list or an object as well: a
    /// `where` of a client older than conditions.
    #[cfg(feature = "wire")]
    pub(crate) fn exactly(json: String) -> Condition {
        Condition(Ok(Matching::OneOf(Among::of(vec![json]))))
    }

    /// One of these values, each the JSON it is written as; none of them with
    /// `not`, the key absent too.
    #[cfg(feature = "wire")]
    pub(crate) fn listed(values: Vec<String>, not: bool) -> Condition {
        let values = Among::of(values);
        Condition(Ok(if not { Matching::NoneOf(values) } else { Matching::OneOf(values) }))
    }

    /// The values of the one column of the statement `described`, which
    /// `read` reads when the query starts; none of them with `not`.
    #[cfg(feature = "sql")]
    pub(crate) fn rows(described: String, read: ReadColumn, not: bool) -> Condition {
        Condition(Ok(Matching::Rows { rows: Rows { described, read }, not }))
    }

    #[cfg(feature = "wire")]
    pub(crate) fn prefix(text: String) -> Condition {
        Condition(Ok(Matching::Prefix(text)))
    }

    /// The condition within its bounds, or why it is refused, naming `key`.
    pub(super) fn checked(self, key: &str) -> std::result::Result<Matching, String> {
        let refused = |why: String| format!("where {key:?}: {why}");
        match self.0.map_err(refused)? {
            Matching::OneOf(values) => listed(&values).map(|()| Matching::OneOf(values)).map_err(refused),
            Matching::NoneOf(values) => listed(&values).map(|()| Matching::NoneOf(values)).map_err(refused),
            Matching::Prefix(text) if text.is_empty() || text.len() > MOST_VALUE_BYTES => {
                Err(refused(format!("a prefix of 1 to {MOST_VALUE_BYTES} bytes")))
            }
            matching @ (Matching::Prefix(_) | Matching::Rows { .. }) => Ok(matching),
        }
    }
}

/// Refuses a list past its bounds.
fn listed(values: &Among) -> std::result::Result<(), String> {
    if values.jsons.is_empty() || values.jsons.len() > MOST_VALUES {
        let why = "a list or an object as the value itself goes in one, [[…]]";
        return Err(format!("a list of 1 to {MOST_VALUES} values: {why}"));
    }
    if values.jsons.iter().any(|value| value.len() > MOST_VALUE_BYTES) {
        return Err(format!("a value of {MOST_VALUE_BYTES} bytes at most"));
    }
    Ok(())
}

impl Rows {
    /// The statement's values, read now, as the values of a list: `not`
    /// makes them none of them. Past its bound of rows, or with a value past
    /// a list's 4 KiB, it is `limit`; a blob, or a real JSON cannot write,
    /// `invalid`. What the values hold while the query lasts is taken from
    /// the store's `memory`, and past it is `limit` too.
    pub(super) fn read(&self, key: &str, not: bool, memory: &Arc<Memory>) -> Result<Matching> {
        let within = |error: Error| error.within(format!("where {key:?}: {}", self.described));
        let values = (self.read)(MOST_ROWS).map_err(within)?;
        if values.len() > MOST_ROWS {
            let refused = format!("where {key:?}: {} gives more than {MOST_ROWS} rows", self.described);
            return Err(FILTER_ROWS.refuse(values.len() as u64, refused));
        }
        let jsons = values.into_iter().filter_map(|value| json_of(value).transpose());
        let jsons = jsons.collect::<std::result::Result<Vec<String>, String>>();
        let mut jsons = jsons.map_err(|why| within(Error::invalid(why)))?;
        jsons.extend(booleans(&jsons));
        if let Some(long) = jsons.iter().find(|json| json.len() > MOST_VALUE_BYTES) {
            let why = format!("a value of {} bytes, past a list's {MOST_VALUE_BYTES}", long.len());
            return Err(within(WHERE_VALUE_BYTES.refuse(long.len() as u64, why)));
        }
        let bytes = jsons.iter().map(|json| 2 * json.len() as u64 + VALUE_HELD).sum();
        let held = memory.reserve_for_read(bytes, "records: a where's statement", PATIENCE).map_err(within)?;
        let mut values = Among::of(jsons);
        if let Some(among) = Arc::get_mut(&mut values) {
            among.held = Held(Some(held));
        }
        Ok(if not { Matching::NoneOf(values) } else { Matching::OneOf(values) })
    }
}

/// The JSON an event's field holds when it equals a statement's value: an
/// integer as itself, `7`; a text as a JSON string, `'7'` as `"7"` and never
/// `7`; a real as JSON writes it, `7.0` and `0.5`, so that `7.0` finds no
/// `7`, and 1 and 0 find `true` and `false` too, as `booleans` says. A
/// null matches nothing; a blob is no JSON value and is refused, as a real
/// past what JSON writes is.
fn json_of(value: SqlValue) -> std::result::Result<Option<String>, String> {
    match value {
        SqlValue::Null => Ok(None),
        SqlValue::Integer(integer) => Ok(Some(integer.to_string())),
        SqlValue::Real(real) if real.is_finite() => Ok(Some(serde_json::Value::from(real).to_string())),
        SqlValue::Real(real) => Err(format!("a real of {real}, which no JSON number is")),
        SqlValue::Text(text) => Ok(Some(serde_json::Value::String(text).to_string())),
        SqlValue::Blob(_) => Err("a blob, which no JSON value is: select it as text, hex(…) or a number".to_owned()),
    }
}

/// The booleans a statement's 1s and 0s stand for too: SQLite keeps a
/// boolean column's `true` as 1, so that `select done from tasks` finds an
/// event's `true`, as a table's index finds its rows' 1 by `true`.
fn booleans(jsons: &[String]) -> Vec<String> {
    let boolean = |json: &String| match json.as_str() {
        "1" => Some("true".to_owned()),
        "0" => Some("false".to_owned()),
        _ => None,
    };
    jsons.iter().filter_map(boolean).collect()
}

/// The values a JSON text stands for: a list's items, or the value alone; an
/// object is refused, since a `where` of a client that writes `{ not }` or
/// `{ prefix }` as one would otherwise match nothing.
fn values_of(json: serde_json::Result<String>) -> std::result::Result<Vec<String>, String> {
    let json = json.map_err(|error| format!("a value JSON cannot write: {error}"))?;
    match json.as_bytes().first() {
        Some(b'[') => {
            let items: Vec<&RawValue> = serde_json::from_str(&json).map_err(|error| format!("a list: {error}"))?;
            Ok(items.into_iter().map(|item| item.get().to_owned()).collect())
        }
        Some(b'{') => {
            let why = "an object as the value itself goes in a list, [{…}]";
            Err(format!("an object, where a value, a list, Not or Prefix goes: {why}"))
        }
        _ => Ok(vec![json]),
    }
}

impl Among {
    pub(super) fn of(values: Vec<String>) -> Arc<Among> {
        let mut among = Among::default();
        for json in values {
            if let Some(inner) = json.strip_prefix('"').and_then(|json| json.strip_suffix('"')) {
                among.quoted_integers.extend(integer(inner));
                among.inners.insert(inner.as_bytes().into());
            }
            among.integers.extend(integer(&json));
            among.jsons.insert(json.into_bytes().into_boxed_slice());
        }
        Arc::new(among)
    }

    /// Whether `json` is one of the values.
    pub(super) fn holds(&self, json: &str) -> bool {
        self.jsons.contains(json.as_bytes())
    }

    /// Each value's JSON.
    pub(super) fn jsons(&self) -> impl Iterator<Item = &[u8]> {
        self.jsons.iter().map(|json| &**json)
    }

    pub(super) fn len(&self) -> usize {
        self.jsons.len()
    }

    /// The values as a column keeps its texts: inside their quotes when it
    /// keeps them `quoted`, where a JSON without quotes is no value of it.
    pub(super) fn texts(&self, quoted: bool) -> &HashSet<Box<[u8]>> {
        if quoted { &self.inners } else { &self.jsons }
    }

    /// The values as a column keeps its integers, `quoted` as `texts` says.
    pub(super) fn integers(&self, quoted: bool) -> &HashSet<i64> {
        if quoted { &self.quoted_integers } else { &self.integers }
    }
}

impl<T: Serialize + ?Sized> From<&T> for Condition {
    /// A value by the JSON it writes: a list is one of its items.
    fn from(value: &T) -> Condition {
        Condition(values_of(serde_json::to_string(value)).map(|values| Matching::OneOf(Among::of(values))))
    }
}

impl<T: Serialize> From<Not<T>> for Condition {
    fn from(Not(value): Not<T>) -> Condition {
        Condition(values_of(serde_json::to_string(&value)).map(|values| Matching::NoneOf(Among::of(values))))
    }
}

impl<T: Into<String>> From<Prefix<T>> for Condition {
    fn from(Prefix(text): Prefix<T>) -> Condition {
        Condition(Ok(Matching::Prefix(text.into())))
    }
}

impl Matching {
    /// Whether an event whose fields and context are these meets the
    /// condition at `key`: by the last value of a key written twice.
    pub(super) fn finds(&self, key: &str, fields: &Fields, context: &Fields) -> bool {
        let (field, held) = (fields.json(key), context.json(key));
        match self {
            Matching::OneOf(_) | Matching::Prefix(_) => self.holds(field) || self.holds(held),
            Matching::NoneOf(_) => !self.holds(field) && !self.holds(held),
            // read before any event is: a query that starts reads its statements
            Matching::Rows { .. } => false,
        }
    }

    /// Whether one value, written as `json`, is one the condition names: in
    /// its list, or a text that starts with its prefix.
    pub(super) fn holds(&self, json: Option<&str>) -> bool {
        let Some(json) = json else { return false };
        match self {
            Matching::OneOf(values) | Matching::NoneOf(values) => values.holds(json),
            Matching::Prefix(prefix) => {
                json.starts_with('"') && serde_json::from_str::<String>(json).is_ok_and(|text| text.starts_with(prefix))
            }
            Matching::Rows { .. } => false,
        }
    }

    /// The values a block may be passed by, when its filter holds none of
    /// them: a list's, of a condition that only an event holding one meets.
    pub(super) fn one_of(&self) -> Option<&Among> {
        match self {
            Matching::OneOf(values) => Some(values),
            Matching::NoneOf(_) | Matching::Prefix(_) | Matching::Rows { .. } => None,
        }
    }

    /// Whether only an event holding the key meets the condition, so that a
    /// segment without the key holds none: all but `Not`, which an event
    /// without the key meets.
    pub(super) fn needs_key(&self) -> bool {
        !matches!(self, Matching::NoneOf(_) | Matching::Rows { not: true, .. })
    }
}

#[cfg(all(test, feature = "sql"))]
#[path = "condition_tests.rs"]
mod tests;
