//! What a name owns, kept in the file with it: its kind while it has series,
//! how long its samples stay, a timer's bounds, and what its values mean.
//!
//! A name's `keep` is its own, as a stream's is in records: a call that
//! gives none changes nothing, and while the store is open one that gives
//! another than an earlier call gave is `invalid`, so that one part of a
//! program never shortens what another keeps.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::time::Duration;

use rusqlite::params;

use super::engine::lock;
use super::labels::NAME;
use crate::sqlite::{File, sql_error};
use crate::{Error, Result};

/// How long a name's samples stay until a call says.
pub(super) const KEEP: Duration = Duration::from_hours(30 * 24);
/// The longest a name keeps its samples.
const LONGEST_KEEP: Duration = Duration::from_hours(100 * 365 * 24);

/// A name as the engine knows it.
#[derive(Clone, Debug)]
pub(super) struct Name {
    pub(super) id: i64,
    /// What the file says it keeps.
    keep: Option<Duration>,
    /// What a call of this run gave: another is refused till the store
    /// closes.
    said: Option<Duration>,
}

impl Name {
    pub(super) fn keeps(&self) -> Duration {
        self.keep.unwrap_or(KEEP)
    }
}

/// Every name the file holds, by its text.
#[derive(Debug)]
pub(super) struct Names {
    known: Mutex<HashMap<String, Name>>,
}

const SELECT_NAMES: &str = "select id, name, keep from names";
const INSERT_NAME: &str = "insert into names (name, keep) values (?1, ?2) returning id";
const SET_KEEP: &str = "update names set keep = ?2 where id = ?1";
/// Moves when the next sample of each series of a name expires, by what its
/// keep moved, through the name's postings rather than every series.
const MOVE_DUE: &str = "update series_state set due = due + ?2
    where due is not null and series_id in (select series_id from postings
    where label_id = (select id from label_values where name = '__name__' and value = ?1))";

/// What a call of names changes in the file: names it makes, and keeps it
/// moves.
#[derive(Default)]
struct Changes {
    made: Vec<(String, Option<i64>)>,
    /// A name, its id, its keep before and after, in milliseconds.
    kept: Vec<(String, i64, i64, i64)>,
}

impl Names {
    pub(super) fn read(file: &File) -> Result<Names> {
        let failed = |error| sql_error("read the names", error);
        let known = file.read(|connection| {
            let mut select = connection.prepare(SELECT_NAMES).map_err(failed)?;
            let names = select.query_map([], |row| {
                let keep: Option<i64> = row.get(2)?;
                let keep = keep.map(|millis| Duration::from_millis(millis.max(0).unsigned_abs()));
                Ok((row.get(1)?, Name { id: row.get(0)?, keep, said: None }))
            });
            names.and_then(Iterator::collect).map_err(failed)
        })?;
        Ok(Names { known: Mutex::new(known) })
    }

    pub(super) fn get(&self, name: &str) -> Option<Name> {
        lock(&self.known).get(name).cloned()
    }

    /// The names a call writes, each made the first time it is given and
    /// given the keep the call says, which the file then keeps. A keep is
    /// checked, and one name given once.
    pub(super) fn settle(
        &self,
        file: &File,
        wanted: &BTreeMap<&str, Option<Duration>>,
    ) -> Result<HashMap<String, Name>> {
        let mut known = lock(&self.known);
        let changes = changes(&known, wanted)?;
        let ids = write(file, &changes)?;
        for ((name, keep), id) in changes.made.into_iter().zip(ids) {
            let keep = keep.map(|millis| Duration::from_millis(millis.unsigned_abs()));
            known.insert(name, Name { id, keep, said: keep });
        }
        for (name, keep) in wanted {
            if let (Some(keep), Some(held)) = (keep, known.get_mut(*name)) {
                (held.keep, held.said) = (Some(*keep), Some(*keep));
            }
        }
        Ok(wanted.keys().filter_map(|name| known.get(*name).map(|held| ((*name).to_owned(), held.clone()))).collect())
    }
}

/// What a call's names change: the names it makes, and the keeps it moves;
/// a keep another than one said before while the store is open is refused.
fn changes(known: &HashMap<String, Name>, wanted: &BTreeMap<&str, Option<Duration>>) -> Result<Changes> {
    let mut changes = Changes::default();
    for (&name, &keep) in wanted {
        let Some(held) = known.get(name) else {
            changes.made.push((name.to_owned(), keep.map(millis)));
            continue;
        };
        let Some(keep) = keep else { continue };
        if let Some(said) = held.said.filter(|said| *said != keep) {
            let (keep, said) = (span(keep), span(said));
            return Err(Error::invalid(format!(
                "metrics {name}: keep {keep}, where it is kept {said} while the store is open"
            )));
        }
        if held.keep != Some(keep) {
            changes.kept.push((name.to_owned(), held.id, millis(held.keeps()), millis(keep)));
        }
    }
    Ok(changes)
}

/// Writes the names a call makes and the keeps it moves, in one write, and
/// gives the ids of those made.
fn write(file: &File, changes: &Changes) -> Result<Vec<i64>> {
    if changes.made.is_empty() && changes.kept.is_empty() {
        return Ok(Vec::new());
    }
    let (made, kept) = (changes.made.clone(), changes.kept.clone());
    let bytes = made.iter().map(|(name, _)| name.len()).sum();
    file.write(bytes, move |tx| {
        let failed = |error| sql_error("write a name", error);
        let mut ids = Vec::with_capacity(made.len());
        for (name, keep) in &made {
            let mut insert = tx.prepare_cached(INSERT_NAME).map_err(failed)?;
            ids.push(insert.query_row(params![name, keep], |row| row.get(0)).map_err(failed)?);
        }
        for (name, id, before, after) in &kept {
            let moved = tx.prepare_cached(SET_KEEP).and_then(|mut set| set.execute(params![id, after]));
            moved.map_err(|error| sql_error("write a name's keep", error))?;
            let due = tx.prepare_cached(MOVE_DUE).and_then(|mut set| set.execute(params![name, after - before]));
            due.map_err(|error| sql_error("move when a name's series expire", error))?;
        }
        debug_assert_eq!(NAME, "__name__", "MOVE_DUE finds a name's series by this label");
        Ok(ids)
    })
}

/// A keep a call gave, in whole milliseconds, within its bounds.
pub(super) fn checked_keep(keep: Duration) -> Result<Duration> {
    let keep = Duration::from_millis(u64::try_from(keep.as_millis()).unwrap_or(u64::MAX));
    if keep.is_zero() || keep > LONGEST_KEEP {
        return Err(Error::invalid(format!("keep {}: a name keeps its samples from 1ms to 100 years", span(keep))));
    }
    Ok(keep)
}

pub(super) fn millis(span: Duration) -> i64 {
    i64::try_from(span.as_millis()).unwrap_or(i64::MAX)
}

/// A span as a program writes it: `30d`, `12h`, `90s`, `250ms`.
pub(super) fn span(span: Duration) -> String {
    let millis = span.as_millis();
    let units = [("d", 86_400_000), ("h", 3_600_000), ("m", 60_000), ("s", 1000)];
    match units.iter().find(|(_, unit)| millis >= *unit && millis.is_multiple_of(*unit)) {
        Some((name, unit)) => format!("{}{name}", millis / unit),
        None => format!("{millis}ms"),
    }
}
