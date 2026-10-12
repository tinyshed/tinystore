//! `delete`: one series removed with every sample it holds, whether they
//! still read or not, in one write.
//!
//! ```text
//! its identity ─► its head ─► each group, the payloads its directory names, its clock let go
//!   ─► the series as expiry reclaims one ─► its name's newest time found again when it was this series'
//! ```
//!
//! Only a directory that reads proves which payloads are its group's: a group
//! whose directory or clock no longer reads goes without them, and they stay
//! in the file.

use rusqlite::{OptionalExtension, params};

use super::Metrics;
use super::clock;
use super::engine::{Engine, execute, lock};
use super::labels::{Kept, format};
use super::publish::read_group;
use super::retention::reclaim;
use super::series::Labels;
use crate::compress::frame::Zstd;
use crate::sqlite::{Tx, sql_error};
use crate::{ErrorKind, Result};

const FIND: &str = "select s.id, s.name, st.max_seen from series s
    left join series_state st on st.series_id = s.id where s.identity = ?1";
const GROUPS: &str = "select id, start_ts, end_ts, clock, directory from groups where series_id = ?1 order by start_ts";
const DROP_PAYLOAD: &str = "delete from payloads where id = ?1";
const DROP_GROUP: &str = "delete from groups where id = ?1";
/// A name whose newest time was the series' finds it again among its other
/// series, a walk of them that only a delete of the newest series makes.
const RENEW_NEWEST: &str = "update names set newest = (select max(max_seen) from series_state where name = ?1)
    where id = ?1 and newest = ?2 and not exists (select 1 from series_state where name = ?1 and max_seen = ?2)";

/// What a delete found.
#[derive(Clone, Copy, Debug, Default)]
struct Deleted {
    found: bool,
    /// Groups whose directory or clock no longer read, whose payloads stay.
    unreadable: u64,
}

impl Metrics {
    /// Removes the one series of exactly this name and these labels, no more
    /// and no fewer, with every sample it holds, whether they still read or
    /// not, and says whether the store held it: `delete("cpu", [("host",
    /// "web-1")])`. Writing the same labels later starts a new series; the
    /// name keeps its `keep`, unit and help.
    pub fn delete<K: AsRef<str>, V: AsRef<str>>(
        &self,
        name: &str,
        labels: impl IntoIterator<Item = (K, V)>,
    ) -> Result<bool> {
        let labels: Labels =
            labels.into_iter().map(|(label, value)| (label.as_ref().to_owned(), value.as_ref().to_owned())).collect();
        let deleted = Kept::held(name, &labels).and_then(|kept| self.engine.delete(&kept));
        deleted.map_err(|error| {
            let mut series = String::new();
            format(&mut series, name, labels.iter()).ok();
            error.within(format!("metrics {series}"))
        })
    }
}

impl Engine {
    /// Removes a series under maintenance's lock, so that no pass packs or
    /// expires it meanwhile.
    fn delete(&self, kept: &Kept) -> Result<bool> {
        let _one = lock(&self.maintaining);
        let identity = kept.identity();
        let zstd = self.codecs.take();
        let (zstd, deleted) = self.file.write(0, move |tx| {
            let mut zstd = zstd;
            let deleted = remove(tx, &mut zstd, identity)?;
            Ok((zstd, deleted))
        })?;
        self.codecs.give_back(zstd);
        if deleted.unreadable > 0 {
            tracing::warn!(
                target: "tinystore",
                groups = deleted.unreadable,
                "metrics: {kept} deleted, the values of its groups that no longer read left in the file"
            );
        }
        Ok(deleted.found)
    }
}

/// Removes the series of `identity` and everything it holds.
fn remove(tx: &Tx<'_>, zstd: &mut Zstd, identity: [u8; 32]) -> Result<Deleted> {
    let found = tx
        .prepare_cached(FIND)
        .and_then(|mut find| {
            find.query_row(params![identity], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get(2)?)))
                .optional()
        })
        .map_err(|error| sql_error("find a series", error))?;
    let Some((id, name, newest)): Option<(i64, i64, Option<i64>)> = found else { return Ok(Deleted::default()) };
    let unreadable = remove_groups(tx, zstd, id)?;
    reclaim(tx, id)?;
    execute(tx, RENEW_NEWEST, params![name, newest], "find a name's newest sample again")?;
    Ok(Deleted { found: true, unreadable })
}

/// Removes every group of a series, with the payloads a directory that
/// reads names, its clock let go; says how many did not read.
fn remove_groups(tx: &Tx<'_>, zstd: &mut Zstd, id: i64) -> Result<u64> {
    let failed = |error| sql_error("read a series' groups", error);
    let rows: Vec<(i64, super::directory::Row)> = {
        let mut select = tx.prepare_cached(GROUPS).map_err(failed)?;
        let rows = select.query_map(params![id], |row| {
            let found = super::directory::Row {
                series: id,
                start: row.get(1)?,
                end: row.get(2)?,
                clock: row.get(3)?,
                directory: row.get(4)?,
            };
            Ok((row.get(0)?, found))
        });
        rows.and_then(Iterator::collect).map_err(failed)?
    };
    let mut unreadable = 0;
    for (row_id, row) in rows {
        match read_group(tx, zstd, &row) {
            Ok(group) => {
                for block in group.blocks.iter().filter(|block| block.external()) {
                    // a payload expiry took with its block needs no removing
                    execute(tx, DROP_PAYLOAD, params![block.payload], "remove a block's values")?;
                }
            }
            // a clock missing from the file reads as no row
            Err(error) if matches!(error.kind(), ErrorKind::Corrupt | ErrorKind::NotFound) => unreadable += 1,
            Err(error) => return Err(error),
        }
        execute(tx, DROP_GROUP, params![row_id], "remove a group")?;
        match clock::let_go(tx, row.clock) {
            Err(error) if error.kind() != ErrorKind::Corrupt => return Err(error),
            _ => {}
        }
    }
    Ok(unreadable)
}

#[cfg(test)]
#[path = "delete_tests.rs"]
mod tests;
