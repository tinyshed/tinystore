//! A query: the series of a name, of labels under conditions, or of both,
//! over a range of time; `read` gives their samples.
//!
//! ```no_run
//! # use std::time::Duration;
//! # fn f(stats: tinystore::metrics::Metrics) -> tinystore::Result<()> {
//! use tinystore::metrics::{Not, Prefix};
//! let failing = stats
//!     .query()
//!     .name("http_requests_total")
//!     .since(Duration::from_hours(1))
//!     .filter("status", ["500", "502"])
//!     .filter("env", Not("dev"))
//!     .filter("host", Prefix("api-"))
//!     .read()?;
//! # Ok(())
//! # }
//! ```

use std::fmt;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use super::engine::Engine;
use super::labels::{MOST_LABELS, MOST_NAME_BYTES, MOST_VALUE_BYTES, NAME};
use super::series::Series;
use crate::{Error, Result, unix_millis};

/// The values a list of a condition holds at most.
const MOST_VALUES: usize = 1000;

/// What a label's value must be for a query to take a series.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Condition {
    /// One of these values.
    OneOf(Vec<String>),
    /// None of these values, or no such label at all.
    NoneOf(Vec<String>),
    /// A value that starts with this text.
    Prefix(String),
}

/// A label equal to none of these values, or absent: `Not("dev")`,
/// `Not(["dev", "test"])`.
#[derive(Clone, Copy, Debug)]
pub struct Not<T>(pub T);

/// A label that starts with this text: `Prefix("api-")`.
#[derive(Clone, Copy, Debug)]
pub struct Prefix<T>(pub T);

/// The values a condition lists: text, or a number or a boolean as its text,
/// as a label keeps them.
pub trait Values {
    fn values(self) -> Vec<String>;
}

impl Values for &str {
    fn values(self) -> Vec<String> {
        vec![self.to_owned()]
    }
}

impl Values for String {
    fn values(self) -> Vec<String> {
        vec![self]
    }
}

impl Values for &String {
    fn values(self) -> Vec<String> {
        vec![self.clone()]
    }
}

impl<T: Values, const N: usize> Values for [T; N] {
    fn values(self) -> Vec<String> {
        self.into_iter().flat_map(Values::values).collect()
    }
}

impl<T: Values> Values for Vec<T> {
    fn values(self) -> Vec<String> {
        self.into_iter().flat_map(Values::values).collect()
    }
}

impl<T: Values + Clone> Values for &[T] {
    fn values(self) -> Vec<String> {
        self.iter().cloned().flat_map(Values::values).collect()
    }
}

macro_rules! values_as_text {
    ($($kind:ty),*) => {$(
        impl Values for $kind {
            fn values(self) -> Vec<String> {
                vec![self.to_string()]
            }
        }
    )*};
}
values_as_text!(i8, i16, i32, i64, u8, u16, u32, u64, usize, bool);

impl<T: Values> From<T> for Condition {
    fn from(values: T) -> Condition {
        Condition::OneOf(values.values())
    }
}

impl<T: Values> From<Not<T>> for Condition {
    fn from(Not(values): Not<T>) -> Condition {
        Condition::NoneOf(values.values())
    }
}

impl<T: Into<String>> From<Prefix<T>> for Condition {
    fn from(Prefix(prefix): Prefix<T>) -> Condition {
        Condition::Prefix(prefix.into())
    }
}

/// A query being asked: its name, conditions and range, which `read` runs.
#[derive(Clone)]
pub struct Query {
    pub(super) engine: Arc<Engine>,
    name: Option<String>,
    conditions: Vec<(String, Condition)>,
    since: Option<Duration>,
    from: Option<SystemTime>,
    to: Option<SystemTime>,
}

impl fmt::Debug for Query {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Query")
            .field("name", &self.name)
            .field("conditions", &self.conditions)
            .field("since", &self.since)
            .field("from", &self.from)
            .field("to", &self.to)
            .finish()
    }
}

/// A query checked: its conditions on labels, the name among them as
/// `__name__`, and its range in unix milliseconds, `[from, to)`.
#[derive(Clone, Debug)]
pub(super) struct Asked {
    pub(super) conditions: Vec<(String, Condition)>,
    pub(super) from: i64,
    pub(super) to: i64,
    /// The store's clock, read once a query.
    pub(super) now: i64,
}

impl Query {
    pub(super) fn of(engine: Arc<Engine>) -> Query {
        Query { engine, name: None, conditions: Vec::new(), since: None, from: None, to: None }
    }

    /// The series of this name; a timer's name, the series it writes.
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// A label's value under a condition: a value, a list of them, a
    /// [`Not`] or a [`Prefix`].
    pub fn filter(mut self, label: impl Into<String>, condition: impl Into<Condition>) -> Self {
        self.conditions.push((label.into(), condition.into()));
        self
    }

    /// The span before now the range covers; or `from` and `to`.
    pub fn since(mut self, span: Duration) -> Self {
        self.since = Some(span);
        self
    }

    /// The range's start, which is in it.
    pub fn from(mut self, at: SystemTime) -> Self {
        self.from = Some(at);
        self
    }

    /// The range's end, which is not in it; now unless given, a sample taken
    /// now in the range.
    pub fn to(mut self, at: SystemTime) -> Self {
        self.to = Some(at);
        self
    }

    /// The samples of each series the query finds, in its range: a series
    /// without one is left out. Past a bound of a read it is a `limit`
    /// error, naming the bound, never fewer series or samples.
    pub fn read(&self) -> Result<Vec<Series>> {
        let asked = self.asked().map_err(|error| error.within("metrics"))?;
        self.engine.read(&asked).map_err(|error| error.within("metrics"))
    }

    /// The query checked against what a read may ask, at the store's time.
    pub(super) fn asked(&self) -> Result<Asked> {
        let now = self.engine.now();
        let from = match (self.since, self.from) {
            (Some(_), Some(_)) => {
                return Err(Error::invalid("a query starts `since` before now or `from` a time, not both"));
            }
            (Some(span), None) => now.saturating_sub(i64::try_from(span.as_millis()).unwrap_or(i64::MAX)),
            (None, Some(from)) => unix_millis(from),
            (None, None) => i64::MIN,
        };
        // a range that starts after now and says no end is empty, not inverted
        let to = self.to.map_or(now.saturating_add(1).max(from), unix_millis);
        if to < from {
            return Err(Error::invalid("a query whose range ends before it starts"));
        }
        let mut conditions = Vec::with_capacity(self.conditions.len() + 1);
        if let Some(name) = &self.name {
            conditions.push((NAME.to_owned(), Condition::OneOf(vec![name.clone()])));
        }
        for (label, condition) in &self.conditions {
            conditions.push((label.clone(), check(label, condition)?));
        }
        if !conditions.iter().any(|(_, condition)| !matches!(condition, Condition::NoneOf(_))) {
            return Err(Error::invalid(
                "a query finds its series by a name, or a label's value, list or prefix: `Not` alone would read every series",
            ));
        }
        if conditions.len() > MOST_LABELS {
            return Err(Error::invalid(format!("a query of more than {MOST_LABELS} conditions")));
        }
        Ok(Asked { conditions, from, to, now })
    }
}

/// A condition as a query keeps it: its values sorted and each once; one on
/// a label of the store's own, of no values or of too many is refused.
fn check(label: &str, condition: &Condition) -> Result<Condition> {
    let condition_of = || format!("label {label:?}");
    if label.starts_with("__") {
        return Err(Error::invalid(format!("{}: a name beginning with __ is the store's own", condition_of())));
    }
    if label.is_empty() || label.len() > MOST_NAME_BYTES {
        return Err(Error::invalid(format!("{}: a name of 1 to {MOST_NAME_BYTES} bytes", condition_of())));
    }
    let checked = |values: &Vec<String>| -> Result<Vec<String>> {
        if values.is_empty() || values.len() > MOST_VALUES {
            return Err(Error::invalid(format!("{}: a condition lists 1 to {MOST_VALUES} values", condition_of())));
        }
        if values.iter().any(|value| value.len() > MOST_VALUE_BYTES) {
            return Err(Error::invalid(format!("{}: a value of {MOST_VALUE_BYTES} bytes at most", condition_of())));
        }
        let mut values = values.clone();
        values.sort_unstable();
        values.dedup();
        Ok(values)
    };
    Ok(match condition {
        Condition::OneOf(values) => Condition::OneOf(checked(values)?),
        Condition::NoneOf(values) => Condition::NoneOf(checked(values)?),
        Condition::Prefix(prefix) if prefix.is_empty() || prefix.len() > MOST_VALUE_BYTES => {
            return Err(Error::invalid(format!("{}: a prefix of 1 to {MOST_VALUE_BYTES} bytes", condition_of())));
        }
        Condition::Prefix(prefix) => Condition::Prefix(prefix.clone()),
    })
}

#[cfg(test)]
#[path = "query_tests.rs"]
mod tests;
