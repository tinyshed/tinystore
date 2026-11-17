//! A set of files as a cache: its ceiling, which the set keeps in its own row
//! so that it holds whoever writes, what a write past it does, and the order
//! a write's evictions and a trim remove files in.
//!
//! The set's row keeps its files' count and sizes, moved by triggers on the
//! files' table (`0004_cache.sql`), so a write reads its set's total in one
//! row. A write past the ceiling removes the lowest ranks in its own
//! savepoint, at most [`EVICT_AT_ONCE`] files, and fails `limit` with its
//! evictions rolled back when that leaves the set over `max(ceiling, its
//! size before)`. A file's rank is written by the use a read records
//! (`uses.rs`): eviction walks `(set_id, coalesce(rank, modified))` from its
//! start.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use rusqlite::{OptionalExtension, params};

use super::change::Change;
use super::engine::{Blobs, rows_of};
use super::files::{Files, Writing};
use crate::limits::Limit;
use crate::sqlite::{Tx, own_work, sql_error};
use crate::sync::lock;
use crate::{Error, ErrorKind, Result};

/// What a write that would take a set past its ceiling does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum WhenFull {
    /// Removes the files used least recently until the set fits: a cache.
    #[default]
    Evict,
    /// Fails `limit` and removes nothing: a quota of files that must stay.
    Refuse,
}

impl WhenFull {
    /// As the set's row and the wire keep it.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            WhenFull::Evict => "evict",
            WhenFull::Refuse => "refuse",
        }
    }

    pub(crate) fn of(text: &str) -> Result<WhenFull> {
        match text {
            "evict" => Ok(WhenFull::Evict),
            "refuse" => Ok(WhenFull::Refuse),
            other => Err(Error::invalid(format!("when full {other:?}: evict or refuse"))),
        }
    }
}

/// What a trim removed: its files, and their sizes as `usage` counts them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Trim {
    pub evictions: u64,
    pub freed: u64,
}

/// A set's ceiling, as its row keeps it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Ceiling {
    pub(crate) max_size: u64,
    pub(crate) when_full: WhenFull,
}

/// What a change of the ceiling expects the set to keep before it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Expect {
    /// Whatever it keeps.
    #[default]
    Any,
    /// No ceiling: an open that writes the first.
    Absent,
    /// This ceiling, as `if_current` names it.
    Is(u64),
}

/// Files one write, or one batch of a trim, removes at most: enough for a
/// write of 50 MiB into a set of 5 KiB thumbnails.
pub(crate) const EVICT_AT_ONCE: usize = 10_000;

/// Rows one look at the lowest ranks reads.
const LOWEST_AT_ONCE: usize = 1000;

/// A set's sizes together, as a `limit` error names the bound.
const SET_BYTES: &str = "set bytes";

const SELECT_TOTAL: &str = "select size, max_size, when_full from _tinystore_blobs_sets where id = ?1";
const SELECT_LOWEST: &str = "select path, size from _tinystore_blobs_files where set_id = ?1 and path <> ?2
    order by coalesce(rank, modified), path limit cast(?3 as integer)";
const SET_CEILING: &str = "update _tinystore_blobs_sets set max_size = ?2, when_full = ?3 where id = ?1";
const SELECT_OVER: &str = "select id, name, max_size from _tinystore_blobs_sets
    where max_size is not null and when_full = 'evict' and size > max_size";

/// A set's size, its files' sizes together, and its ceiling.
pub(crate) struct Total {
    pub(crate) size: i64,
    pub(crate) ceiling: Option<Ceiling>,
}

impl Total {
    fn read(row: &rusqlite::Row<'_>) -> rusqlite::Result<(i64, Option<i64>, Option<String>)> {
        Ok((row.get(0)?, row.get(1)?, row.get(2)?))
    }

    fn of((size, max_size, when_full): (i64, Option<i64>, Option<String>)) -> Result<Total> {
        let ceiling = match max_size {
            None => None,
            Some(max_size) => Some(Ceiling {
                max_size: u64::try_from(max_size).map_err(|_| Error::corrupt(format!("a ceiling of {max_size}")))?,
                when_full: WhenFull::of(when_full.as_deref().unwrap_or("evict"))
                    .map_err(|error| Error::corrupt(format!("a set's ceiling: {error}")))?,
            }),
        };
        Ok(Total { size, ceiling })
    }
}

/// Reads a set's total on `connection`.
pub(crate) fn total_on(connection: &rusqlite::Connection, set: i64) -> Result<Total> {
    let row = connection
        .prepare_cached(SELECT_TOTAL)
        .and_then(|mut statement| statement.query_row([set], Total::read).optional())
        .map_err(|error| sql_error("blobs: a set's total", error))?;
    Total::of(row.ok_or_else(|| Error::not_found(format!("set {set}, which a handle names, is gone")))?)
}

/// The files the ceilings removed since the store opened, by set; a trim the
/// program asked for counts in its own answer instead.
#[derive(Default)]
pub(crate) struct Evictions(Mutex<HashMap<i64, u64>>);

impl Evictions {
    pub(crate) fn of(&self, set: i64) -> u64 {
        lock(&self.0).get(&set).copied().unwrap_or(0)
    }

    fn add(&self, set: i64, evicted: u64) {
        *lock(&self.0).entry(set).or_default() += evicted;
    }
}

/// Whose evictions a write's commit counts: the set's id and name.
#[derive(Clone)]
pub(crate) struct Counted {
    pub(crate) set: i64,
    pub(crate) name: Arc<str>,
}

impl Change<'_, '_> {
    pub(crate) fn total(&self, set: i64) -> Result<Total> {
        total_on(self.tx, set)
    }

    /// Leaves the set within its ceiling once a write wrote `written`, a file
    /// of its bytes at its path, into a set `before` bytes large. A refusing
    /// set fails a write that grows it past its ceiling; an evicting one
    /// removes its lowest ranks, never the file written, and fails a write
    /// that still leaves it past `max(ceiling, before)`.
    pub(crate) fn fit(&mut self, counted: &Counted, written: (&str, i64), before: i64) -> Result<()> {
        let total = self.total(counted.set)?;
        let Some(ceiling) = total.ceiling else { return Ok(()) };
        let most = i64::try_from(ceiling.max_size).unwrap_or(i64::MAX);
        if total.size <= most {
            return Ok(());
        }
        match ceiling.when_full {
            WhenFull::Refuse if total.size > before => Err(past_the_ceiling(total.size, ceiling.max_size)
                .within(format!("the set holds {before} bytes, and refuses a write past its ceiling"))),
            WhenFull::Refuse => Ok(()),
            WhenFull::Evict => {
                let (path, size) = written;
                if size > most {
                    let refused = format!("a file of {size} bytes, past the set's ceiling of {most}");
                    return Err(Limit { name: SET_BYTES, bound: ceiling.max_size }.refuse(size as u64, refused));
                }
                let (evicted, freed) = self.evict(counted.set, path, total.size - most)?;
                let after = total.size - freed;
                if after > most.max(before) {
                    return Err(past_the_ceiling(after, ceiling.max_size).within(format!(
                        "{EVICT_AT_ONCE} evictions leave the set past its ceiling; maintenance trims it"
                    )));
                }
                self.count_evictions(counted, evicted);
                Ok(())
            }
        }
    }

    /// Removes the set's lowest ranks, but the file at `keep`, until they
    /// freed `over` bytes or [`EVICT_AT_ONCE`] files went; says how many went
    /// and what they freed.
    pub(crate) fn evict(&mut self, set: i64, keep: &str, over: i64) -> Result<(usize, i64)> {
        let (mut evicted, mut freed) = (0, 0);
        while freed < over && evicted < EVICT_AT_ONCE {
            let wanted = LOWEST_AT_ONCE.min(EVICT_AT_ONCE - evicted) as i64;
            let lowest: Vec<(String, i64)> = rows_of(
                self.tx,
                (SELECT_LOWEST, "blobs: the files used least recently"),
                params![set, keep, wanted],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            if lowest.is_empty() {
                break;
            }
            for (path, size) in lowest {
                if freed >= over {
                    break;
                }
                self.remove(set, &path)?;
                (evicted, freed) = (evicted + 1, freed + size);
            }
        }
        Ok((evicted, freed))
    }

    /// Counts what a ceiling evicted once the write's commit is in the file.
    fn count_evictions(&self, counted: &Counted, evicted: usize) {
        if evicted == 0 {
            return;
        }
        let (evictions, probe, counted) =
            (Arc::clone(&self.blobs.evictions), Arc::clone(&self.blobs.probe), counted.clone());
        self.tx.after_commit(Box::new(move || {
            evictions.add(counted.set, evicted as u64);
            probe.count("tinystore_files_evicted_total", &[("set", &counted.name)], evicted as f64);
        }));
    }
}

/// A set past its ceiling, `size` bytes large.
fn past_the_ceiling(size: i64, max_size: u64) -> Error {
    let refused = format!("a set of {size} bytes, past its ceiling of {max_size}");
    Limit { name: SET_BYTES, bound: max_size }.refuse(size.unsigned_abs(), refused)
}

/// The ceiling a set keeps, read on `connection`.
pub(crate) fn kept_on(connection: &rusqlite::Connection, set: i64) -> Result<Option<Ceiling>> {
    total_on(connection, set).map(|total| total.ceiling)
}

/// What an open asks of the set's ceiling: one to keep, or to find.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Asked {
    pub(crate) max_size: Option<u64>,
    pub(crate) when_full: Option<WhenFull>,
}

impl Asked {
    /// The ceiling the set keeps once an open asked this: the first open with
    /// one writes it, and one that names another than the set keeps fails
    /// `conflict`, since an open never changes what data already stored
    /// lives by.
    pub(crate) fn settle(self, files: &Files, writes: bool) -> Result<Option<Ceiling>> {
        let kept = files.blobs.file.read(|connection| kept_on(connection, files.set))?;
        match (self.max_size, kept) {
            (None, None) if self.when_full.is_some() => {
                Err(Error::invalid("when_full names what a ceiling does, and the set keeps no max_size"))
            }
            (None, kept) => self.against(kept).map(|()| kept),
            (Some(_), Some(kept)) => self.against(Some(kept)).map(|()| Some(kept)),
            (Some(max_size), None) if writes => {
                let first = Ceiling { max_size, when_full: self.when_full.unwrap_or_default() };
                match own_work(|| files.change_ceiling(Some(first), Expect::Absent)) {
                    Ok(_) => Ok(Some(first)),
                    // another open wrote the first: this one is held to it
                    Err(error) if error.kind() == ErrorKind::Conflict => self.settle(files, false),
                    Err(error) => Err(error),
                }
            }
            (Some(max_size), None) => Err(Error::invalid(format!(
                "a ceiling of {max_size} bytes, which the set does not keep, and this open writes none"
            ))),
        }
    }

    /// Refuses an open whose ceiling is not the one the set keeps.
    fn against(self, kept: Option<Ceiling>) -> Result<()> {
        let conflict = |what: String| Err(Error::new(ErrorKind::Conflict, format!("{what}: set_max_size changes it")));
        match (self.max_size, kept) {
            (Some(asked), Some(kept)) if asked != kept.max_size => {
                conflict(format!("a ceiling of {asked} bytes, and the set keeps {}", kept.max_size))
            }
            (Some(asked), None) => conflict(format!("a ceiling of {asked} bytes, and the set keeps none")),
            _ => match (self.when_full, kept) {
                (Some(asked), Some(kept)) if asked != kept.when_full => conflict(format!(
                    "when full {}, and the set's ceiling does {}",
                    asked.as_str(),
                    kept.when_full.as_str()
                )),
                _ => Ok(()),
            },
        }
    }
}

/// A change of a set's ceiling, its condition before [`SetMaxSize::apply`].
#[derive(Debug)]
#[must_use = "a change of the ceiling does nothing until apply()"]
pub struct SetMaxSize<'f> {
    files: &'f Files,
    max_size: Option<u64>,
    when_full: Option<WhenFull>,
    expect: Expect,
}

impl Files {
    /// Changes the set's ceiling for every handle, in this process and in
    /// every other, from its commit on; a lower one evicts at once.
    pub fn set_max_size(&self, bytes: u64) -> SetMaxSize<'_> {
        SetMaxSize { files: self, max_size: Some(bytes), when_full: None, expect: Expect::Any }
    }

    /// Removes the set's ceiling: it grows as its writes take it.
    pub fn remove_max_size(&self) -> SetMaxSize<'_> {
        SetMaxSize { files: self, max_size: None, when_full: None, expect: Expect::Any }
    }

    /// Removes files in the eviction's order until the set holds at most `to`
    /// bytes, and leaves its ceiling where it was: a batch of 10,000 files a
    /// commit, so that no trim holds the writer long.
    pub fn trim(&self, to: u64) -> Result<Trim> {
        self.usable()?;
        #[cfg(feature = "replication")]
        if self.is_copy() {
            return self.pass_trim(to).map_err(|error| error.within(self.describe()));
        }
        self.trimmed(to, None).map_err(|error| error.within(self.describe()))
    }

    /// The ceiling the set keeps now, and what a write past it does.
    #[cfg(feature = "wire")]
    pub(crate) fn kept_ceiling(&self) -> Result<Option<(u64, WhenFull)>> {
        let kept = self.blobs.file.read(|connection| kept_on(connection, self.set))?;
        Ok(kept.map(|ceiling| (ceiling.max_size, ceiling.when_full)))
    }

    /// Trims the set to `to`, each batch its own commit; with `counted`, as a
    /// ceiling's evictions, which a refusing set never makes.
    pub(crate) fn trimmed(&self, to: u64, counted: Option<Counted>) -> Result<Trim> {
        let mut trim = Trim::default();
        loop {
            let ((evicted, freed), unnamed) = self.blobs.file.write(0, self.trimming(to, counted.clone()))?;
            self.blobs.remove_freed(&unnamed);
            trim.evictions += evicted as u64;
            trim.freed += freed.unsigned_abs();
            if evicted < EVICT_AT_ONCE {
                return Ok(trim);
            }
        }
    }

    /// One batch of a trim: the files it removed and what they freed.
    fn trimming(&self, to: u64, counted: Option<Counted>) -> Writing<(usize, i64)> {
        let (blobs, set) = (Arc::clone(&self.blobs), self.set);
        let to = i64::try_from(to).unwrap_or(i64::MAX);
        Box::new(move |tx: &Tx<'_>| {
            let mut change = Change::new(tx, &blobs);
            let total = change.total(set)?;
            if total.ceiling.is_some_and(|ceiling| ceiling.when_full == WhenFull::Refuse) {
                return Err(Error::invalid("a trim of a set whose ceiling refuses: its files are a user's"));
            }
            if total.size <= to {
                return Ok(((0, 0), Vec::new()));
            }
            let (evicted, freed) = change.evict(set, "", total.size - to)?;
            if let Some(counted) = &counted {
                change.count_evictions(counted, evicted);
            }
            Ok(((evicted, freed), change.freed))
        })
    }

    /// Writes the ceiling `wanted` when the set keeps what `expect` says, and
    /// evicts down to a lower one; says whether it wrote.
    pub(crate) fn change_ceiling(&self, wanted: Option<Ceiling>, expect: Expect) -> Result<bool> {
        #[cfg(feature = "replication")]
        if self.is_copy() {
            return self.pass_ceiling(wanted, expect);
        }
        let set = self.set;
        let written = self.blobs.file.write(0, move |tx| {
            let kept = kept_on(tx, set)?;
            expected(kept, expect)?;
            if kept == wanted {
                return Ok(false);
            }
            let max_size = wanted.map(|ceiling| i64::try_from(ceiling.max_size).unwrap_or(i64::MAX));
            let when_full = wanted.map(|ceiling| ceiling.when_full.as_str());
            tx.prepare_cached(SET_CEILING)
                .and_then(|mut update| update.execute(params![set, max_size, when_full]))
                .map_err(|error| sql_error("blobs: a set's ceiling", error))?;
            Ok(true)
        })?;
        if let Some(Ceiling { max_size, when_full: WhenFull::Evict }) = wanted {
            self.trimmed(max_size, Some(self.counted()))?;
        }
        Ok(written)
    }
}

/// A copy's trim and change of the ceiling, which its home makes.
#[cfg(feature = "replication")]
impl Files {
    fn forwarded_set(&self) -> super::forward::Set<'_> {
        super::forward::Set { name: &self.name, ttl: None, max_file_size: None }
    }

    fn pass_trim(&self, to: u64) -> Result<Trim> {
        super::forward::pass_trim(&self.blobs.store()?, &self.forwarded_set(), to)
    }

    fn pass_ceiling(&self, wanted: Option<Ceiling>, expect: Expect) -> Result<bool> {
        let call = super::forward::Call::Ceiling { wanted, expect };
        let written = super::forward::pass_on(&self.blobs.store()?, &self.forwarded_set(), &call)?;
        Ok(written)
    }
}

/// Refuses a change whose expected ceiling is not the one the set keeps.
fn expected(kept: Option<Ceiling>, expect: Expect) -> Result<()> {
    let kept_size = kept.map(|ceiling| ceiling.max_size);
    let held = match expect {
        Expect::Any => true,
        Expect::Absent => kept_size.is_none(),
        Expect::Is(current) => kept_size == Some(current),
    };
    if held {
        return Ok(());
    }
    let keeps = kept_size.map_or_else(|| "none".to_owned(), |size| format!("{size} bytes"));
    let named = match expect {
        Expect::Is(current) => format!("{current} bytes"),
        Expect::Any | Expect::Absent => "none".to_owned(),
    };
    Err(Error::new(ErrorKind::Conflict, format!("the set's ceiling is {keeps}, not the {named} expected")))
}

impl SetMaxSize<'_> {
    /// Changes the ceiling only while the set keeps `bytes`: another is a
    /// conflict, so that two deploys never silently undo each other.
    pub fn if_current(mut self, bytes: u64) -> Self {
        self.expect = Expect::Is(bytes);
        self
    }

    /// What a write past the new ceiling does; what the set does now when
    /// not given.
    pub fn when_full(mut self, when_full: WhenFull) -> Self {
        self.when_full = Some(when_full);
        self
    }

    /// Writes the ceiling, nothing when the set keeps it already.
    pub fn apply(self) -> Result<()> {
        let files = self.files;
        files.usable()?;
        let changed = (|| {
            let wanted = match self.max_size {
                None if self.when_full.is_some() => {
                    return Err(Error::invalid("when_full names what a ceiling does, and this removes it"));
                }
                None => None,
                Some(max_size) => {
                    let when_full = match self.when_full {
                        Some(when_full) => when_full,
                        None => {
                            let kept = files.blobs.file.read(|connection| kept_on(connection, files.set))?;
                            kept.map(|ceiling| ceiling.when_full).unwrap_or_default()
                        }
                    };
                    Some(Ceiling { max_size, when_full })
                }
            };
            files.change_ceiling(wanted, self.expect)
        })();
        changed.map(drop).map_err(|error| error.within(files.describe()))
    }
}

impl Blobs {
    /// Trims each evicting set its ceiling left past it, a lowered one or one a
    /// write could not bring under in its own evictions, a batch a commit and
    /// ten batches a pass; the files removed.
    pub(crate) fn trim_over(self: &Arc<Self>) -> Result<usize> {
        let over: Vec<(i64, String, i64)> = self.file.read(|connection| {
            rows_of(connection, (SELECT_OVER, "blobs: the sets past their ceiling"), [], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
        })?;
        let mut removed = 0;
        for (set, name, max_size) in over {
            let counted = Counted { set, name: Arc::from(name) };
            let to = max_size.unsigned_abs();
            removed += crate::engine::in_batches(|| {
                let (blobs, counted) = (Arc::clone(self), counted.clone());
                let (evicted, freed) = self.file.write(0, move |tx| {
                    let mut change = Change::new(tx, &blobs);
                    let total = change.total(set)?;
                    let to = i64::try_from(to).unwrap_or(i64::MAX);
                    if total.size <= to {
                        return Ok((0, Vec::new()));
                    }
                    let (evicted, _) = change.evict(set, "", total.size - to)?;
                    change.count_evictions(&counted, evicted);
                    Ok((evicted, change.freed))
                })?;
                self.remove_freed(&freed);
                Ok(evicted)
            })?;
        }
        Ok(removed)
    }
}

#[cfg(test)]
#[path = "cache_tests.rs"]
mod tests;
