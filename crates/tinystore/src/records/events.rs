//! A stream of events as a program opens it, `store.events("audit")`, and
//! every stream as it reads them, `store.all_events()`.

use std::fmt;
use std::sync::{Arc, PoisonError};
use std::time::{Duration, SystemTime};

use rusqlite::params;
use serde::Serialize;

use super::engine::{AHEAD, LATENESS, Records, Stream, execute, nanos};
use super::event::Kept;
use super::event::{Level, MAX_EVENT, MAX_KEYS, NewEvent, SpanId, TraceId};
use super::head::{self, ROW_EVENTS, ROW_WEIGHT, Row};
use super::read::List;
use crate::{Error, ErrorKind, Result, Store, unix_nanos};

/// What one append carries at most: more is sent in pieces.
const MAX_APPEND: usize = 4 << 20;
/// How long an append waits for the store's memory.
const PATIENCE: Duration = Duration::from_secs(10);

/// What opens a stream of events: its name, and how long it keeps them.
#[derive(Debug)]
pub struct EventsBuilder<'s> {
    store: &'s Store,
    stream: String,
    keep: Option<Duration>,
}

impl<'s> EventsBuilder<'s> {
    pub(crate) fn new(store: &'s Store, stream: &str) -> Self {
        EventsBuilder { store, stream: stream.to_owned(), keep: None }
    }

    /// How long the stream keeps its events. It is the stream's own, kept in
    /// the file: an open that says none leaves it, fourteen days until one
    /// says, and another than an earlier open of this store said is
    /// `invalid`.
    pub fn keep(mut self, keep: Duration) -> Self {
        self.keep = Some(keep);
        self
    }

    pub fn open(self) -> Result<Events> {
        let records = Records::of(self.store)?;
        let opened = records.open_stream(&self.stream, self.keep);
        let stream = opened.map_err(|error| error.within(format!("events {}", self.stream)))?;
        Ok(Events { records, stream })
    }
}

/// A stream of events: written with `append`, which waits for the file, and
/// read back by time and by what they hold.
#[derive(Clone)]
pub struct Events {
    pub(super) records: Arc<Records>,
    pub(super) stream: Stream,
}

impl fmt::Debug for Events {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Events({})", self.stream.name)
    }
}

impl Events {
    /// Writes an event of `name` with `fields`, a struct or a map, at the
    /// store's time. It returns once the event is written, and a read finds
    /// it at once.
    pub fn append(&self, name: &str, fields: &impl Serialize) -> Result<()> {
        self.event(name).append(fields)
    }

    /// An append of one event, what it carries beside its fields before its
    /// verb:
    ///
    /// ```no_run
    /// # fn f(audit: tinystore::records::Events, at: std::time::SystemTime) -> tinystore::Result<()> {
    /// let (actor, change) = (serde_json::json!({ "actor": 7 }), serde_json::json!({ "user": 42 }));
    /// audit.event("role.changed").at(at).context(&actor).append(&change)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn event(&self, name: &str) -> EventCall<'_> {
        EventCall { events: self, event: NewEvent::new(name) }
    }

    /// Writes a list of events, all of them or none: a refused one names its
    /// place in the list and its name.
    pub fn append_all(&self, events: impl IntoIterator<Item = NewEvent>) -> Result<()> {
        let appended = self.records.append(&self.stream, events.into_iter().collect());
        appended.map_err(|error| error.within(format!("events {}", self.stream.name)))
    }

    /// The stream's events, found by the conditions the answer adds.
    pub fn list(&self) -> List {
        List::of(Arc::clone(&self.records), Some(self.stream.clone()))
    }
}

/// One event on its way into a stream.
#[derive(Debug)]
pub struct EventCall<'e> {
    events: &'e Events,
    event: NewEvent,
}

impl EventCall<'_> {
    /// Its time, from as long ago as the stream keeps to ten minutes ahead
    /// of the store's clock.
    pub fn at(mut self, at: SystemTime) -> Self {
        self.event = self.event.at(at);
        self
    }

    pub fn level(mut self, level: Level) -> Self {
        self.event = self.event.level(level);
        self
    }

    pub fn message(mut self, message: &str) -> Self {
        self.event = self.event.message(message);
        self
    }

    /// What it happened in: who asked, which request.
    pub fn context(mut self, context: &impl Serialize) -> Self {
        self.event = self.event.context(context);
        self
    }

    pub fn trace(mut self, trace_id: TraceId, span_id: SpanId) -> Self {
        self.event = self.event.trace(trace_id, span_id);
        self
    }

    pub fn append(self, fields: &impl Serialize) -> Result<()> {
        self.events.append_all([self.event.fields(fields)])
    }
}

/// Every stream's events, to read.
#[derive(Clone)]
pub struct AllEvents {
    pub(super) records: Arc<Records>,
}

impl fmt::Debug for AllEvents {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AllEvents")
    }
}

impl AllEvents {
    /// The events of every stream, or of those `streams` names, found by
    /// the conditions the answer adds.
    pub fn list(&self) -> List {
        List::of(Arc::clone(&self.records), None)
    }
}

const INSERT_HEAD: &str = "insert into heads
    (stream, late, first_at, last_at, levels, count, weight, size, written_at, body)
    values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)";
/// What a head holds grows by a row's events; since stays when its oldest
/// row was written.
const ADD_TO_HEAD: &str = "insert into head_state (stream, late, count, weight, since) values (?1, ?2, ?3, ?4, ?5)
    on conflict (stream, late) do update set count = count + excluded.count, weight = weight + excluded.weight";

impl Records {
    /// Appends `events` to a stream, all of them or none: check each, then
    /// write them in one commit.
    fn append(&self, stream: &Stream, events: Vec<NewEvent>) -> Result<()> {
        let now = self.now();
        let window = window(stream, now);
        let mut checked = Vec::with_capacity(events.len());
        for (place, event) in events.into_iter().enumerate() {
            let named = format!("event {place} {:?}", event.name);
            checked.push(check(event, now, window).map_err(|error| error.within(named))?);
        }
        let weight: usize = checked.iter().map(Kept::weight).sum();
        if weight > MAX_APPEND {
            return Err(Error::limit(format!("an append of {weight} bytes, past the {MAX_APPEND} one carries")));
        }
        self.write_checked(stream, checked, now)
    }

    /// Appends what a logger held: an event the file cannot keep is left out,
    /// and the rest written an append's worth at a time. It gives how many
    /// were lost, those left out and what a failed write and the writes after
    /// it held, with the failure.
    pub(super) fn append_held(&self, stream: &Stream, events: Vec<NewEvent>) -> (u64, Result<()>) {
        let now = self.now();
        let window = window(stream, now);
        let (mut lost, mut pieces, mut piece, mut weight) = (0, Vec::new(), Vec::new(), 0);
        for event in events {
            let Ok(event) = check(event, now, window) else {
                lost += 1;
                continue;
            };
            if weight + event.weight() > MAX_APPEND {
                pieces.push(std::mem::take(&mut piece));
                weight = 0;
            }
            weight += event.weight();
            piece.push(event);
        }
        pieces.push(piece);
        let mut left: u64 = pieces.iter().map(|piece| piece.len() as u64).sum();
        for piece in pieces {
            let count = piece.len() as u64;
            if let Err(error) = self.write_checked(stream, piece, now) {
                return (lost + left, Err(error));
            }
            left -= count;
        }
        (lost, Ok(()))
    }

    /// Writes events already checked in one commit: reserve → part the late
    /// from those on time → cut each into rows → write.
    fn write_checked(&self, stream: &Stream, checked: Vec<Kept>, now: i64) -> Result<()> {
        if checked.is_empty() {
            return Ok(());
        }
        let weight: usize = checked.iter().map(Kept::weight).sum();
        let _held = self.memory.reserve(weight as u64, "records: an append", PATIENCE)?;
        let (on_time, late) = self.part_late(stream.id, checked, now);
        let mut rows = Vec::new();
        for (late, events) in [(false, &on_time), (true, &late)].into_iter().filter(|(_, events)| !events.is_empty()) {
            rows.extend(self.rows_of(events)?.into_iter().map(|row| (late, row)));
        }
        let bytes = rows.iter().map(|(_, row)| row.body.len()).sum();
        let name = Arc::clone(&stream.name);
        let stream = stream.id;
        self.file.write(bytes, move |tx| {
            rows.iter().try_for_each(|(late, row)| {
                let (count, weight, size) = (row.count as i64, row.weight as i64, row.body.len() as i64);
                let head = params![stream, late, row.first, row.last, row.levels, count, weight, size, now, row.body];
                execute(tx, INSERT_HEAD, head, "write a head row")?;
                execute(tx, ADD_TO_HEAD, params![stream, late, count, weight, now], "count what a head holds").map(drop)
            })
        })?;
        self.recent.add(&name, on_time.iter().chain(&late));
        if let Some(newest) = on_time.iter().map(|event| event.at).max() {
            let mut waiting = self.waiting.lock().unwrap_or_else(PoisonError::into_inner);
            waiting.entry(stream).and_modify(|waits| *waits = newest.max(*waits)).or_insert(newest);
        }
        Ok(())
    }

    /// `events` parted into those on time and the late, each in the order
    /// they came. An event is late when it comes more than ten seconds behind
    /// the newest its stream showed on time before it, earlier in this
    /// append or waiting in its head, or behind the store's clock where that
    /// is earlier; so a producer ahead of the clock makes no one late:
    ///
    /// ```text
    /// clock 12:01, waiting until 12:00:40; append 12:00:35 12:00:41 11:50:02 12:00:45
    ///                                       → on time 12:00:35 12:00:41 12:00:45, late 11:50:02
    /// clock 12:01, nothing waiting; append 11:58:30 11:59:10 12:00:45   → all on time
    /// clock 12:01, waiting until 12:00:40; append 12:00:20 alone        → 20 s behind: late
    /// clock 12:00, waiting until 12:05:00; append 11:59:55 alone        → 5 s behind the clock: on time
    /// ```
    fn part_late(&self, stream: i64, events: Vec<Kept>, now: i64) -> (Vec<Kept>, Vec<Kept>) {
        let mut newest = self.waiting.lock().unwrap_or_else(PoisonError::into_inner).get(&stream).copied();
        let (mut on_time, mut late) = (Vec::new(), Vec::new());
        for event in events {
            match newest {
                Some(newest) if event.at < newest.min(now).saturating_sub(nanos(LATENESS)) => late.push(event),
                _ => {
                    newest = Some(newest.map_or(event.at, |newest| newest.max(event.at)));
                    on_time.push(event);
                }
            }
        }
        (on_time, late)
    }

    /// The rows of `events` in their order: a row is cut at the events it
    /// holds at most, or at what they weigh.
    fn rows_of(&self, events: &[Kept]) -> Result<Vec<Row>> {
        let mut codec = self.codec.lock().unwrap_or_else(PoisonError::into_inner);
        let (mut rows, mut from, mut weight) = (Vec::new(), 0, 0);
        for (index, event) in events.iter().enumerate() {
            let added = event.weight();
            if index > from && (index - from == ROW_EVENTS || weight + added > ROW_WEIGHT) {
                rows.push(head::encode(&events[from..index], &mut codec)?);
                (from, weight) = (index, 0);
            }
            weight += added;
        }
        rows.push(head::encode(&events[from..], &mut codec)?);
        Ok(rows)
    }
}

/// The times a stream takes an event at: from what it keeps to ten minutes
/// ahead of `now`.
fn window(stream: &Stream, now: i64) -> (i64, i64) {
    (now.saturating_sub(nanos(stream.keeps())), now.saturating_add(nanos(AHEAD)))
}

/// An event with its time, once it is one the file keeps: a name, fields
/// within their bounds, a time within its stream's window.
fn check(event: NewEvent, now: i64, (oldest, newest): (i64, i64)) -> Result<Kept> {
    if let Some(refused) = &event.refused {
        return Err(Error::invalid(refused.clone()));
    }
    if event.name.is_empty() {
        return Err(Error::invalid("an event has a name"));
    }
    for (what, fields) in [("fields", &event.fields), ("context", &event.context)] {
        if fields.len() > MAX_KEYS {
            return Err(Error::invalid(format!("{} keys in its {what}, past {MAX_KEYS}", fields.len())));
        }
    }
    let at = event.at.map_or(now, unix_nanos);
    let event = event.kept(at);
    if event.weight() > MAX_EVENT {
        return Err(Error::limit(format!("{} bytes, past the {MAX_EVENT} an event weighs", event.weight())));
    }
    if at < oldest {
        return Err(Error::new(ErrorKind::TooOld, "older than its stream keeps"));
    }
    if at > newest {
        return Err(Error::new(ErrorKind::TooNew, "more than ten minutes ahead of the store's clock"));
    }
    Ok(event)
}

#[cfg(test)]
#[path = "events_tests.rs"]
mod tests;
