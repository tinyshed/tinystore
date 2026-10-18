//! A query: the series of a name, of labels under conditions, or of both,
//! over a range of time. `read` gives their samples, `aggregate` their
//! buckets, `latest` the newest sample of each, and `explain` what a read or
//! an aggregate would cost.
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

use super::aggregate::{Aggregate, Asking, Op, Shape};
use super::engine::Engine;
use super::explain::Plan;
use super::labels::{MOST_LABELS, MOST_NAME_BYTES, MOST_VALUE_BYTES, NAME};
use super::latest::Latest;
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

/// A query being asked: its name, conditions and range, and for an
/// aggregate its buckets and groups.
#[derive(Clone)]
pub struct Query {
    pub(super) engine: Arc<Engine>,
    name: Option<String>,
    conditions: Vec<(String, Condition)>,
    since: Option<Duration>,
    from: Option<SystemTime>,
    to: Option<SystemTime>,
    every: Option<Duration>,
    by: Option<Vec<String>>,
    without: Option<Vec<String>>,
    estimate: bool,
}

impl fmt::Debug for Query {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Query")
            .field("name", &self.name)
            .field("conditions", &self.conditions)
            .field("since", &self.since)
            .field("from", &self.from)
            .field("to", &self.to)
            .field("every", &self.every)
            .field("by", &self.by)
            .field("without", &self.without)
            .field("estimate", &self.estimate)
            .finish()
    }
}

/// A query checked: its conditions on labels, the name among them as
/// `__name__`, and its range in unix milliseconds, `[from, to)`.
#[derive(Clone, Debug)]
pub(super) struct Asked {
    /// The name it asked for, which a timer's query reads as the series the
    /// timer writes.
    pub(super) name: Option<String>,
    pub(super) conditions: Vec<(String, Condition)>,
    pub(super) from: i64,
    pub(super) to: i64,
    /// The store's clock, read once a query.
    pub(super) now: i64,
}

impl Query {
    pub(super) fn of(engine: Arc<Engine>) -> Query {
        Query {
            engine,
            name: None,
            conditions: Vec::new(),
            since: None,
            from: None,
            to: None,
            every: None,
            by: None,
            without: None,
            estimate: false,
        }
    }

    /// The series of this name.
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

    /// An aggregate's buckets: spans this long, each beginning at a whole
    /// number of them since 1970 in UTC, the range cutting the first and the
    /// last. Without it the range is one bucket.
    pub fn every(mut self, span: Duration) -> Self {
        self.every = Some(span);
        self
    }

    /// The labels an aggregate keeps: the series of a name that share them
    /// are one group. Without `by` and `without`, every series of a name is
    /// one group.
    pub fn by(mut self, labels: &[&str]) -> Self {
        self.by.get_or_insert_default().extend(labels.iter().map(|label| (*label).to_owned()));
        self
    }

    /// The labels an aggregate leaves out, keeping every other: `&[]` keeps
    /// every series apart.
    pub fn without(mut self, labels: &[&str]) -> Self {
        self.without.get_or_insert_default().extend(labels.iter().map(|label| (*label).to_owned()));
        self
    }

    /// A timer's percentile also gives a point between the bounds of the
    /// bucket of its spread that holds the rank, interpolated as Prometheus'
    /// `histogram_quantile` does: an estimate, which no measurement gave.
    pub fn estimate(mut self) -> Self {
        self.estimate = true;
        self
    }

    /// The samples of each series the query finds, in its range: a series
    /// without one is left out. Past a bound of a read it is a `limit`
    /// error, naming the bound, never fewer series or samples.
    pub fn read(&self) -> Result<Vec<Series>> {
        let read = self.plain("read").and_then(|()| self.asked()).and_then(|asked| self.engine.read(&asked));
        read.map_err(|error| error.within("metrics"))
    }

    /// The buckets of each group of the series the query finds, each one
    /// exact value of `op`, rounded once: a group without a sample in the
    /// range is left out, and so is a bucket without one. An `op` a series'
    /// kind has not is `invalid`, and so is a bucket over a NaN, an infinity
    /// or a counter below zero; past a bound it is a `limit` error, naming
    /// the bound, never fewer buckets.
    pub fn aggregate(&self, op: Op) -> Result<Vec<Aggregate>> {
        let aggregate = self.shape(op).and_then(|shape| self.engine.aggregate(&self.asked()?, &shape));
        aggregate.map_err(|error| error.within("metrics"))
    }

    /// The newest sample of each series the query finds, when it has one in
    /// the range: a series that stopped writing leaves the answer rather
    /// than show its last value for ever.
    pub fn latest(&self) -> Result<Vec<Latest>> {
        let latest = self.plain("latest").and_then(|()| self.asked()).and_then(|asked| self.engine.latest(&asked));
        latest.map_err(|error| error.within("metrics"))
    }

    /// What `read`, or `aggregate` when given its `op`, would spend of each
    /// bound of a query, and the bound it would stop at. It decodes no
    /// sample.
    ///
    /// ```no_run
    /// # use std::time::Duration;
    /// # fn f(stats: tinystore::metrics::Metrics) -> tinystore::Result<()> {
    /// use tinystore::metrics::Op;
    /// let month = stats.query().name("cpu").since(Duration::from_hours(30 * 24));
    /// let plan = month.clone().every(Duration::from_hours(1)).explain(Op::Avg)?;
    /// assert!(plan.exceeds.is_none(), "whole hours answer from their summaries");
    /// let read = month.explain(None)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn explain(&self, op: impl Into<Option<Op>>) -> Result<Plan> {
        let shape = match op.into() {
            Some(op) => self.shape(op).map(Some),
            None => self.plain("a read").map(|()| None),
        };
        let plan = shape.and_then(|shape| self.engine.explain(&self.asked()?, shape.as_ref()));
        plan.map_err(|error| error.within("metrics"))
    }

    /// An aggregate's buckets and groups, checked.
    fn shape(&self, op: Op) -> Result<Shape> {
        let asking = Asking {
            every: self.every,
            by: self.by.as_deref(),
            without: self.without.as_deref(),
            name: self.name.as_deref(),
            estimate: self.estimate,
        };
        Shape::of(op, asking)
    }

    /// Refuses what only an aggregate takes, for a call that is none.
    fn plain(&self, call: &str) -> Result<()> {
        if self.every.is_some() || self.by.is_some() || self.without.is_some() || self.estimate {
            return Err(Error::invalid(format!(
                "`every`, `by`, `without` and `estimate` are an aggregate's: {call} takes none"
            )));
        }
        Ok(())
    }

    /// The query checked against what a read may ask, at the store's time.
    pub(super) fn asked(&self) -> Result<Asked> {
        let now = self.engine.now();
        let from = match (self.since, self.from) {
            (Some(_), Some(_)) => {
                return Err(Error::invalid("a query starts `since` before now or `from` a time, not both"));
            }
            (Some(span), None) => now.saturating_sub(crate::clock::millis(span)),
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
                "a query finds its series by a name, or a label's value, list or prefix: `Not` alone would read every \
                 series",
            ));
        }
        if conditions.len() > MOST_LABELS {
            return Err(Error::invalid(format!("a query of more than {MOST_LABELS} conditions")));
        }
        Ok(Asked { name: self.name.clone(), conditions, from, to, now })
    }
}

/// A condition as a query keeps it: its values sorted and each once; one on
/// a label of the store's own, of no values or of too many is refused.
fn check(label: &str, condition: &Condition) -> Result<Condition> {
    let condition_of = || format!("label {label:?}");
    if super::labels::is_own(label) {
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
