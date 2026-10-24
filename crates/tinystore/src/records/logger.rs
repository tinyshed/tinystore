//! A store's logger: a `tracing` layer that prints each line as it is logged
//! and keeps it in its stream through a buffer of its own, which the store
//! writes every second.
//!
//! ```text
//! tracing::info!(…) ─► printed ─► its buffer ─► records.db
//! ```

use std::fmt;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use super::buffer::{BUFFER, Buffer};
use super::engine::Records;
use crate::console::{Console, Layer, Line, Told, own_line, told_options};
use crate::{Error, Result, Store, unix_nanos};

/// The least levels a store's logger keeps and prints, each part left out as
/// the option is: no `store` keeps every level, and no `console` prints what
/// is kept, following the config `log`'s changes of it. One `tracing::Level`
/// is the `store` part alone, so it sets both.
///
/// ```text
/// Level::INFO                                                      info+ kept and printed
/// Levels { store: Some(Level::INFO), console: Some(Level::WARN) }  info+ kept, warn+ printed
/// Levels { console: Some(Level::WARN), ..Levels::default() }       all kept, warn+ printed
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Levels {
    /// The least level kept; `LOG_LEVEL` and the config `log` win over it.
    pub store: Option<tracing::Level>,
    /// The least level printed; `LOG_CONSOLE_LEVEL` wins over it.
    pub console: Option<tracing::Level>,
}

impl From<tracing::Level> for Levels {
    fn from(level: tracing::Level) -> Levels {
        Levels { store: Some(level), console: None }
    }
}

/// What opens a logger: its stream, how it prints, the lines it holds.
pub struct LoggerBuilder<'s> {
    store: &'s Store,
    stream: String,
    told: Told,
    buffer: usize,
    keep: Option<Duration>,
}

impl fmt::Debug for LoggerBuilder<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoggerBuilder").field("stream", &self.stream).field("buffer", &self.buffer).finish()
    }
}

impl<'s> LoggerBuilder<'s> {
    pub(crate) fn new(store: &'s Store, stream: &str) -> Self {
        LoggerBuilder { store, stream: stream.to_owned(), told: Told::default(), buffer: BUFFER, keep: None }
    }

    /// The least level kept and printed, every level without it, or each
    /// apart as `Levels` says. `LOG_LEVEL` wins over the level kept, and
    /// `LOG_CONSOLE_LEVEL` over the level printed.
    ///
    /// ```no_run
    /// # fn main() -> tinystore::Result<()> {
    /// # let store = tinystore::Store::open("data", Default::default())?;
    /// use tinystore::records::Levels;
    /// use tracing::Level;
    ///
    /// let both = store.logger("api").level(Level::INFO).layer()?;
    /// // every line from info kept, warnings and errors printed
    /// let levels = Levels { store: Some(Level::INFO), console: Some(Level::WARN) };
    /// let quiet = store.logger("jobs").level(levels).layer()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn level(mut self, level: impl Into<Levels>) -> Self {
        let levels = level.into();
        self.told.level = levels.store;
        self.told.printed = levels.console;
        self
    }

    told_options!();

    /// The lines held between writes, 1024 unless given: a line that finds
    /// them all held is dropped and counted.
    pub fn buffer(mut self, lines: usize) -> Self {
        self.buffer = lines;
        self
    }

    /// How long the stream keeps its events, as `store.events` takes it.
    pub fn keep(mut self, keep: Duration) -> Self {
        self.keep = Some(keep);
        self
    }

    /// The layer that prints each line `tracing` hands it and keeps it in
    /// the stream within a second.
    pub fn layer(self) -> Result<Layer> {
        let shown = format!("logger {}", self.stream);
        if self.buffer == 0 {
            return Err(Error::invalid("a buffer of no lines").within(shown));
        }
        let records = Records::of(self.store)?;
        let stream = records.open_stream(&self.stream, self.keep).map_err(|error| error.within(&shown))?;
        let console = Arc::new(Console::new(&self.stream, self.told));
        say_unheard_warnings(&console);
        #[cfg(feature = "config")]
        let following = crate::log_config::follow(self.store, &console).map_err(|error| error.within(&shown))?;
        let buffer = Buffer::open(&records, stream, self.buffer, Some(Arc::clone(&console)))?;
        let layer = Layer::new(console, Some(buffer));
        #[cfg(feature = "config")]
        let layer = layer.following(following);
        Ok(layer)
    }
}

/// Prints what the store warned of before anyone listened, a slow SQLite
/// build: the store opens, and its first connection with it, before the
/// subscriber that holds this logger is set.
fn say_unheard_warnings(console: &Console) {
    for warning in crate::sqlite::unheard_warnings() {
        console.say(&Line {
            level: Some(4),
            message: Some((*warning).to_owned()),
            ..own_line(unix_nanos(SystemTime::now()))
        });
    }
}

#[cfg(test)]
#[path = "logger_tests.rs"]
mod tests;
