//! A stream of events as a program opens it, `store.events("audit")`, and
//! every stream as it reads them, `store.all_events()`.

use std::fmt;
use std::sync::{Arc, MutexGuard, PoisonError};
#[cfg(feature = "async")]
use std::sync::{Mutex, TryLockError};
use std::time::{Duration, SystemTime};

use rusqlite::params;
use serde::Serialize;

use super::engine::{AHEAD, LATENESS, Records, Stream, execute, nanos};
use super::event::Kept;
use super::event::{Level, MAX_EVENT, MAX_KEYS, NewEvent, SpanId, TraceId};
use super::head::{self, Codec, Row};
use super::read::List;
use crate::sqlite::Tx;
use crate::{Error, ErrorKind, Result, Store, unix_nanos};
#[cfg(feature = "async")]
use crate::{Reservation, polled::Lead};

/// What one append carries at most: more is sent in pieces.
const MAX_APPEND: usize = 4 << 20;
/// What an awaited append compresses in its poll at most: a head row's
/// worth. A heavier one, up to the 4 MiB an append carries, is compressed on
/// the store's pool, so that a poll costs the executor one row at most.
#[cfg(feature = "async")]
const COMPRESSED_IN_A_POLL: u64 = head::ROW_WEIGHT as u64;
/// What an append's hold of the store's memory names.
const APPEND: &str = "records: an append";
use crate::memory::PATIENCE;

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

/// What the async API's appends stand on: `append_all` answered through a
/// completion rather than on the thread that waits.
#[cfg(feature = "async")]
impl Events {
    /// Appends `events`, all of them or none, without waiting: they are
    /// checked here, and `done` is given the answer on the thread that
    /// commits them, or here when they are refused first.
    ///
    /// Run inside `polled::leading_on`, as an awaited append's submit is, it
    /// waits for nothing here:
    ///
    /// | the store's memory | the codec, the events | compressed and submitted                 |
    /// |--------------------|-----------------------|------------------------------------------|
    /// | room now           | free, a row's worth   | here, the commit led on the scope's pool |
    /// | room now           | held, or heavier      | on the pool, the memory held             |
    /// | no room yet        | either                | on the pool, once the memory serves it   |
    ///
    /// The codec is every append's, a logger's flush among them: a poll that
    /// waited for it would wait while another compresses up to 4 MiB. What
    /// goes to the pool goes whatever becomes of the future that asked, so a
    /// polled append commits (DUR-11), and fails `Limit` once the memory's
    /// patience is out.
    pub(crate) fn append_then(&self, events: Vec<NewEvent>, done: impl FnOnce(Result<()>) + Send + 'static) {
        let name = Arc::clone(&self.stream.name);
        let done = move |appended: Result<()>| done(appended.map_err(|error| error.within(format!("events {name}"))));
        let (checked, now) = match self.records.check_append(&self.stream, events) {
            Ok(checked) => checked,
            Err(error) => return done(Err(error)),
        };
        if checked.is_empty() {
            return done(Ok(()));
        }
        let weight = weight_of(&checked);
        let records = Arc::clone(&self.records);
        let appending = Appending { records, stream: self.stream.clone(), checked, now, weight };
        match self.records.memory.try_reserve(weight, APPEND) {
            Ok(held) => appending.submit_here_or_lead(held, done),
            Err(_) => appending.wait_for_memory(done),
        }
    }

    /// The store, while it is open: what an async handle made from this one
    /// takes the store's pool from.
    pub(crate) fn store(&self) -> Option<Store> {
        self.records.store()
    }

    /// The stream's name, which an awaited call's errors name.
    pub(crate) fn name(&self) -> &str {
        &self.stream.name
    }
}

#[cfg(feature = "async")]
impl AllEvents {
    /// The store, while it is open.
    pub(crate) fn store(&self) -> Option<Store> {
        self.records.store()
    }
}

/// An append checked and on its way to its file.
#[cfg(feature = "async")]
struct Appending {
    records: Arc<Records>,
    stream: Stream,
    checked: Vec<Kept>,
    now: i64,
    /// What the events hold of the store's memory.
    weight: u64,
}

#[cfg(feature = "async")]
impl Appending {
    /// Compresses the events and queues them at the file's writer here, when
    /// they weigh a head row at most and no other append holds the codec;
    /// otherwise both run on the scope's pool, `held` with them.
    fn submit_here_or_lead(self, held: Reservation, done: impl FnOnce(Result<()>) + Send + 'static) {
        let records = Arc::clone(&self.records);
        let light = self.weight <= COMPRESSED_IN_A_POLL;
        let Some(mut codec) = light.then(|| records.codec_if_free()).flatten() else {
            return self.lead(crate::polled::lead(), held, done);
        };
        let encoded = records.encode(&self.stream, self.checked, self.now, &mut codec);
        drop(codec);
        records.submit_encoded(encoded, held, done);
    }

    /// Compresses the events, waiting for the codec, and queues them: on the
    /// pool, where a wait is no executor's.
    fn submit(self, held: Reservation, done: impl FnOnce(Result<()>) + Send + 'static) {
        let Appending { records, stream, checked, now, .. } = self;
        let encoded = records.encode(&stream, checked, now, &mut records.codec());
        records.submit_encoded(encoded, held, done);
    }

    /// Hands the compression and the submit to `lead`, the scope's pool,
    /// which runs them whoever still waits: never on the thread that polls,
    /// nor on the one that gave memory back. Outside an awaited call, with no
    /// lead, they run here.
    fn lead(self, lead: Option<Lead>, held: Reservation, done: impl FnOnce(Result<()>) + Send + 'static) {
        let submit = move || self.submit(held, done);
        match lead {
            Some(lead) => lead(Box::new(submit)),
            None => submit(),
        }
    }

    /// Waits for the events' weight of the store's memory in the line every
    /// reservation waits in, on no thread, then submits on the scope's pool.
    ///
    /// The waiter is kept by what answers it, not by the future that asked:
    /// dropped with that future it would leave the line, and an append polled
    /// once would then write nothing.
    fn wait_for_memory(self, done: impl FnOnce(Result<()>) + Send + 'static) {
        let (memory, lead) = (Arc::clone(&self.records.memory), crate::polled::lead());
        let kept: Arc<Mutex<Option<crate::memory::Waiting>>> = Arc::default();
        let keeping = Arc::clone(&kept);
        let waiting = memory.reserve_then(self.weight, APPEND, PATIENCE, move |reserved| {
            let _answered = keeping;
            match reserved {
                Ok(held) => self.lead(lead, held, done),
                Err(error) => done(Err(error)),
            }
        });
        *crate::sync::lock(&kept) = Some(waiting);
    }
}

#[cfg(feature = "async")]
impl Records {
    /// The codec, unless another append holds it now.
    fn codec_if_free(&self) -> Option<MutexGuard<'_, Codec>> {
        match self.codec.try_lock() {
            Ok(codec) => Some(codec),
            Err(TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
            Err(TryLockError::WouldBlock) => None,
        }
    }

    /// Queues an append's rows at the file's writer: `held` goes back once
    /// they commit.
    fn submit_encoded(
        self: &Arc<Self>,
        encoded: Result<(HeadRows, Written)>,
        held: Reservation,
        done: impl FnOnce(Result<()>) + Send + 'static,
    ) {
        let (rows, written) = match encoded {
            Ok(encoded) => encoded,
            Err(error) => return done(Err(error)),
        };
        let records = Arc::clone(self);
        self.file.submit(
            rows.bytes(),
            move |tx| rows.write(tx).map(|()| records.when_committed(tx, written)),
            move |appended| {
                drop(held);
                done(appended);
            },
        );
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
    fn append(self: &Arc<Self>, stream: &Stream, events: Vec<NewEvent>) -> Result<()> {
        let (checked, now) = self.check_append(stream, events)?;
        self.write_checked(stream, checked, now)
    }

    /// The events of an append, each one the file keeps, and the store's time
    /// they were checked at: a refused one names its place and its name.
    fn check_append(&self, stream: &Stream, events: Vec<NewEvent>) -> Result<(Vec<Kept>, i64)> {
        let now = self.now();
        let window = window(stream, now);
        let mut checked = Vec::with_capacity(events.len());
        for (place, event) in events.into_iter().enumerate() {
            let named = format!("event {place} {:?}", event.name);
            checked.push(check(event, now, window).map_err(|error| error.within(named))?);
        }
        let weight = weight_of(&checked);
        if weight > MAX_APPEND as u64 {
            return Err(Error::limit(format!("an append of {weight} bytes, past the {MAX_APPEND} one carries")));
        }
        Ok((checked, now))
    }

    /// Appends what a logger held: an event the file cannot keep is left out,
    /// and the rest written an append's worth at a time. It gives how many
    /// were lost, those left out and what a failed write and the writes after
    /// it held, with the failure.
    pub(super) fn append_held(self: &Arc<Self>, stream: &Stream, events: Vec<NewEvent>) -> (u64, Result<()>) {
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

    /// Writes events already checked in one commit: reserve → encode → write
    /// → hand them to the tails.
    fn write_checked(self: &Arc<Self>, stream: &Stream, checked: Vec<Kept>, now: i64) -> Result<()> {
        if checked.is_empty() {
            return Ok(());
        }
        let _held = self.memory.reserve(weight_of(&checked), APPEND, PATIENCE)?;
        let (rows, written) = self.encode(stream, checked, now, &mut self.codec())?;
        let records = Arc::clone(self);
        self.file.write(rows.bytes(), move |tx| rows.write(tx).map(|()| records.when_committed(tx, written)))
    }

    /// Asks the commit that writes an append's rows to follow it with
    /// [`written`](Self::written), with the file's writer still held. The
    /// rows of a commit's appends take their ids, and a read its order of
    /// events of one time, in the order the commit wrote them; handed to the
    /// tails as each append's answer comes back, an awaited append, answered
    /// inside the commit, would pass a blocking one whose thread is woken
    /// after it, and two blocking ones would race.
    fn when_committed(self: &Arc<Self>, tx: &Tx<'_>, written: Written) {
        let records = Arc::clone(self);
        tx.after_commit(Box::new(move || records.written(written)));
    }

    /// The zstd context every append compresses its rows with, which one
    /// append holds at a time.
    fn codec(&self) -> MutexGuard<'_, Codec> {
        self.codec.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Parts the late from those on time, and cuts each into the head rows
    /// that hold them, compressed with `codec`.
    fn encode(&self, stream: &Stream, checked: Vec<Kept>, now: i64, codec: &mut Codec) -> Result<(HeadRows, Written)> {
        let (on_time, late) = self.part_late(stream.id, checked, now);
        let mut rows = Vec::new();
        for (is_late, events) in [(false, &on_time), (true, &late)].into_iter().filter(|(_, events)| !events.is_empty())
        {
            rows.extend(rows_of(events, codec)?.into_iter().map(|row| (is_late, row)));
        }
        let written = Written { stream: stream.id, name: Arc::clone(&stream.name), on_time, late };
        Ok((HeadRows { stream: stream.id, now, rows }, written))
    }

    /// What follows an append's commit: its events go to the tails open and
    /// are counted, and the newest on time moves on, which tells the next
    /// event of its stream whether it is late.
    fn written(&self, written: Written) {
        let Written { stream, name, on_time, late } = written;
        self.recent.add(&name, on_time.iter().chain(&late));
        self.probe.count("tinystore_records_events_total", &[("stream", &name)], (on_time.len() + late.len()) as f64);
        if let Some(newest) = on_time.iter().map(|event| event.at).max() {
            let mut waiting = self.waiting.lock().unwrap_or_else(PoisonError::into_inner);
            waiting.entry(stream).and_modify(|waits| *waits = newest.max(*waits)).or_insert(newest);
        }
    }

    /// `events` parted into those on time and the late, each in the order
    /// they came. An event is late when it comes more than ten seconds behind
    /// the newest its stream showed on time before it, earlier in this
    /// append or in `waiting`, or behind the store's clock where that is
    /// earlier; so a producer ahead of the clock makes no one late:
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
}

/// The rows of `events` in their order: a row is cut at the events it holds
/// at most, or at what they weigh.
fn rows_of(events: &[Kept], codec: &mut Codec) -> Result<Vec<Row>> {
    super::pack::cut(events).into_iter().map(|row| head::encode(row, codec)).collect()
}

/// What an append's checked events weigh, which it holds of the store's memory.
fn weight_of(checked: &[Kept]) -> u64 {
    checked.iter().map(|event| event.weight() as u64).sum()
}

/// An append's head rows, as its commit writes them.
struct HeadRows {
    stream: i64,
    now: i64,
    /// Each row, and whether it holds the late.
    rows: Vec<(bool, Row)>,
}

impl HeadRows {
    fn bytes(&self) -> usize {
        self.rows.iter().map(|(_, row)| row.body.len()).sum()
    }

    fn write(&self, tx: &Tx<'_>) -> Result<()> {
        let (stream, now) = (self.stream, self.now);
        self.rows.iter().try_for_each(|(late, row)| {
            let (count, weight, size) = (row.count as i64, row.weight as i64, row.body.len() as i64);
            let head = params![stream, late, row.first, row.last, row.levels, count, weight, size, now, row.body];
            execute(tx, INSERT_HEAD, head, "write a head row")?;
            execute(tx, ADD_TO_HEAD, params![stream, late, count, weight, now], "count what a head holds").map(drop)
        })
    }
}

/// An append's events, kept for what follows its commit.
struct Written {
    stream: i64,
    name: Arc<str>,
    on_time: Vec<Kept>,
    late: Vec<Kept>,
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
        let ahead = super::engine::AHEAD.as_secs() / 60;
        return Err(Error::new(ErrorKind::TooNew, format!("more than {ahead} minutes ahead of the store's clock")));
    }
    Ok(event)
}

#[cfg(test)]
#[path = "events_tests.rs"]
mod tests;
