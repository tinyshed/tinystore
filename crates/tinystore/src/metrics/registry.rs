//! Series by their identity: found, or registered with their dictionary
//! pairs and postings in the write that brings their first samples, within
//! the store's bound of series.
//!
//! A name has one kind while it has series: its first series sets it, and
//! the last to go clears it, so that a kind in the file always means series
//! of that kind exist.
//!
//! A row written here is read back with `last_insert_rowid`, never with
//! `insert … returning id`: SQLite gives a statement that returns what it
//! wrote a table of its own each time it runs, ten times a plain insert's
//! cost, and registering a text's 100,000 series paid it twice a series.

use rusqlite::{OptionalExtension, params};

use super::engine::{execute, query_one};
use super::ingest::Batch;
use super::labels::encode_ids;
use super::series::Kind;
use crate::sqlite::{Tx, sql_error};
use crate::{Error, Result};

const RESOLVE: &str = "select s.id, n.kind from series s join names n on n.id = s.name where s.identity = ?1";
const SERIES_COUNT: &str = "select series_count from store_state where id = 1";
const PAIR: &str = "select id from label_values where name = ?1 and value = ?2";
const INSERT_PAIR: &str = "insert into label_values (name, value) values (?1, ?2)";
const INSERT_SERIES: &str = "insert into series (identity, name, label_ids) values (?1, ?2, ?3)";
const INSERT_POSTING: &str = "insert into postings (label_id, series_id) values (?1, ?2)";
const COUNT_POSTING: &str = "update label_values set posting_count = posting_count + 1 where id = ?1";
/// A name's label one series more holds; the store's own are left out.
const COUNT_NAME_LABEL: &str = "insert into name_labels (name, label, value, series) values (?1, ?2, ?3, 1)
    on conflict (name, label, value) do update set series = series + 1";
const INSERT_STATE: &str = "insert into series_state (series_id, name, oldest, max_seen) values (?1, ?2, ?3, ?3)";
const MORE_SERIES: &str = "update store_state set series_count = series_count + ?1 where id = 1";
const NAME_KIND: &str = "select kind from names where id = ?1";
const SET_KIND: &str = "update names set kind = ?2 where id = ?1";

/// What one write registers series with: the series the store held when the
/// write registered its first, read once, and those it registered since,
/// written once by `finish`; and the pairs of the series it registered last,
/// which the next mostly shares, a text's given labels among them.
///
/// A write whose part it undoes, as the instruments' write undoes a slot,
/// registers that part with a `Registering` of its own, finished before the
/// part ends and dropped with it.
pub(super) struct Registering {
    max_series: i64,
    held: Option<i64>,
    registered: i64,
    last: Vec<(String, String, i64)>,
}

impl Registering {
    pub(super) fn new(max_series: i64) -> Registering {
        Registering { max_series, held: None, registered: 0, last: Vec::new() }
    }

    /// The id of the series of `batch`, and whether this write registered
    /// it, when the file had none yet and held fewer than `max_series`; its
    /// kind must be its name's.
    pub(super) fn resolve(&mut self, tx: &Tx<'_>, batch: &Batch) -> Result<(i64, bool)> {
        let found = tx
            .prepare_cached(RESOLVE)
            .and_then(|mut select| {
                let row = |row: &rusqlite::Row<'_>| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?));
                select.query_row(params![batch.identity], row).optional()
            })
            .map_err(|error| sql_error("find a series", error))?;
        let Some((id, kind)) = found else { return self.register(tx, batch).map(|id| (id, true)) };
        if kind.as_deref() != Some(batch.kind.as_str()) {
            return Err(kind_refused(batch.kind, kind.as_deref()));
        }
        Ok((id, false))
    }

    /// Counts the series the write registered against the store's.
    pub(super) fn finish(self, tx: &Tx<'_>) -> Result<()> {
        if self.registered > 0 {
            execute(tx, MORE_SERIES, params![self.registered], "count the series registered")?;
        }
        Ok(())
    }

    /// Registers a series: its name's kind, its dictionary pairs, the
    /// series, its postings, its name's labels and its state.
    fn register(&mut self, tx: &Tx<'_>, batch: &Batch) -> Result<i64> {
        self.refuse_past_the_bound(tx)?;
        settle_kind(tx, batch)?;
        let mut ids = self.pairs(tx, batch.kept.pairs())?;
        let encoded = encode_ids(&mut ids);
        execute(tx, INSERT_SERIES, params![batch.identity, batch.name, encoded], "register a series")?;
        let id = tx.last_insert_rowid();
        for pair in &ids {
            execute(tx, INSERT_POSTING, params![pair, id], "index a series' label")?;
            execute(tx, COUNT_POSTING, params![pair], "count a label's series")?;
        }
        for (label, value) in batch.kept.pairs().iter().filter(|(label, _)| !super::labels::is_own(label)) {
            execute(tx, COUNT_NAME_LABEL, params![batch.name, label, value], "count a name's label")?;
        }
        execute(tx, INSERT_STATE, params![id, batch.name, batch.points[0].at], "start a series' state")?;
        self.registered += 1;
        Ok(id)
    }

    /// Refuses a series past the store's `max_series`.
    fn refuse_past_the_bound(&mut self, tx: &Tx<'_>) -> Result<()> {
        let held = match self.held {
            Some(held) => held,
            None => *self.held.insert(query_one(tx, SERIES_COUNT, params![], "read the count of series")?),
        };
        let count = held + self.registered;
        if count < self.max_series {
            return Ok(());
        }
        let max_series = self.max_series;
        Err(Error::limit(format!("a series past the {max_series} a store holds, its max_series"))
            .naming("limit", "series in a store")
            .naming("wanted", (count + 1).to_string())
            .naming("bound", max_series.to_string()))
    }

    /// The dictionary ids of `pairs`, those of the series registered last
    /// taken from it, and the others found or made.
    fn pairs(&mut self, tx: &Tx<'_>, pairs: &[(String, String)]) -> Result<Vec<i64>> {
        let mut last = Vec::with_capacity(pairs.len());
        for (label, value) in pairs {
            let known = self.last.iter().find(|(known, of, _)| known == label && of == value).map(|(.., id)| *id);
            let id = match known {
                Some(id) => id,
                None => pair(tx, label, value)?,
            };
            last.push((label.clone(), value.clone(), id));
        }
        self.last = last;
        Ok(self.last.iter().map(|(.., id)| *id).collect())
    }
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
        None => {
            execute(tx, INSERT_PAIR, params![label, value], "write a label pair")?;
            Ok(tx.last_insert_rowid())
        }
    }
}
