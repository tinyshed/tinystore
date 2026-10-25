//! The Records engine's awaited calls: the streams, events and reads of
//! [`crate::records`], each call that reads or writes returning a future,
//! and an answer walked event by event or a tail a [`Stream`].
//!
//! ```no_run
//! # async fn example() -> tinystore::Result<()> {
//! use tinystore::aio::Store;
//! use tinystore::aio::records::Level;
//!
//! let store = Store::open("data", Default::default())?;
//! let audit = store.events("audit").open()?; // opening stays a plain call
//! audit.append("role.changed", &serde_json::json!({ "user": 42 })).await?; // written once it returns
//! let warnings = store.all_events()?.list().min_level(Level::WARN).count().await?;
//! # Ok(())
//! # }
//! ```
//!
//! | A call                                     | Runs                                                   | Its future or stream dropped once polled |
//! |--------------------------------------------|--------------------------------------------------------|------------------------------------------|
//! | an append                                  | checked in the poll, compressed there or on the pool   | commits; its answer is let go            |
//! | a read: `page`, `count`, `poll`, `streams` | on the store's pool                                    | stops; its reader goes back              |
//! | [`List::all`], a stream of events          | a page's worth at a time on the pool                   | the page being read stops                |
//! | [`List::tail`], a stream of batches        | a waiter on no thread, each batch taken in the poll    | the tail closes                          |
//! | [`AllEvents::drop`], [`Lines::flush`]      | blocking, on the store's pool                          | runs to its end                          |
//!
//! [`Stream`]: futures_core::Stream
//!
//! Opening a stream, the logger and a write of lines stay plain calls: a
//! logger's line and a writer's piece go to a buffer and never wait. The
//! values are the blocking module's, re-exported here. A store opened without
//! background work runs [`crate::records::maintain`], which stays blocking,
//! on [`Store::blocking`].
//!
//! Records has no point read: a page, a count and a poll each read rows and
//! decode blocks, so every read runs on the pool.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, SystemTime};

use futures_core::Stream;
use serde::Serialize;

use super::Store;
use super::run;
use crate::cancel::{self, Captured};
use crate::pool::Pool;
use crate::sync::lock;
use crate::{ErrorKind, Result};

pub use crate::records::{
    Aggregate, Damage, Event, Fields, Level, NewEvent, Number, PAGE_EVENTS, Page, Part, PollBatch, Row, SpanId, Stats,
    StreamInfo, TailBatch, Top, TraceId,
};

impl Store {
    /// A logger of the stream `stream`, as [`crate::Store::logger`] gives
    /// one: a `tracing` layer whose lines go to a buffer and never wait.
    pub fn logger(&self, stream: &str) -> crate::records::LoggerBuilder<'_> {
        self.blocking().logger(stream)
    }

    /// The stream of events `stream`, made the first time it opens, as
    /// [`crate::Store::events`] opens it: its appends and reads awaited.
    pub fn events(&self, stream: &str) -> EventsBuilder<'_> {
        EventsBuilder { builder: self.blocking().events(stream), pool: self.pool() }
    }

    /// Every stream's events, to read. The first opens the records engine,
    /// a plain call made once at start, since it may migrate records.db.
    pub fn all_events(&self) -> Result<AllEvents> {
        Ok(AllEvents { all: self.blocking().all_events()?, pool: Arc::clone(self.pool()) })
    }
}

/// What opens a stream of events: its name, and how long it keeps them.
#[derive(Debug)]
pub struct EventsBuilder<'s> {
    builder: crate::records::EventsBuilder<'s>,
    pool: &'s Arc<Pool>,
}

impl EventsBuilder<'_> {
    /// How long the stream keeps its events, as
    /// [`crate::records::EventsBuilder::keep`] says.
    pub fn keep(mut self, keep: Duration) -> Self {
        self.builder = self.builder.keep(keep);
        self
    }

    /// Opens the stream: a plain call, made once at start, which may migrate
    /// records.db and write the stream's row the first time.
    pub fn open(self) -> Result<Events> {
        Ok(Events { events: self.builder.open()?, pool: Arc::clone(self.pool) })
    }
}

/// A stream of events: its appends wait for its file without a thread, and
/// its reads run on the store's pool.
#[derive(Clone)]
pub struct Events {
    events: crate::records::Events,
    pool: Arc<Pool>,
}

impl fmt::Debug for Events {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.events, f)
    }
}

impl From<crate::records::Events> for Events {
    /// The same stream, its calls awaited.
    fn from(events: crate::records::Events) -> Events {
        let pool = events.store().map_or_else(Pool::stopped, |store| Pool::of_or_stopped(&store));
        Events { events, pool }
    }
}

impl Events {
    /// The same stream, for its blocking calls.
    pub fn blocking(&self) -> &crate::records::Events {
        &self.events
    }

    /// Writes an event of `name` with `fields`, a struct or a map, at the
    /// store's time, its fields serialized when the call is made. It answers
    /// once the event is written, and a read finds it at once: always
    /// `true`, since only an event with an id can be one its stream holds.
    pub fn append(&self, name: &str, fields: &impl Serialize) -> impl Future<Output = Result<bool>> + Send + 'static {
        one(self.append_all([NewEvent::new(name).fields(fields)]))
    }

    /// An append of one event, what it carries beside its fields before its
    /// verb, as [`crate::records::Events::event`] has.
    pub fn event(&self, name: &str) -> EventCall<'_> {
        EventCall { events: self, event: NewEvent::new(name) }
    }

    /// Writes a list of events, all of them or none, in one commit, and
    /// answers how many it wrote: an event whose id the stream holds, or an
    /// earlier event of the list holds, is nothing.
    ///
    /// The events are checked in the future's first poll, at most 4 MiB an
    /// append, and queued at records.db's writer, whose commit runs on the
    /// store's pool. Up to 256 KiB, a head row's worth, they are cut into rows
    /// and compressed in that poll too; a heavier append, or one that comes
    /// while another compresses, is compressed on the pool. When the store's
    /// memory has no room for them yet, the append waits for it on no thread,
    /// and is compressed on the pool once the room comes. Once polled, the
    /// events commit and reach every tail open, whether or not anyone still
    /// awaits them.
    pub fn append_all(
        &self,
        events: impl IntoIterator<Item = NewEvent>,
    ) -> impl Future<Output = Result<usize>> + Send + 'static {
        let (handle, events) = (self.events.clone(), events.into_iter().collect::<Vec<_>>());
        let what = format!("events {}: an append", self.events.name());
        run::write(&self.pool, what, move |answer| handle.append_then(events, move |appended| answer.send(appended)))
    }

    /// The stream's events, found by the conditions the answer adds.
    pub fn list(&self) -> List {
        let named = format!("events {}", self.events.name());
        List { list: self.events.list(), pool: Arc::clone(&self.pool), named: Arc::from(named) }
    }

    /// A writer of another program's output, as
    /// [`crate::records::Events::lines`] opens one: a write never waits, and
    /// a flush is awaited.
    pub fn lines(&self) -> Result<Lines> {
        let named = format!("lines of events {}", self.events.name());
        Ok(Lines { lines: self.events.lines()?, pool: Arc::clone(&self.pool), named })
    }
}

/// One event on its way into a stream.
#[derive(Debug)]
pub struct EventCall<'e> {
    events: &'e Events,
    event: NewEvent,
}

impl EventCall<'_> {
    /// What names it in its stream, as [`crate::records::EventCall::id`]
    /// has: an append of an id the stream holds writes nothing.
    pub fn id(mut self, id: impl AsRef<str>) -> Self {
        self.event = self.event.id(id);
        self
    }

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

    /// Writes the event with `fields`, as [`Events::append_all`] writes one,
    /// and answers whether it did: `false` when its stream holds its id.
    pub fn append(self, fields: &impl Serialize) -> impl Future<Output = Result<bool>> + Send + 'static {
        one(self.events.append_all([self.event.fields(fields)]))
    }
}

/// Whether an append of one event wrote it.
async fn one(appended: impl Future<Output = Result<usize>>) -> Result<bool> {
    appended.await.map(|written| written == 1)
}

/// Every stream's events, to read.
#[derive(Clone)]
pub struct AllEvents {
    all: crate::records::AllEvents,
    pool: Arc<Pool>,
}

impl fmt::Debug for AllEvents {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.all, f)
    }
}

impl From<crate::records::AllEvents> for AllEvents {
    /// The same events, their calls awaited.
    fn from(all: crate::records::AllEvents) -> AllEvents {
        let pool = all.store().map_or_else(Pool::stopped, |store| Pool::of_or_stopped(&store));
        AllEvents { all, pool }
    }
}

impl AllEvents {
    /// The same events, for their blocking calls.
    pub fn blocking(&self) -> &crate::records::AllEvents {
        &self.all
    }

    /// The events of every stream, or of those `streams` names, found by
    /// the conditions the answer adds.
    pub fn list(&self) -> List {
        List { list: self.all.list(), pool: Arc::clone(&self.pool), named: Arc::from("events") }
    }

    /// Every stream with events, by name, as
    /// [`crate::records::AllEvents::streams`] lists them.
    pub fn streams(&self) -> impl Future<Output = Result<Vec<StreamInfo>>> + Send + 'static {
        let all = self.all.clone();
        run::read(&self.pool, "events: the streams".to_owned(), move || all.streams())
    }

    /// Every packed event once, a batch a call, 1000 at most, from `cursor`,
    /// as [`crate::records::AllEvents::poll`] gives them: an empty batch at
    /// once when nothing past the cursor is packed. A dropped poll leaves the
    /// cursor the program keeps as it was.
    pub fn poll(&self, cursor: Option<&str>) -> impl Future<Output = Result<PollBatch>> + Send + 'static {
        let (all, cursor) = (self.all.clone(), cursor.map(str::to_owned));
        run::read(&self.pool, "events: a poll".to_owned(), move || all.poll(cursor.as_deref()))
    }

    /// As `poll`, `limit` events at most, 10,000 at the most.
    pub fn poll_limit(
        &self,
        cursor: Option<&str>,
        limit: usize,
    ) -> impl Future<Output = Result<PollBatch>> + Send + 'static {
        let (all, cursor) = (self.all.clone(), cursor.map(str::to_owned));
        run::read(&self.pool, "events: a poll".to_owned(), move || all.poll_limit(cursor.as_deref(), limit))
    }

    /// What reads, packing, merges and polls met that no longer reads, as
    /// [`crate::records::AllEvents::damaged`] lists it.
    pub fn damaged(&self) -> impl Future<Output = Result<Vec<Damage>>> + Send + 'static {
        let all = self.all.clone();
        run::read(&self.pool, "events: what is damaged".to_owned(), move || all.damaged())
    }

    /// Removes a head row or a segment that no longer reads, as
    /// [`crate::records::AllEvents::drop`] does, on the store's pool: it
    /// reads the part again under maintenance's lock, then writes its
    /// removal. Once polled it runs to its end, as a write does.
    pub fn drop(&self, damage: &Damage) -> impl Future<Output = Result<()>> + Send + 'static {
        let (all, damage) = (self.all.clone(), damage.clone());
        run::blocking(&self.pool, None, format!("events: a drop of {damage}"), move || all.drop(&damage))
    }
}

/// A read being asked: every condition narrows it, and an awaited call runs
/// it on the store's pool.
#[derive(Clone)]
pub struct List {
    list: crate::records::List,
    pool: Arc<Pool>,
    /// What the read's errors name: `events audit`.
    named: Arc<str>,
}

impl fmt::Debug for List {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.list, f)
    }
}

impl List {
    /// The span before now the range covers; or `from` and `to`.
    pub fn since(self, span: Duration) -> Self {
        self.with(|list| list.since(span))
    }

    /// The range's start, which is in it.
    pub fn from(self, at: SystemTime) -> Self {
        self.with(|list| list.from(at))
    }

    /// The range's end, which is not in it.
    pub fn to(self, at: SystemTime) -> Self {
        self.with(|list| list.to(at))
    }

    /// Any of these streams, for a read of every stream.
    pub fn streams<S: Into<String>>(self, streams: impl IntoIterator<Item = S>) -> Self {
        self.with(|list| list.streams(streams))
    }

    /// Any of these names: `["log"]` is the lines.
    pub fn names<S: Into<String>>(self, names: impl IntoIterator<Item = S>) -> Self {
        self.with(|list| list.names(names))
    }

    /// This level and worse. An event without a level is not found.
    pub fn min_level(self, level: Level) -> Self {
        self.with(|list| list.min_level(level))
    }

    pub fn trace_id(self, trace_id: TraceId) -> Self {
        self.with(|list| list.trace_id(trace_id))
    }

    /// `key` with this value, at the top of an event's fields or of its
    /// context, by the JSON the value is written as.
    pub fn filter(self, key: &str, value: &impl Serialize) -> Self {
        self.with(|list| list.filter(key, value))
    }

    /// This text as a phrase, its case ignored.
    pub fn search(self, text: &str) -> Self {
        self.with(|list| list.search(text))
    }

    /// Newest first; oldest first without it.
    pub fn newest(self) -> Self {
        self.with(crate::records::List::newest)
    }

    /// The events of a page: 1000, and 10,000 at most.
    pub fn limit(self, events: usize) -> Self {
        self.with(|list| list.limit(events))
    }

    /// A page's `next`, with the rest of the read as it was.
    pub fn after(self, next: &str) -> Self {
        self.with(|list| list.after(next))
    }

    fn with(mut self, narrow: impl FnOnce(crate::records::List) -> crate::records::List) -> Self {
        self.list = narrow(self.list);
        self
    }

    /// One page, from one moment of the file, as
    /// [`crate::records::List::page`] reads it.
    pub fn page(&self) -> impl Future<Output = Result<Page>> + Send + 'static {
        let list = self.list.clone();
        run::read(&self.pool, format!("{}: a page", self.named), move || list.page())
    }

    /// The keys the events of the read's range hold, sorted, as
    /// [`crate::records::List::field_names`] gives them.
    pub fn field_names(&self) -> impl Future<Output = Result<Vec<String>>> + Send + 'static {
        self.field_names_prefix("")
    }

    /// The keys `field_names` gives that begin with `prefix`.
    pub fn field_names_prefix(&self, prefix: &str) -> impl Future<Output = Result<Vec<String>>> + Send + 'static {
        let (list, prefix) = (self.list.clone(), prefix.to_owned());
        run::read(&self.pool, format!("{}: its field names", self.named), move || list.field_names_prefix(&prefix))
    }

    /// How many events the read finds. It walks every page of the read on
    /// one thread of the store's pool, several at once while they hold few
    /// events; a drop stops the walk at the page it reads.
    pub fn count(&self) -> impl Future<Output = Result<u64>> + Send + 'static {
        let list = self.list.clone();
        run::read(&self.pool, format!("{}: a count", self.named), move || list.count())
    }

    /// The most frequent values of `key` and how many events hold each, as
    /// [`crate::records::List::top`] counts them, walking the read as
    /// `count` does.
    pub fn top(&self, key: &str) -> impl Future<Output = Result<Vec<Top>>> + Send + 'static {
        let (list, key) = (self.list.clone(), key.to_owned());
        run::read(&self.pool, format!("{}: a top of {key}", self.named), move || list.top(&key))
    }

    /// A row for each span and each value of the keys `by` names, as
    /// [`crate::records::List::aggregate`] makes them, walking the read as
    /// `count` does.
    pub fn aggregate(&self, aggregate: &Aggregate) -> impl Future<Output = Result<Vec<Row>>> + Send + 'static {
        let (list, aggregate) = (self.list.clone(), aggregate.clone());
        run::read(&self.pool, format!("{}: an aggregate", self.named), move || list.aggregate(&aggregate))
    }

    /// The events appended from now on that meet the read's conditions, a
    /// batch as they come, as [`crate::records::List::tail`] opens a tail:
    /// a plain call, which checks the read and opens the tail.
    pub fn tail(&self) -> Result<Tail> {
        Ok(Tail::waking(self.list.tail()?))
    }

    /// Every event of the answer, as a stream: a page's worth at a time, read
    /// on the store's pool once the stream has given the last, never sooner.
    /// A cancellation token around this call stops its reads as one around a
    /// blocking read does.
    pub fn all(self) -> All {
        let List { list, pool, named } = self;
        All {
            walk: Some(list.all()),
            page: Vec::new().into_iter(),
            reading: None,
            pool,
            named,
            tokens: cancel::captured(),
        }
    }
}

/// What a page of [`All`] reads on the pool: the walk back, and the page.
type Read = Pin<Box<dyn Future<Output = Result<(crate::records::All, Option<Result<Vec<Event>>>)>> + Send>>;

/// Every event of an answer, a [`Stream`] that reads a page's worth on the
/// store's pool each time it has given the last. Dropped while a page is
/// read, that read stops as a dropped read does.
pub struct All {
    /// The blocking walk between two pages; none while a page is read on the
    /// pool, and once the answer ended.
    walk: Option<crate::records::All>,
    page: std::vec::IntoIter<Event>,
    reading: Option<Read>,
    pool: Arc<Pool>,
    named: Arc<str>,
    /// The tokens around the call that made the stream, which each page's
    /// read runs under.
    tokens: Captured,
}

impl fmt::Debug for All {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("All").field("read", &self.named).field("reading", &self.reading.is_some()).finish()
    }
}

impl Stream for All {
    type Item = Result<Event>;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let all = self.get_mut();
        loop {
            if let Some(event) = all.page.next() {
                return Poll::Ready(Some(Ok(event)));
            }
            if let Some(reading) = &mut all.reading {
                let read = std::task::ready!(reading.as_mut().poll(context));
                all.reading = None;
                match read {
                    Ok((walk, Some(Ok(events)))) => (all.walk, all.page) = (Some(walk), events.into_iter()),
                    // the walk ends with its error, as the blocking iterator does, and so does a closed pool's
                    Ok((_, Some(Err(error)))) | Err(error) => return Poll::Ready(Some(Err(error))),
                    Ok((_, None)) => return Poll::Ready(None),
                }
                continue;
            }
            let Some(mut walk) = all.walk.take() else { return Poll::Ready(None) };
            let tokens = all.tokens.clone();
            let next = move || {
                let page = tokens.run(None, || walk.page());
                Ok((walk, page))
            };
            all.reading = Some(Box::pin(run::read(&all.pool, format!("{}: a page of all", all.named), next)));
        }
    }
}

/// The events appended after a tail opened that meet its read, a [`Stream`]
/// of batches that waits on no thread: each append that brings events wakes
/// it from the thread that committed them, and the batch is taken in the
/// poll, from the store's hold of its last megabyte of events. It ends when
/// the store closes, and dropped, the tail closes.
///
/// A wait with a bound is the runtime's: `tokio::time::timeout(d,
/// tail.next())` ends the wait and keeps the tail open.
pub struct Tail {
    tail: crate::records::Tail,
    /// What the task that polled last is woken through.
    waker: Arc<Mutex<Option<std::task::Waker>>>,
    ended: bool,
}

impl fmt::Debug for Tail {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.tail, f)
    }
}

impl Tail {
    fn waking(tail: crate::records::Tail) -> Tail {
        let waker: Arc<Mutex<Option<std::task::Waker>>> = Arc::default();
        let woken = Arc::clone(&waker);
        tail.wake_with(Arc::new(move || {
            let waker = lock(&woken).clone();
            if let Some(waker) = waker {
                waker.wake();
            }
        }));
        Tail { tail, waker, ended: false }
    }
}

impl Stream for Tail {
    type Item = Result<TailBatch>;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let tail = self.get_mut();
        if tail.ended {
            return Poll::Ready(None);
        }
        // Kept before the batch is looked for: an append between the two wakes this task.
        let mut waker = lock(&tail.waker);
        if !waker.as_ref().is_some_and(|kept| kept.will_wake(context.waker())) {
            *waker = Some(context.waker().clone());
        }
        drop(waker);
        match tail.tail.next_within(Duration::ZERO) {
            Ok(Some(batch)) => Poll::Ready(Some(Ok(batch))),
            Ok(None) => Poll::Pending,
            Err(error) if error.kind() == ErrorKind::Closed => {
                tail.ended = true;
                Poll::Ready(None)
            }
            Err(error) => Poll::Ready(Some(Err(error))),
        }
    }
}

/// A writer of another program's lines into a stream: a write goes to its
/// buffer and never waits, and a flush is awaited. Dropped, it hands over
/// what it holds, a last line without its newline too, which the store
/// writes within a second.
///
/// It is no `std::io::Write`, whose flush would block the executor's thread.
pub struct Lines {
    lines: crate::records::Lines,
    pool: Arc<Pool>,
    named: String,
}

impl fmt::Debug for Lines {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.lines, f)
    }
}

impl Lines {
    /// Takes a piece of output: each line it ends becomes an event. It never
    /// waits, and fails only once the writer or its store closed.
    pub fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.lines.take(bytes)
    }

    /// Writes the events of every line ended so far, and answers once they
    /// are written; a line without its newline yet stays. It runs on the
    /// store's pool, and once polled runs to its end, as a write does.
    pub fn flush(&mut self) -> impl Future<Output = Result<()>> + Send + 'static {
        run::blocking(&self.pool, None, format!("{}: a flush", self.named), self.lines.flushing())
    }

    /// The lines dropped since the writer began: a full buffer, a line
    /// longer than an event holds, one the file refused.
    pub fn dropped(&self) -> u64 {
        self.lines.dropped()
    }
}

#[cfg(test)]
#[path = "records_tests.rs"]
mod tests;
