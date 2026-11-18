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

use serde::Serialize;
use serde_json::value::RawValue;

use super::event::Fields;
use crate::{Not, Prefix};

/// The values a list of a condition holds at most, as in metrics.
const MOST_VALUES: usize = 1000;
/// What a value of a list, or a prefix, weighs at most, as a metrics label's.
const MOST_VALUE_BYTES: usize = 4 << 10;

/// What a key's value must be for a read to find an event: one of some JSON
/// values, none of them, or a text that starts with a prefix. A value, a
/// [`Not`] or a [`Prefix`] converts into one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Condition(Result<Matching, String>);

/// A condition taken: each list sorted, to look a value up by halves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Matching {
    OneOf(Vec<String>),
    /// None of them, or the key absent.
    NoneOf(Vec<String>),
    Prefix(String),
}

impl Condition {
    /// The value written as `json`, itself, a list or an object as well: a
    /// `where` of a client older than conditions.
    #[cfg(feature = "wire")]
    pub(crate) fn exactly(json: String) -> Condition {
        Condition(Ok(Matching::OneOf(vec![json])))
    }

    /// One of these values, each the JSON it is written as; none of them with
    /// `not`, the key absent too.
    #[cfg(feature = "wire")]
    pub(crate) fn listed(values: Vec<String>, not: bool) -> Condition {
        Condition(Ok(if not { Matching::NoneOf(values) } else { Matching::OneOf(values) }))
    }

    #[cfg(feature = "wire")]
    pub(crate) fn prefix(text: String) -> Condition {
        Condition(Ok(Matching::Prefix(text)))
    }

    /// The condition within its bounds, or why it is refused, naming `key`.
    pub(super) fn checked(self, key: &str) -> Result<Matching, String> {
        let refused = |why: String| format!("where {key:?}: {why}");
        match self.0.map_err(refused)? {
            Matching::OneOf(values) => Ok(Matching::OneOf(sorted(values).map_err(refused)?)),
            Matching::NoneOf(values) => Ok(Matching::NoneOf(sorted(values).map_err(refused)?)),
            Matching::Prefix(text) if text.is_empty() || text.len() > MOST_VALUE_BYTES => {
                Err(refused(format!("a prefix of 1 to {MOST_VALUE_BYTES} bytes")))
            }
            Matching::Prefix(text) => Ok(Matching::Prefix(text)),
        }
    }
}

/// A list within its bounds, sorted and each value once.
fn sorted(mut values: Vec<String>) -> Result<Vec<String>, String> {
    if values.is_empty() || values.len() > MOST_VALUES {
        let why = "a list or an object as the value itself goes in one, [[…]]";
        return Err(format!("a list of 1 to {MOST_VALUES} values: {why}"));
    }
    if values.iter().any(|value| value.len() > MOST_VALUE_BYTES) {
        return Err(format!("a value of {MOST_VALUE_BYTES} bytes at most"));
    }
    values.sort_unstable();
    values.dedup();
    Ok(values)
}

/// The values a JSON text stands for: a list's items, or the value alone; an
/// object is refused, since a `where` of a client that writes `{ not }` or
/// `{ prefix }` as one would otherwise match nothing.
fn values_of(json: serde_json::Result<String>) -> Result<Vec<String>, String> {
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

/// Whether `json` is one of `values`, which are sorted.
pub(super) fn among(values: &[String], json: &str) -> bool {
    values.binary_search_by(|value| value.as_str().cmp(json)).is_ok()
}

impl<T: Serialize + ?Sized> From<&T> for Condition {
    /// A value by the JSON it writes: a list is one of its items.
    fn from(value: &T) -> Condition {
        Condition(values_of(serde_json::to_string(value)).map(Matching::OneOf))
    }
}

impl<T: Serialize> From<Not<T>> for Condition {
    fn from(Not(value): Not<T>) -> Condition {
        Condition(values_of(serde_json::to_string(&value)).map(Matching::NoneOf))
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
            Matching::OneOf(_) => self.holds(field) || self.holds(held),
            Matching::NoneOf(_) => !self.holds(field) && !self.holds(held),
            Matching::Prefix(_) => self.holds(field) || self.holds(held),
        }
    }

    /// Whether one value, written as `json`, is one the condition names: in
    /// its list, or a text that starts with its prefix.
    pub(super) fn holds(&self, json: Option<&str>) -> bool {
        let Some(json) = json else { return false };
        match self {
            Matching::OneOf(values) | Matching::NoneOf(values) => among(values, json),
            Matching::Prefix(prefix) => {
                json.starts_with('"') && serde_json::from_str::<String>(json).is_ok_and(|text| text.starts_with(prefix))
            }
        }
    }

    /// The values a block may be passed by, when its filter holds none of
    /// them: a list's, of a condition that only an event holding one meets.
    pub(super) fn one_of(&self) -> Option<&[String]> {
        match self {
            Matching::OneOf(values) => Some(values),
            Matching::NoneOf(_) | Matching::Prefix(_) => None,
        }
    }

    /// Whether only an event holding the key meets the condition, so that a
    /// segment without the key holds none: all but `Not`, which an event
    /// without the key meets.
    pub(super) fn needs_key(&self) -> bool {
        !matches!(self, Matching::NoneOf(_))
    }
}
