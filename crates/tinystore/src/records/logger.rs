//! A store's logger: a `tracing` layer that prints each line as it is logged
//! and keeps it in its stream, through a buffer the store's background work
//! writes every second, sooner once half of it waits, and when the store
//! closes.
//!
//! ```text
//! tracing::info!(…) ─► printed ─► its buffer, 1024 lines ─► records.db, an append a flush
//!                                   └─ full: dropped, counted, and said once in ten minutes
//! ```

use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, UNIX_EPOCH};

use super::engine::{Records, Stream, nanos};
use super::event::{Fields, Level, NewEvent, SpanId, TraceId};
use crate::console::{Console, Keep, Layer, Line, Told, own_line, quoted, told_options};
use crate::engine::{Host, Wake};
use crate::{Clock, Error, Result, Store, unix_nanos};

/// The lines a logger holds between writes unless its builder says.
const BUFFER: usize = 1024;
/// How often the store writes what its loggers hold.
const FLUSH: Duration = Duration::from_secs(1);
/// How long a logger's drops go unsaid once it said some: a burst is one
/// line, and a logger that keeps dropping says how many every ten minutes.
const QUIET: Duration = Duration::from_mins(10);

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
        let wake = records.loggers_wake(self.store)?;
        let console = Arc::new(Console::new(&self.stream, self.told));
        let buffer = Arc::new(Buffer {
            records: Arc::downgrade(&records),
            clock: self.store.clock(),
            stream,
            console: Arc::clone(&console),
            most: self.buffer,
            waiting: Mutex::default(),
            asked: AtomicBool::new(false),
            wake,
            flushing: Mutex::default(),
            dropped: AtomicU64::new(0),
            full: AtomicU64::new(0),
        });
        records.lock_loggers().buffers.push(Arc::downgrade(&buffer));
        Ok(Layer::new(console, Some(buffer)))
    }
}

/// The loggers of a store's records, which its background work flushes.
#[derive(Default)]
pub(super) struct Loggers {
    buffers: Vec<Weak<Buffer>>,
    /// What runs their flush now: none until the first logger opens.
    wake: Option<Wake>,
}

/// The lines a logger holds for its stream until a flush writes them.
struct Buffer {
    records: Weak<Records>,
    clock: Arc<dyn Clock>,
    stream: Stream,
    console: Arc<Console>,
    most: usize,
    waiting: Mutex<Waiting>,
    /// Whether a flush was asked for since the last took what waited, so
    /// that a buffer half full wakes the flush once rather than a line.
    asked: AtomicBool,
    wake: Wake,
    /// Held by a flush, so that two never write one logger's lines out of
    /// their order; with what it last said of the drops.
    flushing: Mutex<Said>,
    dropped: AtomicU64,
    /// Of those, the lines a full buffer dropped, which a flush says.
    full: AtomicU64,
}

#[derive(Default)]
struct Waiting {
    lines: Vec<Line>,
    /// The store closed: no flush comes, and a line is dropped.
    closed: bool,
}

/// What a logger last said of its drops: how many a full buffer had dropped
/// then, and when.
#[derive(Default)]
struct Said {
    full: u64,
    at: Option<i64>,
}

impl Keep for Buffer {
    fn keep(&self, line: Line) {
        let mut waiting = lock(&self.waiting);
        if waiting.closed || waiting.lines.len() >= self.most {
            let full = !waiting.closed;
            drop(waiting);
            self.dropped.fetch_add(1, Ordering::Relaxed);
            self.full.fetch_add(u64::from(full), Ordering::Relaxed);
            return;
        }
        waiting.lines.push(line);
        let half = waiting.lines.len() * 2 >= self.most;
        drop(waiting);
        if half && !self.asked.swap(true, Ordering::AcqRel) {
            self.wake.wake();
        }
    }

    fn now(&self) -> i64 {
        unix_nanos(self.clock.now())
    }

    fn flush(&self) {
        // what a write could not keep is counted, as a logger's every loss is
        let _ = self.write_waiting(false);
    }

    fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

impl Buffer {
    /// Writes what waits, and says the drops due. `closing` refuses every
    /// line after it, for the store's close.
    fn write_waiting(&self, closing: bool) -> Result<()> {
        let mut said = lock(&self.flushing);
        let lines = {
            let mut waiting = lock(&self.waiting);
            waiting.closed |= closing;
            self.asked.store(false, Ordering::Release);
            std::mem::take(&mut waiting.lines)
        };
        let written = self.append(lines);
        self.say_drops(&mut said);
        written
    }

    fn append(&self, lines: Vec<Line>) -> Result<()> {
        if lines.is_empty() {
            return Ok(());
        }
        let Some(records) = self.records.upgrade() else {
            self.dropped.fetch_add(lines.len() as u64, Ordering::Relaxed);
            return Err(Error::closed(format!("logger {}", self.stream.name)));
        };
        let (lost, written) = records.append_held(&self.stream, lines.into_iter().map(event_of).collect());
        self.dropped.fetch_add(lost, Ordering::Relaxed);
        written.map_err(|error| error.within(format!("logger {}", self.stream.name)))
    }

    /// Says how many lines a full buffer dropped since it last said so, at
    /// most once in ten minutes, as the Go, Bun and Python loggers say
    /// theirs. A failed write is background work failing, which the store's
    /// own log says.
    fn say_drops(&self, said: &mut Said) {
        let (full, now) = (self.full.load(Ordering::Relaxed), self.now());
        if full == said.full || said.at.is_some_and(|at| now.saturating_sub(at) < nanos(QUIET)) {
            return;
        }
        self.console.say(&Line {
            level: Some(4),
            message: Some("log lines dropped".to_owned()),
            fields: vec![
                // logger, since the line's own stream is the store's
                ("logger".to_owned(), quoted(&self.stream.name)),
                ("dropped".to_owned(), (full - said.full).to_string()),
                ("buffer".to_owned(), self.most.to_string()),
            ],
            ..own_line(now)
        });
        (said.full, said.at) = (full, Some(now));
    }
}

/// A line as an event of its stream.
fn event_of(line: Line) -> NewEvent {
    NewEvent {
        name: line.name,
        at: Some(UNIX_EPOCH + Duration::from_nanos(line.at.max(0) as u64)),
        level: line.level.map(Level::of),
        message: line.message,
        fields: Fields::from_pairs(line.fields),
        context: Fields::from_pairs(line.context),
        trace_id: line.trace.map(TraceId),
        span_id: line.span.map(SpanId),
        refused: None,
    }
}

impl Records {
    /// What runs the loggers' flush now, the flush every second added with
    /// the first logger.
    fn loggers_wake(self: &Arc<Self>, store: &Store) -> Result<Wake> {
        let mut loggers = self.lock_loggers();
        if let Some(wake) = &loggers.wake {
            return Ok(wake.clone());
        }
        let weak = Arc::downgrade(self);
        let wake = store.every("records: loggers", FLUSH, move || {
            weak.upgrade().map_or(Ok(()), |records| records.flush_loggers(false))
        })?;
        loggers.wake = Some(wake.clone());
        Ok(wake)
    }

    /// Writes what every logger holds; `closing` refuses their lines from
    /// then on. A failure is the first logger's that failed: the others
    /// still write.
    pub(super) fn flush_loggers(&self, closing: bool) -> Result<()> {
        let buffers: Vec<Arc<Buffer>> = {
            let mut loggers = self.lock_loggers();
            loggers.buffers.retain(|buffer| buffer.strong_count() > 0);
            loggers.buffers.iter().filter_map(Weak::upgrade).collect()
        };
        let mut flushed = Ok(());
        for buffer in buffers {
            let written = buffer.write_waiting(closing);
            flushed = flushed.and(written);
        }
        flushed
    }

    fn lock_loggers(&self) -> MutexGuard<'_, Loggers> {
        self.loggers.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[path = "logger_tests.rs"]
mod tests;
