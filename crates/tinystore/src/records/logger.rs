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
