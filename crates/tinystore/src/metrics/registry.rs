//! Series by their identity: found, or registered with their dictionary
//! pairs and postings in the write that brings their first samples, within
//! the store's bound of series.
//!
//! A name has one kind while it has series: its first series sets it, and
//! the last to go clears it, so that a kind in the file always means series
//! of that kind exist.

use rusqlite::{OptionalExtension, params};

use super::engine::{execute, query_one};
use super::ingest::Batch;
use super::labels::encode_ids;
use super::series::Kind;
use crate::sqlite::{Tx, sql_error};
use crate::{Error, Result};

/// The series a store holds at most.
pub(super) const MOST_SERIES: i64 = 100_000;

const RESOLVE: &str = "select s.id, n.kind from series s join names n on n.id = s.name where s.identity = ?1";
const SERIES_COUNT: &str = "select series_count from store_state where id = 1";
const PAIR: &str = "select id from label_values where name = ?1 and value = ?2";
const INSERT_PAIR: &str = "insert into label_values (name, value) values (?1, ?2) returning id";
const INSERT_SERIES: &str = "insert into series (identity, name, label_ids) values (?1, ?2, ?3) returning id";
const INSERT_POSTING: &str = "insert into postings (label_id, series_id) values (?1, ?2)";
const COUNT_POSTING: &str = "update label_values set posting_count = posting_count + 1 where id = ?1";
/// A name's label one series more holds; the store's own are left out.
const COUNT_NAME_LABEL: &str = "insert into name_labels (name, label, value, series) values (?1, ?2, ?3, 1)
    on conflict (name, label, value) do update set series = series + 1";
const INSERT_STATE: &str = "insert into series_state (series_id, name, oldest, max_seen) values (?1, ?2, ?3, ?3)";
const ONE_SERIES_MORE: &str = "update store_state set series_count = series_count + 1 where id = 1";
const NAME_KIND: &str = "select kind from names where id = ?1";
const SET_KIND: &str = "update names set kind = ?2 where id = ?1";

/// The id of the series of `batch`, and whether this write registered it,
/// when the file had none yet; its kind must be its name's.
pub(super) fn resolve(tx: &Tx<'_>, batch: &Batch) -> Result<(i64, bool)> {
    let found = tx
        .prepare_cached(RESOLVE)
        .and_then(|mut select| {
            select
                .query_row(params![batch.identity], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?)))
                .optional()
        })
        .map_err(|error| sql_error("find a series", error))?;
    let Some((id, kind)) = found else { return register(tx, batch).map(|id| (id, true)) };
    if kind.as_deref() != Some(batch.kind.as_str()) {
        return Err(kind_refused(batch.kind, kind.as_deref()));
    }
    Ok((id, false))
}

/// Registers a series: its name's kind, its dictionary pairs, the series,
/// its postings, its name's labels and its state, and one series more
/// against the bound.
fn register(tx: &Tx<'_>, batch: &Batch) -> Result<i64> {
    let count: i64 = query_one(tx, SERIES_COUNT, params![], "read the count of series")?;
    if count >= MOST_SERIES {
        return Err(Error::limit(format!("a series past the {MOST_SERIES} a store holds"))
            .naming("limit", "series in a store")
            .naming("wanted", (count + 1).to_string())
            .naming("bound", MOST_SERIES.to_string()));
    }
    settle_kind(tx, batch)?;
    let mut ids = Vec::with_capacity(batch.kept.pairs().len());
    for (label, value) in batch.kept.pairs() {
        ids.push(pair(tx, label, value)?);
    }
    let encoded = encode_ids(&mut ids);
    let id: i64 = query_one(tx, INSERT_SERIES, params![batch.identity, batch.name, encoded], "register a series")?;
    for pair in &ids {
        execute(tx, INSERT_POSTING, params![pair, id], "index a series' label")?;
        execute(tx, COUNT_POSTING, params![pair], "count a label's series")?;
    }
    for (label, value) in batch.kept.pairs().iter().filter(|(label, _)| !label.starts_with("__")) {
        execute(tx, COUNT_NAME_LABEL, params![batch.name, label, value], "count a name's label")?;
    }
    execute(tx, INSERT_STATE, params![id, batch.name, batch.points[0].at], "start a series' state")?;
    execute(tx, ONE_SERIES_MORE, params![], "count a series")?;
    Ok(id)
}

/// Sets a name's kind with its first series; a name that has series keeps
/// its kind.
fn settle_kind(tx: &Tx<'_>, batch: &Batch) -> Result<()> {
    let kind: Option<String> = query_one(tx, NAME_KIND, params![batch.name], "read a name's kind")?;
    match kind {
        None => execute(tx, SET_KIND, params![batch.name, batch.kind.as_str()], "write a name's kind").map(drop),
        Some(kind) if kind == batch.kind.as_str() => Ok(()),
        Some(kind) => Err(kind_refused(batch.kind, Some(&kind))),
    }
}

fn kind_refused(asked: Kind, kept: Option<&str>) -> Error {
    let kept = kept.unwrap_or("none");
    Error::invalid(format!("a {asked}, where the name's series are {kept}s: a name has one kind while it has series"))
}

/// The dictionary id of a pair, made the first time.
fn pair(tx: &Tx<'_>, label: &str, value: &str) -> Result<i64> {
    let found = tx
        .prepare_cached(PAIR)
        .and_then(|mut select| select.query_row(params![label, value], |row| row.get(0)).optional())
        .map_err(|error| sql_error("find a label pair", error))?;
    match found {
        Some(id) => Ok(id),
        None => query_one(tx, INSERT_PAIR, params![label, value], "write a label pair"),
    }
}
