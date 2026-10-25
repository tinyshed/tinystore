//! What a name owns, kept in the file with it: its kind while it has series,
//! how long its samples stay, a timer's bounds, and what its values mean.
//!
//! A name's `keep` is its own, as a stream's is in records: a call that
//! gives none changes nothing, and while the store is open one that gives
//! another than an earlier call gave is `invalid`, so that one part of a
//! program never shortens what another keeps.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use rusqlite::params;

use super::labels::NAME;
use crate::sqlite::{File, Tx, sql_error};
use crate::sync::lock;
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
const INSERT_NAME: &str = "insert into names (name, keep) values (?1, ?2)";
/// A name's unit and help in place of what they were, its row made when it
/// has none yet.
const DESCRIBE: &str = "insert into names (name, unit, help) values (?1, ?2, ?3)
    on conflict (name) do update set unit = excluded.unit, help = excluded.help returning id";
const SET_KEEP: &str = "update names set keep = ?2 where id = ?1";
/// Moves when the next sample of each series of a name expires, by what its
/// keep moved, through the name's postings rather than every series.
const MOVE_DUE: &str = "update series_state set due = due + ?2
    where due is not null and series_id in (select series_id from postings
    where label_id = (select id from label_values where name = '__name__' and value = ?1))";

/// What a call of names changes in the file: names it makes, and keeps it
/// moves.
#[derive(Clone, Debug, Default)]
pub(super) struct Changes {
    made: Vec<(String, Option<i64>)>,
    /// A name, its id, its keep before and after, in milliseconds.
    kept: Vec<(String, i64, i64, i64)>,
}

impl Changes {
    fn is_empty(&self) -> bool {
        self.made.is_empty() && self.kept.is_empty()
    }
}

/// A call's names between working out what they change and the write that
/// carries it: an ingest writes the changes in its own savepoint, so that a
/// call refused there makes no name and moves no keep. While it changes any,
/// it holds the names, so that no other call settles them meanwhile.
pub(super) struct Settling<'n> {
    names: &'n Names,
    held: Option<MutexGuard<'n, HashMap<String, Name>>>,
    changes: Changes,
    wanted: Vec<(String, Option<Duration>)>,
}

impl Settling<'_> {
    /// What to write, for a write's work to carry.
    pub(super) fn changes(&self) -> Changes {
        self.changes.clone()
    }

    /// Each name the call gives as it is once its write commits; one it
    /// makes has no id until then.
    pub(super) fn names(&self) -> HashMap<String, Name> {
        let Some(known) = &self.held else { return given(&lock(&self.names.known), &self.wanted) };
        let mut view = given(known, &self.wanted);
        let made = self.changes.made.iter().map(|(name, _)| (name.clone(), 0));
        settle_in(&mut view, &self.changes, made.collect(), &self.wanted);
        view
    }

    /// Takes in what the write that carried the changes made: the ids of
    /// the names it made.
    pub(super) fn settled(self, made: HashMap<String, i64>) {
        if let Some(mut known) = self.held {
            settle_in(&mut known, &self.changes, made, &self.wanted);
        }
    }
}

/// Each wanted name `known` holds.
fn given(known: &HashMap<String, Name>, wanted: &[(String, Option<Duration>)]) -> HashMap<String, Name> {
    wanted.iter().filter_map(|(name, _)| known.get(name).map(|held| (name.clone(), held.clone()))).collect()
}

/// Puts into `known` the names `changes` made, with their ids, and what each
/// wanted name keeps.
fn settle_in(
    known: &mut HashMap<String, Name>,
    changes: &Changes,
    mut ids: HashMap<String, i64>,
    wanted: &[(String, Option<Duration>)],
) {
    for (name, keep) in &changes.made {
        let keep = keep.map(|millis| Duration::from_millis(millis.unsigned_abs()));
        let id = ids.remove(name).unwrap_or_default();
        known.insert(name.clone(), Name { id, keep, said: keep });
    }
    for (name, keep) in wanted {
        if let (Some(keep), Some(held)) = (keep, known.get_mut(name)) {
            (held.keep, held.said) = (Some(*keep), Some(*keep));
        }
    }
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

    /// Reads the names again, as a copy does once its home's commits arrive.
    pub(super) fn read_again(&self, file: &File) -> Result<()> {
        let again = Names::read(file)?;
        *self.known.lock().unwrap_or_else(std::sync::PoisonError::into_inner) =
            again.known.into_inner().unwrap_or_default();
        Ok(())
    }

    pub(super) fn get(&self, name: &str) -> Option<Name> {
        lock(&self.known).get(name).cloned()
    }

    /// The names a call writes, each made the first time it is given and
    /// given the keep the call says, which the file then keeps, in a write
    /// of their own. A keep is checked, and one name given once.
    pub(super) fn settle(&self, file: &File, wanted: &BTreeMap<&str, Option<Duration>>) -> Result<()> {
        let settling = self.settling(wanted)?;
        let changes = settling.changes();
        let made = if changes.is_empty() {
            HashMap::new()
        } else {
            let bytes = changes.made.iter().map(|(name, _)| name.len()).sum();
            file.write(bytes, move |tx| write_changes(tx, &changes))?
        };
        settling.settled(made);
        Ok(())
    }

    /// What a call's names change, for a write to carry: a keep is checked,
    /// and one name given once. A call that changes nothing in the file says
    /// its keeps at once.
    pub(super) fn settling(&self, wanted: &BTreeMap<&str, Option<Duration>>) -> Result<Settling<'_>> {
        let mut known = lock(&self.known);
        let changes = changes(&known, wanted)?;
        let wanted: Vec<(String, Option<Duration>)> =
            wanted.iter().map(|(name, keep)| ((*name).to_owned(), *keep)).collect();
        if changes.is_empty() {
            settle_in(&mut known, &changes, HashMap::new(), &wanted);
            return Ok(Settling { names: self, held: None, changes, wanted });
        }
        Ok(Settling { names: self, held: Some(known), changes, wanted })
    }

    /// What each name would keep once `settle` took `wanted`, written nowhere:
    /// a call checks its samples against it before it makes a name.
    pub(super) fn foreseen(&self, wanted: &BTreeMap<&str, Option<Duration>>) -> HashMap<String, Duration> {
        let known = lock(&self.known);
        let keeps = |name: &str, keep: Option<Duration>| keep.or_else(|| known.get(name).and_then(|held| held.keep));
        wanted.iter().map(|(&name, &keep)| (name.to_owned(), keeps(name, keep).unwrap_or(KEEP))).collect()
    }

    /// Writes a name's unit and help, the name made the first time it is
    /// given.
    pub(super) fn describe(&self, file: &File, name: &str, unit: String, help: String) -> Result<()> {
        let mut known = lock(&self.known);
        let (owned, bytes) = (name.to_owned(), name.len() + unit.len() + help.len());
        let id = file.write(bytes, move |tx| {
            let mut describe = tx.prepare_cached(DESCRIBE).map_err(|error| sql_error("describe a name", error))?;
            describe
                .query_row(params![owned, unit, help], |row| row.get(0))
                .map_err(|error| sql_error("describe a name", error))
        })?;
        known.entry(name.to_owned()).or_insert(Name { id, keep: None, said: None });
        Ok(())
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
            let (keep, said) = (crate::text::span(keep), crate::text::span(said));
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

/// Writes the names a call makes and the keeps it moves, and gives the ids
/// of those made by their names.
pub(super) fn write_changes(tx: &Tx<'_>, changes: &Changes) -> Result<HashMap<String, i64>> {
    let failed = |error| sql_error("write a name", error);
    let mut ids = HashMap::with_capacity(changes.made.len());
    // read back with last_insert_rowid: `returning` costs SQLite a table of its own each time
    for (name, keep) in &changes.made {
        tx.prepare_cached(INSERT_NAME).and_then(|mut insert| insert.execute(params![name, keep])).map_err(failed)?;
        ids.insert(name.clone(), tx.last_insert_rowid());
    }
    for (name, id, before, after) in &changes.kept {
        let moved = tx.prepare_cached(SET_KEEP).and_then(|mut set| set.execute(params![id, after]));
        moved.map_err(|error| sql_error("write a name's keep", error))?;
        let due = tx.prepare_cached(MOVE_DUE).and_then(|mut set| set.execute(params![name, after - before]));
        due.map_err(|error| sql_error("move when a name's series expire", error))?;
    }
    debug_assert_eq!(NAME, "__name__", "MOVE_DUE finds a name's series by this label");
    Ok(ids)
}

/// A keep a call gave, in whole milliseconds, within its bounds.
pub(super) fn checked_keep(keep: Duration) -> Result<Duration> {
    let keep = Duration::from_millis(crate::clock::millis(keep) as u64);
    if keep.is_zero() || keep > LONGEST_KEEP {
        return Err(Error::invalid(format!(
            "keep {}: a name keeps its samples from 1ms to 100 years",
            crate::text::span(keep)
        )));
    }
    Ok(keep)
}

pub(super) use crate::clock::millis;
