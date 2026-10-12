//! Series by their labels, through the postings: the smallest set of series
//! a condition finds drives, every other set is checked against it, and a
//! `NoneOf` leaves series out. Nothing scans every series.
//!
//! ```text
//! status one of 500, 502: 40 series   host = web-1: 900   env none of dev
//!   → from status's 40, each in host's set, none in env=dev's
//! ```

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::types::Value;
use rusqlite::{Connection, params, params_from_iter};

use super::budget::Budget;
use super::labels::{Kept, decode_ids};
use super::query::{Asked, Condition};
use super::series::Kind;
use crate::sqlite::{TextBytes, prefix_end, sql_error};
use crate::{Error, Result};

/// A series a query found: its id, kind and labels.
#[derive(Clone, Debug)]
pub(super) struct Found {
    pub(super) id: i64,
    pub(super) kind: Kind,
    pub(super) kept: Kept,
}

/// The postings a condition stands for.
#[derive(Debug)]
enum Set {
    /// Pairs by their dictionary ids.
    Pairs(Vec<i64>),
    /// Every value of a label that starts with a prefix.
    Prefix { label: String, prefix: String },
}

const PAIRS: &str =
    "select id, posting_count from label_values where name = ?1 and value in (select value from json_each(?2))";
const PREFIX_COUNT: &str = "select coalesce(sum(posting_count), 0) from label_values
    where name = ?1 and value >= ?2 and (?3 is null or value < ?3)";
const LABELS: &str = "select id, name, value from label_values where id in (select value from json_each(?1))";

/// The series that meet every condition, in the order of their ids.
pub(super) fn find(connection: &Connection, asked: &Asked, budget: &mut Budget) -> Result<Vec<Found>> {
    let Some((finding, leaving)) = sets(connection, asked)? else { return Ok(Vec::new()) };
    let rows = series_of(connection, &finding, &leaving, budget)?;
    labelled(connection, rows, budget)
}

/// The sets that find series, the smallest first, and those that leave
/// series out; none when a set that finds series holds none.
fn sets(connection: &Connection, asked: &Asked) -> Result<Option<(Vec<Set>, Vec<Set>)>> {
    let (mut finding, mut leaving) = (Vec::new(), Vec::new());
    for (label, condition) in &asked.conditions {
        match condition {
            Condition::OneOf(values) => {
                let (ids, series) = pairs(connection, label, values)?;
                if series == 0 {
                    return Ok(None);
                }
                finding.push((series, Set::Pairs(ids)));
            }
            Condition::Prefix(prefix) => {
                let end = prefix_end(prefix);
                let counted = connection
                    .prepare_cached(PREFIX_COUNT)
                    .and_then(|mut count| {
                        count.query_row(params![label, prefix, end.as_deref().map(TextBytes)], |row| row.get(0))
                    })
                    .map_err(|error| sql_error("count a prefix's series", error))?;
                if counted == 0 {
                    return Ok(None);
                }
                finding.push((counted, Set::Prefix { label: label.clone(), prefix: prefix.clone() }));
            }
            Condition::NoneOf(values) => {
                let (ids, _) = pairs(connection, label, values)?;
                if !ids.is_empty() {
                    leaving.push(Set::Pairs(ids));
                }
            }
        }
    }
    finding.sort_by_key(|(series, _)| *series);
    Ok(Some((finding.into_iter().map(|(_, set)| set).collect(), leaving)))
}

/// The ids of a label's values and how many series they hold.
fn pairs(connection: &Connection, label: &str, values: &[String]) -> Result<(Vec<i64>, i64)> {
    let values =
        serde_json::to_string(values).map_err(|error| Error::internal("a condition's values").with_source(error))?;
    let failed = |error| sql_error("find a label's values", error);
    let mut select = connection.prepare_cached(PAIRS).map_err(failed)?;
    let rows = select.query_map(params![label, values], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)));
    let rows: Vec<(i64, i64)> = rows.and_then(Iterator::collect).map_err(failed)?;
    Ok((rows.iter().map(|(id, _)| *id).collect(), rows.iter().map(|(_, series)| series).sum()))
}

/// What a set asks of a row of postings `alias`, and the values it binds.
fn membership(set: &Set, alias: &str, values: &mut Vec<Value>) -> String {
    match set {
        Set::Pairs(ids) => {
            values.push(Value::Text(serde_json::to_string(ids).unwrap_or_default()));
            format!("{alias}.label_id in (select value from json_each(?{}))", values.len())
        }
        Set::Prefix { label, prefix } => {
            values.push(Value::Text(label.clone()));
            values.push(Value::Text(prefix.clone()));
            // a prefix's end may not be UTF-8, so it is bound as text bytes
            values.push(prefix_end(prefix).map_or(Value::Null, Value::Blob));
            let (name, start, end) = (values.len() - 2, values.len() - 1, values.len());
            format!(
                "{alias}.label_id in (select id from label_values where name = ?{name} and value >= ?{start} \
                 and (?{end} is null or value < cast(?{end} as text)))"
            )
        }
    }
}

/// A matched series' row: its id, its name's kind and its label ids.
pub(super) type Row = (i64, Option<String>, Vec<u8>);

/// The series of the postings every finding set holds and no leaving set
/// does, the first past the budget's count refused.
fn series_of(connection: &Connection, finding: &[Set], leaving: &[Set], budget: &mut Budget) -> Result<Vec<Row>> {
    let mut values = Vec::new();
    let mut filter = format!("select p.series_id from postings p where {}", membership(&finding[0], "p", &mut values));
    for (set, negated) in finding[1..].iter().map(|set| (set, "")).chain(leaving.iter().map(|set| (set, "not "))) {
        let member = membership(set, "q", &mut values);
        filter.push_str(&format!(
            " and {negated}exists (select 1 from postings q where {member} and q.series_id = p.series_id)"
        ));
    }
    values.push(Value::Integer(budget.series_limit() as i64 + 1));
    let sql = format!(
        "select s.id, n.kind, s.label_ids from ({filter}) m join series s on s.id = m.series_id \
         join names n on n.id = s.name order by s.id limit cast(?{} as integer)",
        values.len()
    );
    let failed = |error| sql_error("match series", error);
    let mut select = connection.prepare_cached(&sql).map_err(failed)?;
    let mut rows = select.query(params_from_iter(values)).map_err(failed)?;
    let mut matched = Vec::new();
    while let Some(row) = rows.next().map_err(failed)? {
        budget.take_series()?;
        let ids: Vec<u8> = row.get(2).map_err(failed)?;
        budget.take_bytes(ids.len())?;
        matched.push((row.get(0).map_err(failed)?, row.get(1).map_err(failed)?, ids));
    }
    Ok(matched)
}

/// Each series' labels, read from the dictionary in one statement for the
/// whole match rather than one a series.
pub(super) fn labelled(connection: &Connection, rows: Vec<Row>, budget: &mut Budget) -> Result<Vec<Found>> {
    let mut ids = Vec::with_capacity(rows.len());
    for (_, _, label_ids) in &rows {
        ids.push(decode_ids(label_ids)?);
    }
    let wanted: BTreeSet<i64> = ids.iter().flatten().copied().collect();
    let wanted = serde_json::to_string(&wanted).map_err(|error| Error::internal("label ids").with_source(error))?;
    let failed = |error| sql_error("read the labels of series", error);
    let mut select = connection.prepare_cached(LABELS).map_err(failed)?;
    let pairs = select.query_map(params![wanted], |row| Ok((row.get::<_, i64>(0)?, (row.get(1)?, row.get(2)?))));
    let dictionary: BTreeMap<i64, (String, String)> = pairs.and_then(Iterator::collect).map_err(failed)?;
    budget.take_bytes(dictionary.values().map(|(name, value)| name.len() + value.len()).sum())?;
    let mut found = Vec::with_capacity(rows.len());
    for ((id, kind, _), label_ids) in rows.into_iter().zip(ids) {
        let pairs = label_ids.iter().map(|label| dictionary.get(label).cloned());
        let pairs = pairs
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| Error::corrupt("a series' label missing from the dictionary"))?;
        let kind =
            kind.as_deref().and_then(Kind::parse).ok_or_else(|| Error::corrupt("a series whose name has no kind"))?;
        found.push(Found { id, kind, kept: Kept::read(pairs)? });
    }
    Ok(found)
}
