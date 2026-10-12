//! What a name's values mean: its unit and a line of help, which belong to
//! the name rather than a series, so that a series deleted leaves them.
//!
//! ```no_run
//! # fn f(stats: tinystore::metrics::Metrics) -> tinystore::Result<()> {
//! use tinystore::metrics::Description;
//! stats.describe("cpu", Description { unit: "%".into(), help: "Busy time of a core.".into() })?;
//! let Description { unit, help } = stats.description("cpu")?;
//! # Ok(())
//! # }
//! ```
//!
//! `describe` replaces both, as the `metrics` crate's `describe_gauge!`
//! does; an instrument told a unit or a help writes what it was told at its
//! next write, and leaves what it was not told as it is.

use rusqlite::{OptionalExtension, params};

use super::Metrics;
use super::engine::Engine;
use super::instruments::{MOST_HELP, MOST_UNIT};
use super::labels::Kept;
use super::series::Labels;
use crate::sqlite::sql_error;
use crate::{Error, Result};

const DESCRIPTION: &str = "select unit, help from names where name = ?1";

/// A name's unit and help, empty when nothing gave them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Description {
    /// The unit of its values, 32 bytes at most: `ms`, `bytes`, `%`.
    pub unit: String,
    /// A line on what it means, 1024 bytes at most.
    pub help: String,
}

impl Metrics {
    /// Sets a name's unit and help in place of what it had; empty ones
    /// remove them.
    pub fn describe(&self, name: &str, description: Description) -> Result<()> {
        let described = checked(name, &description).and_then(|()| self.engine.describe(name, description));
        described.map_err(|error| error.within(format!("metrics {name}")))
    }

    /// What a name's unit and help are; both empty when nothing described
    /// it.
    pub fn description(&self, name: &str) -> Result<Description> {
        let read = checked(name, &Description::default()).and_then(|()| self.engine.description(name));
        read.map_err(|error| error.within(format!("metrics {name}")))
    }
}

impl Engine {
    fn describe(&self, name: &str, description: Description) -> Result<()> {
        self.names.describe(&self.file, name, description.unit, description.help)
    }

    fn description(&self, name: &str) -> Result<Description> {
        let failed = |error| sql_error("read a name's description", error);
        let found: Option<(String, String)> = self.file.read(|connection| {
            let mut select = connection.prepare_cached(DESCRIPTION).map_err(failed)?;
            select.query_row(params![name], |row| Ok((row.get(0)?, row.get(1)?))).optional().map_err(failed)
        })?;
        Ok(found.map(|(unit, help)| Description { unit, help }).unwrap_or_default())
    }
}

/// A name and a description within their bounds.
fn checked(name: &str, description: &Description) -> Result<()> {
    Kept::of(name, &Labels::new())?;
    if description.unit.len() > MOST_UNIT {
        return Err(Error::invalid(format!("a unit of {MOST_UNIT} bytes at most")));
    }
    if description.help.len() > MOST_HELP {
        return Err(Error::invalid(format!("a help of {MOST_HELP} bytes at most")));
    }
    Ok(())
}

#[cfg(test)]
#[path = "describe_tests.rs"]
mod tests;
