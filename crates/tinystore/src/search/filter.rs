//! Filters: a document's values of each filter field, kept as keys of the
//! filter's part beside its words, and the documents a search's conditions
//! pass. [`eq`], [`is_in`], [`or`] and [`and`] make conditions as `sql::eq`,
//! `sql::is_in`, `sql::or` and `sql::and` make a query's, [`range`] one of
//! numbers between two bounds, and [`missing`] one of no value.
//!
//! A value is a key's term by its type, so that the text "42" and the
//! number 42 never pass for each other, and a document with no value, no
//! such field, null or an empty list, holds the term of none, which
//! [`missing`] asks for:
//!
//! ```text
//! "shoes" → t:shoes     42, 42.0 → i:42     true → b:true     none → n:
//! ```
//!
//! A number is also kept at eight precisions, as Lucene's numeric fields
//! were: its place among all numbers as a u64 that sorts as they do, and that
//! place with its last 8, 16, … 56 bits dropped, `r0:` to `r7:`. A range is
//! then whole blocks of numbers at the coarsest precision that fits and
//! single numbers only at its edges: up to 255 keys on each side at each of
//! the seven finer precisions and 255 at the coarsest, 3,825 at most and a
//! few hundred for most ranges, which the bound on a filter's values leaves
//! out.
//!
//! A key's postings are a word's, packed and purged with them, a frequency of
//! 1 each, and the key counts the documents that hold it. A search reads every
//! posting of the keys its conditions name, the head's and the blocks', its
//! documents gone left out, and joins them: `or` and a list's values by their
//! union, `and` and a search's conditions by their intersection.

use std::collections::HashSet;
use std::ops::{Bound, RangeBounds};

use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use serde_json::Value;

use super::pack::{Stored, read_list};
use super::postings::{unpack, unpack_inline};
use super::score::Part;
use super::settings::Settings;
use crate::sqlite::sql_error;
use crate::{Error, Result};

const FIND_KEY: &str = "select id from _tinystore_search_keys where index_id = ?1 and part = ?2 and term = ?3";
const READ_PAYLOADS: &str = "select payload from _tinystore_search_blocks where key = ?1 order by first";
const READ_HEAD: &str = "select doc from _tinystore_search_head where index_id = ?1 and key = ?2 order by doc";

/// The longest text a filter keeps as a value.
pub(crate) const MOST_VALUE: usize = 256;
/// The conditions a search holds at most, `or` and `and` counting those
/// inside them.
pub(crate) const MOST_CONDITIONS: usize = 8;
/// The values the lists of a search's conditions hold at most.
pub(crate) const MOST_VALUES: usize = 100;
/// The term of no value.
const NONE: &str = "n:";
/// A number's precisions, each 8 bits coarser than the one before.
const LEVELS: u32 = 8;
const STEP: u32 = 8;

/// A condition of a search, given to [`SearchCall::filter`](super::SearchCall::filter).
#[derive(Clone, Debug)]
pub struct Condition(Node);

#[derive(Clone, Debug)]
enum Node {
    /// One of a filter's values, or one of a list of them.
    Values {
        path: String,
        values: std::result::Result<Value, String>,
        list: bool,
    },
    /// No value.
    Missing {
        path: String,
    },
    /// A number between two bounds.
    Range {
        path: String,
        low: Bound<Value>,
        high: Bound<Value>,
    },
    Or(Vec<Condition>),
    And(Vec<Condition>),
}

/// A filter equal to `value`: a document passes when it holds it, or one of
/// its list is it. Values match exactly, case and type: the text "50" is not
/// the number 50, and 42.0 is 42. `None` is `invalid`, so that an absent id
/// never asks for documents without one; [`missing`] asks for those.
pub fn eq<T: Serialize + ?Sized>(path: &str, value: &T) -> Condition {
    let values = serde_json::to_value(value).map_err(|error| error.to_string());
    Condition(Node::Values { path: path.to_owned(), values, list: false })
}

/// A filter holding one of `values`, as SQL's `in (…)`; an empty list passes
/// nothing.
pub fn is_in<T: Serialize>(path: &str, values: &[T]) -> Condition {
    let values = serde_json::to_value(values).map_err(|error| error.to_string());
    Condition(Node::Values { path: path.to_owned(), values, list: true })
}

/// A filter holding a number within `bounds`, any of Rust's ranges:
/// `range("price", 10..100)` holds 10 and not 100, `range("at", since..)`.
/// A document with no number there never passes. Numbers compare as f64,
/// so whole numbers past 2⁵³ compare by the nearest f64, where `eq` is
/// exact.
pub fn range<T: Serialize>(path: &str, bounds: impl RangeBounds<T>) -> Condition {
    let value = |bound: Bound<&T>| match bound {
        Bound::Included(value) => Bound::Included(serde_json::to_value(value).unwrap_or(Value::Null)),
        Bound::Excluded(value) => Bound::Excluded(serde_json::to_value(value).unwrap_or(Value::Null)),
        Bound::Unbounded => Bound::Unbounded,
    };
    Condition(Node::Range { path: path.to_owned(), low: value(bounds.start_bound()), high: value(bounds.end_bound()) })
}

/// A filter with no value: no such field, null or an empty list.
pub fn missing(path: &str) -> Condition {
    Condition(Node::Missing { path: path.to_owned() })
}

/// Any of `conditions`, in parentheses; an empty `or` passes nothing.
pub fn or(conditions: impl IntoIterator<Item = Condition>) -> Condition {
    Condition(Node::Or(conditions.into_iter().collect()))
}

/// Every one of `conditions`, for a group inside an `or`; an empty `and`
/// passes everything.
pub fn and(conditions: impl IntoIterator<Item = Condition>) -> Condition {
    Condition(Node::And(conditions.into_iter().collect()))
}

/// The conditions a search's `where` writes over the wire, each one of
///
/// ```text
/// {"eq": [path, value]}   {"in": [path, [values]]}   {"missing": path}
/// {"range": [path, {"gt" | "gte" | "lt" | "lte": number, …}]}
/// {"or": [conditions]}    {"and": [conditions]}
/// ```
///
/// a list of them joined by and, as the Bun SDK writes its `where` object.
#[cfg(feature = "wire")]
pub(crate) fn from_json(text: &str) -> Result<Vec<Condition>> {
    let value: Value = serde_json::from_str(text).map_err(|error| Error::invalid(format!("a where: {error}")))?;
    match value {
        Value::Array(items) => items.iter().map(condition_of).collect(),
        one => Ok(vec![condition_of(&one)?]),
    }
}

#[cfg(feature = "wire")]
fn condition_of(value: &Value) -> Result<Condition> {
    let refused = || Error::invalid(format!("a where's condition {value}"));
    let Some((kind, given)) = value.as_object().filter(|object| object.len() == 1).and_then(|o| o.iter().next()) else {
        return Err(refused());
    };
    let path_and = |given: &Value| -> Result<(String, Value)> {
        match given.as_array().map(Vec::as_slice) {
            Some([Value::String(path), operand]) => Ok((path.clone(), operand.clone())),
            _ => Err(refused()),
        }
    };
    match kind.as_str() {
        "eq" => path_and(given).map(|(path, operand)| eq(&path, &operand)),
        "in" => match path_and(given)? {
            (path, Value::Array(values)) => Ok(is_in(&path, &values)),
            _ => Err(refused()),
        },
        "missing" => given.as_str().map(missing).ok_or_else(refused),
        "range" => {
            let (path, bounds) = path_and(given)?;
            let bound = |strict: &str, loose: &str| match (bounds.get(strict), bounds.get(loose)) {
                (Some(value), _) => Bound::Excluded(value.clone()),
                (None, Some(value)) => Bound::Included(value.clone()),
                (None, None) => Bound::Unbounded,
            };
            Ok(Condition(Node::Range { path, low: bound("gt", "gte"), high: bound("lt", "lte") }))
        }
        "or" | "and" => {
            let parts: Vec<Condition> =
                given.as_array().ok_or_else(refused)?.iter().map(condition_of).collect::<Result<_>>()?;
            Ok(if kind == "or" { or(parts) } else { and(parts) })
        }
        _ => Err(refused()),
    }
}

/// A condition as a search reads it: the keys of a filter one of which a
/// document holds, or its parts joined.
#[derive(Clone, Debug)]
pub(crate) enum Asked {
    Terms { path: String, terms: Vec<String> },
    Or(Vec<Asked>),
    And(Vec<Asked>),
}

/// How many conditions and values a search's filter holds.
#[derive(Default)]
struct Counted {
    conditions: usize,
    values: usize,
}

/// The search's conditions as it reads them, within their bounds.
pub(crate) fn asked(conditions: &[Condition]) -> Result<Asked> {
    let mut counted = Counted::default();
    let asked = conditions.iter().map(|condition| condition.asked(&mut counted)).collect::<Result<Vec<_>>>()?;
    if counted.conditions > MOST_CONDITIONS || counted.values > MOST_VALUES {
        return Err(Error::limit(format!(
            "a filter of {} conditions and {} values, past {MOST_CONDITIONS} and {MOST_VALUES}",
            counted.conditions, counted.values
        )));
    }
    Ok(Asked::And(asked))
}

impl Condition {
    fn asked(&self, counted: &mut Counted) -> Result<Asked> {
        match &self.0 {
            Node::Values { path, values, list } => {
                counted.conditions += 1;
                let values = values.as_ref().map_err(|why| Error::invalid(format!("filter {path}: {why}")))?;
                let terms = match (values, list) {
                    (Value::Array(_), false) => {
                        return Err(Error::invalid(format!("filter {path}: eq takes one value, and is_in a list")));
                    }
                    (Value::Array(items), true) => items.iter().map(|item| term(item, path)).collect::<Result<_>>()?,
                    (value, _) => vec![term(value, path)?],
                };
                counted.values += terms.len();
                Ok(Asked::Terms { path: path.clone(), terms })
            }
            Node::Range { path, low, high } => {
                counted.conditions += 1;
                Ok(Asked::Terms { path: path.clone(), terms: range_terms(path, low, high)? })
            }
            Node::Missing { path } => {
                counted.conditions += 1;
                Ok(Asked::Terms { path: path.clone(), terms: vec![NONE.to_owned()] })
            }
            Node::Or(conditions) => Ok(Asked::Or(conditions.iter().map(|c| c.asked(counted)).collect::<Result<_>>()?)),
            Node::And(conditions) => {
                Ok(Asked::And(conditions.iter().map(|c| c.asked(counted)).collect::<Result<_>>()?))
            }
        }
    }
}

/// The terms a document holds at `path`: its value's, or each of its list's,
/// and the term of none where it holds none.
pub(crate) fn held(document: &Value, path: &str) -> Result<Vec<String>> {
    let values: Vec<&Value> = match at(document, path) {
        Value::Array(items) => items.iter().filter(|item| !item.is_null()).collect(),
        Value::Null => Vec::new(),
        value => vec![value],
    };
    let mut terms = Vec::with_capacity(values.len() * (1 + LEVELS as usize));
    for value in values {
        terms.push(term(value, path)?);
        if let Some(number) = value.as_f64() {
            terms.extend((0..LEVELS).map(|level| precise(level, sortable(number) >> (STEP * level))));
        }
    }
    if terms.is_empty() {
        terms.push(NONE.to_owned());
    }
    terms.sort_unstable();
    terms.dedup();
    Ok(terms)
}

/// A value's term: text, a whole number or a boolean; a number with a
/// fraction is its place at the finest precision, so that an equal one
/// passes.
fn term(value: &Value, path: &str) -> Result<String> {
    let refused =
        |what: &str| Error::invalid(format!("filter {path} holds {what}: a filter keeps text, numbers and booleans"));
    match value {
        Value::Null => Err(Error::invalid(format!(
            "filter {path}: a value of None or null; missing({path:?}) asks for a document with none"
        ))),
        Value::String(text) if text.len() > MOST_VALUE => {
            Err(Error::limit(format!("filter {path} holds {} bytes of text, past {MOST_VALUE}", text.len())))
        }
        Value::String(text) => Ok(format!("t:{text}")),
        Value::Bool(on) => Ok(format!("b:{on}")),
        Value::Number(number) => match (number.as_i64(), number.as_u64(), number.as_f64()) {
            (Some(whole), _, _) => Ok(format!("i:{whole}")),
            (None, Some(whole), _) => Ok(format!("i:{whole}")),
            (None, None, Some(float)) if float.fract() == 0.0 && float.abs() < 9.2e18 => {
                Ok(format!("i:{}", float as i64))
            }
            (None, None, Some(float)) => Ok(precise(0, sortable(float))),
            _ => Err(refused(&format!("{number}"))),
        },
        Value::Array(_) => Err(refused("a list in a list")),
        Value::Object(_) => Err(refused("an object")),
    }
}

/// A number's place among all numbers, as a u64 that sorts as they do;
/// −0 is 0.
fn sortable(number: f64) -> u64 {
    let bits = if number == 0.0 { 0.0f64.to_bits() } else { number.to_bits() };
    if bits >> 63 == 1 { !bits } else { bits | 1 << 63 }
}

/// The term of a number's place at a precision, its last `STEP × level`
/// bits dropped.
fn precise(level: u32, prefix: u64) -> String {
    format!("r{level}:{prefix:x}")
}

/// The terms whose numbers together are those within a range's bounds.
fn range_terms(path: &str, low: &Bound<Value>, high: &Bound<Value>) -> Result<Vec<String>> {
    let place = |value: &Value| match value.as_f64() {
        Some(number) if number.is_finite() => Ok(sortable(number)),
        _ => Err(Error::invalid(format!("filter {path}: a range is of numbers, not {value}"))),
    };
    let low = match low {
        Bound::Included(value) => Some(place(value)?),
        Bound::Excluded(value) => place(value)?.checked_add(1),
        Bound::Unbounded => Some(0),
    };
    let high = match high {
        Bound::Included(value) => Some(place(value)?),
        Bound::Excluded(value) => place(value)?.checked_sub(1),
        Bound::Unbounded => Some(u64::MAX),
    };
    let (Some(low), Some(high)) = (low, high) else { return Ok(Vec::new()) };
    Ok(blocks(low, high).into_iter().map(|(level, prefix)| precise(level, prefix)).collect())
}

/// The fewest prefixes whose places together are `low..=high`: at each
/// precision, the places from the edges to the next coarser block's bounds,
/// and the blocks between at the coarser one.
fn blocks(low: u64, high: u64) -> Vec<(u32, u64)> {
    let (mut low, mut high) = (u128::from(low), u128::from(high) + 1);
    let mut blocks = Vec::new();
    let mut take = |level: u32, from: u128, to: u128| {
        let shift = STEP * level;
        blocks.extend(((from >> shift)..(to >> shift)).map(|prefix| (level, prefix as u64)));
    };
    for level in 0..LEVELS {
        let coarser = 1u128 << (STEP * (level + 1));
        let (up, down) = (low.div_ceil(coarser) * coarser, high / coarser * coarser);
        if low >= high {
            break;
        }
        if level == LEVELS - 1 || up >= down {
            take(level, low, high);
            break;
        }
        take(level, low, up);
        take(level, down, high);
        (low, high) = (up, down);
    }
    blocks
}

/// The value at `path` in a document, null where it has none.
fn at<'v>(document: &'v Value, path: &str) -> &'v Value {
    let mut at = document;
    for name in path.split('.') {
        match at.get(name) {
            Some(value) => at = value,
            None => return &Value::Null,
        }
    }
    at
}

/// The documents a filter passes: every one, or these, in their order.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Docs {
    All,
    Only(Vec<u32>),
}

/// What a search reads its conditions' documents from.
pub(crate) struct Reader<'a> {
    pub(crate) connection: &'a Connection,
    pub(crate) index: i64,
    pub(crate) settings: &'a Settings,
    pub(crate) gone: &'a HashSet<u32>,
}

impl Reader<'_> {
    /// The documents `asked` passes.
    pub(crate) fn passing(&self, asked: &Asked) -> Result<Docs> {
        match asked {
            Asked::Terms { path, terms } => self.holding(path, terms).map(Docs::Only),
            Asked::Or(parts) => {
                let mut passing = Docs::Only(Vec::new());
                for part in parts {
                    passing = match (passing, self.passing(part)?) {
                        (Docs::All, _) | (_, Docs::All) => return Ok(Docs::All),
                        (Docs::Only(a), Docs::Only(b)) => Docs::Only(union(&a, &b)),
                    };
                }
                Ok(passing)
            }
            Asked::And(parts) => {
                let mut passing = Docs::All;
                for part in parts {
                    passing = match (passing, self.passing(part)?) {
                        (Docs::All, other) | (other, Docs::All) => other,
                        (Docs::Only(a), Docs::Only(b)) => Docs::Only(intersection(&a, &b)),
                    };
                    if passing == Docs::Only(Vec::new()) {
                        break;
                    }
                }
                Ok(passing)
            }
        }
    }

    /// The documents that hold any of `terms` of the filter at `path`.
    fn holding(&self, path: &str, terms: &[String]) -> Result<Vec<u32>> {
        let Some(filter) = self.settings.filter(path) else {
            let paths = self.settings.filters.iter().map(|filter| filter.path.as_str());
            let meant = meant(path, paths).map(|meant| format!("; did you mean {meant}?")).unwrap_or_default();
            return Err(Error::invalid(format!("a filter on {path}, which the index was not opened with{meant}")));
        };
        let mut holding = Vec::new();
        for term in terms {
            if let Some(key) = self.find(Part::filter(filter.number), self.as_kept(term))? {
                holding = union(&holding, &self.docs_of(key)?);
            }
        }
        Ok(holding)
    }

    /// A term as the index keeps it: a table's rows hold a boolean as SQLite
    /// does, the integer 1 or 0, so that `eq("done", &true)` passes them.
    fn as_kept<'t>(&self, term: &'t str) -> &'t str {
        match term {
            "b:true" if self.settings.table.is_some() => "i:1",
            "b:false" if self.settings.table.is_some() => "i:0",
            term => term,
        }
    }

    fn find(&self, part: Part, term: &str) -> Result<Option<i64>> {
        let failed = |error| sql_error("find a filter's value", error);
        let mut select = self.connection.prepare_cached(FIND_KEY).map_err(failed)?;
        select.query_row((self.index, part.0, term), |row| row.get(0)).optional().map_err(failed)
    }

    /// Every document a key's postings hold, those gone left out, in order.
    fn docs_of(&self, key: i64) -> Result<Vec<u32>> {
        let within = |error: Error| error.within(format!("key {key}"));
        let failed = |error| sql_error(format!("read key {key}'s postings"), error);
        let mut docs: Vec<u32> = Vec::new();
        match read_list(self.connection, key).map_err(within)? {
            Stored::Blocks(_) => {
                let mut select = self.connection.prepare_cached(READ_PAYLOADS).map_err(failed)?;
                let mut rows = select.query([key]).map_err(failed)?;
                while let Some(row) = rows.next().map_err(failed)? {
                    let payload = row.get_ref(0).and_then(|value| Ok(value.as_blob()?)).map_err(failed)?;
                    docs.extend(unpack(payload).map_err(within)?.into_iter().map(|(doc, _)| doc));
                }
            }
            Stored::Inline(inline) => {
                docs.extend(unpack_inline(&inline).map_err(within)?.iter().map(|posting| posting.doc));
            }
            Stored::Nothing => {}
        }
        let mut select = self.connection.prepare_cached(READ_HEAD).map_err(failed)?;
        let head = select.query_map((self.index, key), |row| row.get::<_, u32>(0));
        docs.extend(head.and_then(Iterator::collect::<rusqlite::Result<Vec<u32>>>).map_err(failed)?);
        if !docs.is_sorted_by(|a, b| a < b) {
            return Err(Error::corrupt(format!("key {key}'s documents are out of their order")));
        }
        docs.retain(|doc| !self.gone.contains(doc));
        Ok(docs)
    }
}

/// The filter a person may have meant by `given`, among `paths`: one edit
/// away for a name of three to six letters, two for a longer one, as
/// config's keys are suggested. It is only said.
fn meant<'p>(given: &str, paths: impl IntoIterator<Item = &'p str>) -> Option<String> {
    let edits = match given.chars().count() {
        0..3 => 0,
        3..7 => 1,
        _ => 2,
    };
    crate::typos::Dictionary::new(paths).nearest(given, edits).map(|near| near.word)
}

/// The documents of either list, each once, in order.
fn union(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut joined = Vec::with_capacity(a.len() + b.len());
    let (mut i, mut j) = (0, 0);
    loop {
        let next = match (a.get(i), b.get(j)) {
            (Some(x), Some(y)) => *x.min(y),
            (Some(x), None) => *x,
            (None, Some(y)) => *y,
            (None, None) => return joined,
        };
        joined.push(next);
        i += usize::from(a.get(i) == Some(&next));
        j += usize::from(b.get(j) == Some(&next));
    }
}

/// The documents of both lists, in order.
fn intersection(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut both = Vec::with_capacity(a.len().min(b.len()));
    let (mut i, mut j) = (0, 0);
    while let (Some(x), Some(y)) = (a.get(i), b.get(j)) {
        match x.cmp(y) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                both.push(*x);
                (i, j) = (i + 1, j + 1);
            }
        }
    }
    both
}

#[cfg(test)]
#[path = "filter_tests.rs"]
mod tests;
