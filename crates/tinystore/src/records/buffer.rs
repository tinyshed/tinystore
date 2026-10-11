//! What a logger or a writer of another program's lines holds for its stream
//! until a flush writes it: every second on the store's background work,
//! sooner once half a buffer waits, at a `flush` and when the store closes.
//!
//! ```text
//! a line ─► its buffer, 1024 lines ─► records.db, an append a flush
//!             └─ full: dropped, counted, and said once in ten minutes
//! ```
//!
//! The engine holds every buffer until it is written: a logger or a writer
//! dropped with lines waiting loses none of them.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, UNIX_EPOCH};

use super::engine::{Records, Stream, nanos};
use super::event::{Fields, Level, NewEvent, SpanId, TraceId};
use super::lines::LineWriter;
use crate::console::{Console, Keep, Line, own_line, quoted};
use crate::engine::{Host, Wake};
use crate::{Clock, Error, Result, unix_nanos};

/// The lines a buffer holds between writes unless its logger says.
pub(super) const BUFFER: usize = 1024;
/// How often the store writes what its buffers hold.
const FLUSH: Duration = Duration::from_secs(1);
/// How long a buffer's drops go unsaid once it said some: a burst is one
/// line, and a buffer that keeps dropping says how many every ten minutes.
const QUIET: Duration = Duration::from_mins(10);

/// What a store's records hold for its loggers and its writers of lines,
/// which its background work writes.
#[derive(Default)]
pub(super) struct Held {
    buffers: Vec<Arc<Buffer>>,
    /// Writers whose last event may still grow, which a flush asks for it.
    writers: Vec<Weak<LineWriter>>,
    /// What runs the flush now: none until the first buffer opens.
    wake: Option<Wake>,
    /// The store closed: what is held was written, and no buffer opens.
    closed: bool,
}

/// The lines held for one stream until a flush writes them.
pub(super) struct Buffer {
    records: Weak<Records>,
    clock: Arc<dyn Clock>,
    stream: Stream,
    /// Where a logger's buffer says its drops; a writer of lines has no
    /// console, and its drops are said in the store's own log.
    console: Option<Arc<Console>>,
    most: usize,
    waiting: Mutex<Waiting>,
    /// Whether a flush was asked for since the last took what waited, so
    /// that a buffer half full wakes the flush once rather than a line.
    asked: AtomicBool,
    wake: Wake,
    /// Held by a flush, so that two never write one buffer's lines out of
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

/// What a buffer last said of its drops: how many a full buffer had dropped
/// then, and when.
#[derive(Default)]
struct Said {
    full: u64,
    at: Option<i64>,
}

impl Buffer {
    /// A buffer of `most` lines for `stream`, which the store writes from now
    /// on.
    pub(super) fn open(
        records: &Arc<Records>,
        stream: Stream,
        most: usize,
        console: Option<Arc<Console>>,
    ) -> Result<Arc<Buffer>> {
        let buffer = Arc::new(Buffer {
            records: Arc::downgrade(records),
            clock: records.clock(),
            stream,
            console,
            most,
            waiting: Mutex::default(),
            asked: AtomicBool::new(false),
            wake: records.held_wake()?,
            flushing: Mutex::default(),
            dropped: AtomicU64::new(0),
            full: AtomicU64::new(0),
        });
        records.lock_held().buffers.push(Arc::clone(&buffer));
        Ok(buffer)
    }

    /// Holds an event a writer of lines hands over at a flush or as it
    /// closes, even past the buffer's bound: two at most, the event its last
    /// line ended and the one it was joining, which the flush then writes.
    pub(super) fn keep_past_bound(&self, line: Line) {
        let mut waiting = lock(&self.waiting);
        if waiting.closed {
            drop(waiting);
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        waiting.lines.push(line);
    }

    /// Counts a line dropped before it reached the buffer: one longer than
    /// an event holds.
    pub(super) fn count_dropped(&self) {
        self.dropped.fetch_add(1, Ordering::Relaxed);
    }

    /// Writes what waits, and says the drops due. `closing` refuses every
    /// line after it, for the store's close.
    pub(super) fn write_waiting(&self, closing: bool) -> Result<()> {
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
            return Err(Error::closed(format!("events {}", self.stream.name)));
        };
        let (lost, written) = records.append_held(&self.stream, lines.into_iter().map(event_of).collect());
        self.dropped.fetch_add(lost, Ordering::Relaxed);
        written.map_err(|error| error.within(format!("events {}", self.stream.name)))
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
        let (stream, dropped) = (&self.stream.name, full - said.full);
        match &self.console {
            Some(console) => console.say(&Line {
                level: Some(4),
                message: Some("log lines dropped".to_owned()),
                fields: vec![
                    // logger, since the line's own stream is the store's
                    ("logger".to_owned(), quoted(stream)),
                    ("dropped".to_owned(), dropped.to_string()),
                    ("buffer".to_owned(), self.most.to_string()),
                ],
                ..own_line(now)
            }),
            None => tracing::warn!(target: "tinystore", %stream, dropped, buffer = self.most, "lines dropped"),
        }
        (said.full, said.at) = (full, Some(now));
    }
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
    /// What runs the flush of every buffer now, the flush every second added
    /// with the first buffer.
    fn held_wake(self: &Arc<Self>) -> Result<Wake> {
        let mut held = self.lock_held();
        if held.closed {
            return Err(Error::closed("records"));
        }
        if let Some(wake) = &held.wake {
            return Ok(wake.clone());
        }
        let store = self.store().ok_or_else(|| Error::closed("records"))?;
        let weak = Arc::downgrade(self);
        let wake = store.every("records: loggers and lines", FLUSH, move || {
            weak.upgrade().map_or(Ok(()), |records| records.flush_held(false))
        })?;
        held.wake = Some(wake.clone());
        Ok(wake)
    }

    /// Asks `writer` at every flush for an event that stopped growing.
    pub(super) fn add_writer(&self, writer: &Arc<LineWriter>) {
        self.lock_held().writers.push(Arc::downgrade(writer));
    }

    /// Writes what every buffer holds, once each writer of lines handed over
    /// an event that did not grow since the flush before; `closing` hands over
    /// all of them and refuses every line from then on. A failure is the
    /// first buffer's that failed: the others still write. A buffer that no
    /// logger or writer holds any more goes once it is written.
    pub(super) fn flush_held(&self, closing: bool) -> Result<()> {
        let (writers, buffers): (Vec<Arc<LineWriter>>, Vec<Arc<Buffer>>) = {
            let mut held = self.lock_held();
            held.closed |= closing;
            held.writers.retain(|writer| writer.strong_count() > 0);
            (held.writers.iter().filter_map(Weak::upgrade).collect(), held.buffers.clone())
        };
        for writer in writers {
            if closing { writer.close() } else { writer.hand_over_idle() }
        }
        let mut flushed = Ok(());
        for buffer in &buffers {
            flushed = flushed.and(buffer.write_waiting(closing));
        }
        drop(buffers);
        self.lock_held().buffers.retain(|buffer| Arc::strong_count(buffer) > 1);
        flushed
    }

    fn lock_held(&self) -> MutexGuard<'_, Held> {
        self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
