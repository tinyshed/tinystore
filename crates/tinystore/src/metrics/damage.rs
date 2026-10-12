//! What no longer reads: a series whose bytes changed under it, met by a
//! read, by packing or by publication. The series is suspended, said once: it
//! takes no sample, packing and expiry pass it by, and what reads over the
//! bytes that changed fails `corrupt` till `delete` removes it. Its
//! suspension is kept in the file, so `damaged` lists it after a restart too.
//!
//! ```text
//! a read     metrics cpu{host="web-1"}: a block from 1800000000000: its checksum: corrupt
//! ingest     metrics cpu{host="web-1"}: it stopped taking samples: …: suspended
//! damaged    cpu{host="web-1"}, and what broke
//! delete     the series gone with every sample it held
//! ```

use std::fmt;

use rusqlite::params;

use super::Metrics;
use super::budget::Budget;
use super::engine::{Engine, execute};
use super::matching::{Found, Row, labelled};
use super::series::Labels;
use crate::sqlite::sql_error;
use crate::{Error, ErrorKind, Result};

/// The damaged series one answer gives at most; deleting them shows the
/// next.
const MOST_DAMAGED: i64 = 1000;
/// What a failure keeps of its error.
const MOST_FAILURE: usize = 1024;

const SUSPEND: &str = "update series_state set failed_at = ?2, failure = ?3 where series_id = ?1 and failed_at is null";
const DAMAGED: &str = "select s.id, n.kind, s.label_ids, st.failure from series_state st
    join series s on s.id = st.series_id join names n on n.id = s.name
    where st.failed_at is not null order by st.series_id limit cast(?1 as integer)";

/// A series whose bytes no longer read, as `damaged` lists it and `delete`
/// takes it.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Damage {
    pub name: String,
    pub labels: Labels,
    /// What broke, as the call that met it said.
    pub error: String,
}

impl fmt::Display for Damage {
    /// As Prometheus prints the series: `cpu{host="web-1"}`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        super::labels::format(f, &self.name, self.labels.iter())
    }
}

impl Metrics {
    /// The series whose bytes no longer read, which a read, packing or
    /// publication met: 1000 at most, the first the file holds, each with
    /// what broke. Deleting them shows the next.
    pub fn damaged(&self) -> Result<Vec<Damage>> {
        self.engine.damaged().map_err(|error| error.within("metrics"))
    }
}

impl Engine {
    /// Stops a series taking samples, with what broke, said once; its
    /// neighbours go on.
    pub(super) fn suspend(&self, id: i64, failure: &str) -> Result<u64> {
        let now = self.now();
        let failure: String = failure.chars().take(MOST_FAILURE).collect();
        let reason = failure.clone();
        let changed =
            self.file.write(0, move |tx| execute(tx, SUSPEND, params![id, now, reason], "suspend a series"))?;
        if changed == 1 {
            tracing::warn!(target: "tinystore", series = id, failure, "metrics: a series stopped taking samples");
        }
        Ok(changed as u64)
    }

    /// Names an error a read met in a series as the series, and suspends the
    /// series when its bytes no longer read, so that `damaged` lists it. A
    /// read calls this once its snapshot has ended, since a suspension
    /// writes.
    pub(super) fn met(&self, found: &Found, error: Error) -> Error {
        if error.kind() == ErrorKind::Corrupt {
            // the read's own error is what its caller needs: a suspension
            // that fails is made by the next call that meets the bytes
            self.suspend(found.id, &error.to_string()).ok();
        }
        error.within(&found.kept)
    }

    fn damaged(&self) -> Result<Vec<Damage>> {
        let failed = |error| sql_error("read the damaged series", error);
        let (found, failures) = self.file.read(|connection| {
            let mut select = connection.prepare_cached(DAMAGED).map_err(failed)?;
            let rows = select
                .query_map(params![MOST_DAMAGED], |row| Ok(((row.get(0)?, row.get(1)?, row.get(2)?), row.get(3)?)));
            let rows: Vec<(Row, Option<String>)> = rows.and_then(Iterator::collect).map_err(failed)?;
            let (rows, failures): (Vec<Row>, Vec<Option<String>>) = rows.into_iter().unzip();
            Ok((labelled(connection, rows, &mut Budget::default())?, failures))
        })?;
        let damaged = found.into_iter().zip(failures).map(|(found, failure)| Damage {
            name: found.kept.name().to_owned(),
            labels: found.kept.labels(),
            error: failure.unwrap_or_default(),
        });
        Ok(damaged.collect())
    }
}

#[cfg(test)]
#[path = "damage_tests.rs"]
mod tests;
