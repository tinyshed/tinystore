//! A timer's name: a timer writes no series under it, and a query of it
//! reads the series the timer does write, `_count`, `_sum`, `_max` and
//! `_bucket`, those the call needs.
//!
//! ```text
//! http_request_ms   avg   →  http_request_ms_count, http_request_ms_sum
//!                   p95   →  http_request_ms_count, http_request_ms_bucket
//!                   read  →  all four
//! ```
//!
//! A name is a timer's when it has no series of its own and its `_count` has
//! some: a histogram a scrape brought reads the same way, without `_max`.

use rusqlite::{Connection, params};

use super::aggregate::Op;
use super::labels::NAME;
use super::query::{Asked, Condition};
use super::snapshot::Want;
use crate::sqlite::sql_error;
use crate::{Error, Result};

/// What a series is to a query: one of the name it asked, or a part of the
/// timer it named.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Part {
    Own,
    /// How many durations: a counter.
    Count,
    /// Their sum: a counter.
    Sum,
    /// The longest since the write before: a gauge.
    Longest,
    /// The spread: a histogram's buckets.
    Buckets,
}

/// The parts of a timer, and the end of the name of each.
const PARTS: [(Part, &str); 4] =
    [(Part::Count, "_count"), (Part::Sum, "_sum"), (Part::Longest, "_max"), (Part::Buckets, "_bucket")];

/// The names of `?1` and of the series a timer of it writes, among those
/// with series.
const HELD: &str = "select name from names where kind is not null
    and name in (?1, ?1 || '_count', ?1 || '_sum', ?1 || '_max', ?1 || '_bucket')";

impl Part {
    /// What the series named `series` is to a query of `name`.
    pub(super) fn of(series: &str, name: Option<&str>) -> Part {
        let Some(end) = name.and_then(|name| series.strip_prefix(name)) else { return Part::Own };
        PARTS.iter().find(|(_, written)| *written == end).map_or(Part::Own, |(part, _)| *part)
    }

    fn name(self, timer: &str) -> String {
        let end = PARTS.iter().find(|(part, _)| *part == self).map_or("", |(_, end)| end);
        format!("{timer}{end}")
    }
}

/// The query read as the series of the timer it names and the call needs,
/// when it names one; none when its name has series of its own or names no
/// timer.
pub(super) fn read_as(connection: &Connection, asked: &Asked, want: Want<'_>) -> Result<Option<Asked>> {
    let Some(timer) = &asked.name else { return Ok(None) };
    let held = held(connection, timer)?;
    if held.contains(timer) || !held.contains(&Part::Count.name(timer)) {
        return Ok(None);
    }
    let parts = match want {
        Want::Buckets(shape) => wanted(shape.op(), timer, &held)?,
        Want::Samples | Want::Newest => PARTS.iter().map(|(part, _)| *part).collect(),
    };
    let mut names: Vec<String> = parts.iter().map(|part| part.name(timer)).filter(|name| held.contains(name)).collect();
    names.sort_unstable();
    let mut read = asked.clone();
    for (label, condition) in &mut read.conditions {
        if label == NAME {
            *condition = Condition::OneOf(names.clone());
        }
    }
    Ok(Some(read))
}

/// The names among `timer`'s and its parts' that have series.
fn held(connection: &Connection, timer: &str) -> Result<Vec<String>> {
    let failed = |error| sql_error("read a timer's names", error);
    let mut select = connection.prepare_cached(HELD).map_err(failed)?;
    let names = select.query_map(params![timer], |row| row.get(0));
    names.and_then(Iterator::collect).map_err(failed)
}

/// The parts of a timer an aggregate of `op` reads: an `op` a timer has not
/// is `invalid`, and so is one whose part the timer keeps no series of.
fn wanted(op: Op, timer: &str, held: &[String]) -> Result<Vec<Part>> {
    let parts = match op {
        Op::Count | Op::Rate => vec![Part::Count],
        Op::Sum | Op::Avg => vec![Part::Count, Part::Sum],
        Op::Max => vec![Part::Count, Part::Longest],
        Op::Percentile(_) => vec![Part::Count, Part::Buckets],
        _ => {
            let has = "a timer has count, sum, avg, max, rate and percentiles";
            return Err(Error::invalid(format!("{op} of a timer: {has}")).within(timer));
        }
    };
    let missing = parts.iter().find(|part| !held.contains(&part.name(timer)));
    let why = match missing {
        None => return Ok(parts),
        Some(Part::Buckets) => "a timer without buckets has no percentile".to_owned(),
        Some(part) => format!("{op} of a timer that keeps no {}", part.name(timer)),
    };
    Err(Error::invalid(why).within(timer))
}

#[cfg(test)]
#[path = "timer_tests.rs"]
mod tests;
