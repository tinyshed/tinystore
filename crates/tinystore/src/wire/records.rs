//! records over protocol 2: a stream of events by handle, appended whole and
//! read a page at a time, and every stream read without one. Fields and a
//! context travel as the JSON object each is, every value spelled as it was
//! written, so that what a client wrote comes back byte for byte.
//!
//! A tail is a stream the server sends on, a batch each time events arrive
//! while the client's credit takes them; a writer of lines is one the client
//! sends on, pieces of another program's output written as they come, the
//! last answered with what the writer dropped.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::value::RawValue;

use super::codec::Message;
use super::protocol::{
    Empty, Failure, Handle, RecordsAggregateAsked, RecordsAppend, RecordsBatch, RecordsCount, RecordsDamage,
    RecordsDamages, RecordsDropped, RecordsEvent, RecordsFieldNamesAsked, RecordsNames, RecordsNewEvent, RecordsOpen,
    RecordsPage, RecordsPiece, RecordsPollAsked, RecordsQuery, RecordsRow, RecordsRows, RecordsStats, RecordsStream,
    RecordsStreams, RecordsTailBatch, RecordsTop, RecordsTopAsked, RecordsTops, method,
};
use super::{Link, Route};
use crate::records::{
    Aggregate, Damage, Event, Events, Fields, Level, Lines, List, NewEvent, Number, Part, SpanId, Stats, Tail,
    TailBatch, TraceId,
};
use crate::{ErrorKind, Store, unix_nanos};

pub(crate) type Answered = Result<Vec<u8>, Failure>;

/// A connection's tails, by stream.
type Tails = Mutex<HashMap<u32, Arc<Tailing>>>;

/// The bytes of a tail's batch past its events.
const ENVELOPE: usize = 32;

pub(crate) fn route(called: u16) -> Route {
    match called {
        method::RECORDS_TAIL => Route::Tail,
        method::RECORDS_LINES => Route::Lines,
        _ => Route::Worker,
    }
}

/// The streams a connection opened, by handle, and its tails and writers of
/// lines, by the stream each came on.
#[derive(Default)]
pub(crate) struct Handles {
    open: Mutex<HashMap<u64, Events>>,
    last: AtomicU64,
    tails: Arc<Tails>,
    writers: Mutex<HashMap<u32, Writer>>,
}

impl Handles {
    fn events(&self, handle: u64) -> Result<Events, Failure> {
        let open = self.open.lock().unwrap_or_else(PoisonError::into_inner);
        open.get(&handle)
            .cloned()
            .ok_or_else(|| Failure::invalid(format!("handle {handle} names no stream opened here")))
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<u64, Events>> {
        self.open.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Credit the client granted a tail's stream.
    pub(crate) fn grant(&self, stream: u32, credit: u32) {
        let tailing = lock(&self.tails).get(&stream).cloned();
        if let Some(tailing) = tailing {
            tailing.grant(credit);
        }
    }

    /// Writes a client's piece of lines and gives its credit back; the last,
    /// or one that fails, ends the writer off this thread. Says whether
    /// `stream` is a writer's.
    pub(crate) fn piece(&self, stream: u32, body: &[u8], last: bool) -> bool {
        let mut writers = lock(&self.writers);
        let Some(writer) = writers.get(&stream) else {
            return false;
        };
        let taken = if body.is_empty() {
            Ok(())
        } else {
            RecordsPiece::decode(body).and_then(|piece| writer.lines.take(&piece.bytes).map_err(Failure::from))
        };
        if taken.is_ok() && !last {
            (writer.link.grant)(u32::try_from(body.len()).unwrap_or(u32::MAX));
            return true;
        }
        if let Some(writer) = writers.remove(&stream) {
            let spawn = Arc::clone(&writer.link.spawn);
            spawn(Box::new(move || writer.end(taken)));
        }
        true
    }

    /// Ends a tail or a writer the client cancelled; says whether there was
    /// one. A writer cancelled still hands over the lines it was given.
    pub(crate) fn cancel(&self, stream: u32) -> bool {
        let tailing = lock(&self.tails).remove(&stream);
        if let Some(tailing) = tailing {
            tailing.close(Err(Failure::cancelled("the client cancelled its tail")));
            return true;
        }
        let writer = lock(&self.writers).remove(&stream);
        if let Some(writer) = writer {
            (writer.link.finish)(Err(Failure::cancelled("the client cancelled its lines")));
            return true;
        }
        false
    }

    /// Ends every tail: the server is closing. A writer runs to its end.
    pub(crate) fn go_away(&self) {
        let tails: Vec<_> = lock(&self.tails).drain().map(|(_, tailing)| tailing).collect();
        for tailing in tails {
            tailing.close(Err(Failure::unavailable("the server is closing; tail again on another connection")));
        }
    }

    /// Lets go of every tail and writer at the session's end, a writer
    /// handing over the lines it was given.
    pub(crate) fn end(&self) {
        let tails: Vec<_> = lock(&self.tails).drain().map(|(_, tailing)| tailing).collect();
        for tailing in tails {
            tailing.end();
        }
        lock(&self.writers).clear();
    }
}

/// Answers a call of one message.
pub(crate) fn call(store: &Store, handles: &Handles, called: u16, body: &[u8], read_only: bool) -> Answered {
    match called {
        method::RECORDS_OPEN => open(store, handles, RecordsOpen::decode(body)?, read_only),
        method::RECORDS_APPEND => append(handles, RecordsAppend::decode(body)?),
        method::RECORDS_LIST => list(store, handles, &RecordsQuery::decode(body)?),
        method::RECORDS_COUNT => count(store, handles, &RecordsQuery::decode(body)?),
        method::RECORDS_TOP => top(store, handles, &RecordsTopAsked::decode(body)?),
        method::RECORDS_AGGREGATE => aggregate(store, handles, RecordsAggregateAsked::decode(body)?),
        method::RECORDS_STREAMS => streams(store),
        method::RECORDS_FIELD_NAMES => field_names(store, handles, &RecordsFieldNamesAsked::decode(body)?),
        method::RECORDS_POLL => poll(store, &RecordsPollAsked::decode(body)?),
        method::RECORDS_DAMAGED => damaged(store),
        method::RECORDS_DROP => drop_damage(store, RecordsDamage::decode(body)?),
        other => Err(Failure::unimplemented(format!("method {other:#06x}"))),
    }
}

/// Opens a stream as the call says. A connection that only reads changes
/// no stream's keep: what one keeps is the writer's to say.
fn open(store: &Store, handles: &Handles, asked: RecordsOpen, read_only: bool) -> Answered {
    let mut events = store.events(&asked.stream);
    if let Some(keep) = asked.keep {
        if read_only {
            return Err(Failure::permission("a read connection gives a stream no keep"));
        }
        events = events.keep(Duration::from_millis(keep));
    }
    let events = events.open()?;
    let handle = handles.last.fetch_add(1, Ordering::Relaxed) + 1;
    handles.lock().insert(handle, events);
    Ok(Handle { handle }.encode())
}

fn append(handles: &Handles, asked: RecordsAppend) -> Answered {
    let events = handles.events(asked.handle)?;
    let new = asked.events.into_iter().enumerate().map(|(place, event)| new_event(place, event));
    events.append_all(new.collect::<Result<Vec<_>, Failure>>()?)?;
    Ok(Empty {}.encode())
}

/// An event as the client wrote it; what cannot be one names its place in
/// the list, as a refused event of an append does.
fn new_event(place: usize, asked: RecordsNewEvent) -> Result<NewEvent, Failure> {
    let refused = |what: &str| Failure::invalid(format!("event {place} {:?}: {what}", asked.name));
    let mut event = NewEvent::new(&asked.name);
    if let Some(at) = asked.at {
        event = event.at(time_of(at));
    }
    if let Some(level) = asked.level {
        event = event.level(level_of(level).map_err(|_| refused("a level past what slog counts"))?);
    }
    if let Some(message) = &asked.message {
        event = event.message(message);
    }
    if let Some(fields) = asked.fields {
        event = event.fields(&RawValue::from_string(fields).map_err(|_| refused("fields that are no JSON"))?);
    }
    if let Some(context) = asked.context {
        event = event.context(&RawValue::from_string(context).map_err(|_| refused("a context that is no JSON"))?);
    }
    match (asked.trace_id, asked.span_id) {
        (Some(trace), Some(span)) => Ok(event.trace(TraceId(sized(&trace)?), SpanId(sized(&span)?))),
        (None, None) => Ok(event),
        _ => Err(refused("a trace id and a span id come together")),
    }
}

fn list(store: &Store, handles: &Handles, asked: &RecordsQuery) -> Answered {
    let page = query(store, handles, asked)?.page()?;
    Ok(RecordsPage { events: page.events.into_iter().map(event_of).collect(), next: page.next }.encode())
}

fn count(store: &Store, handles: &Handles, asked: &RecordsQuery) -> Answered {
    Ok(RecordsCount { count: query(store, handles, asked)?.count()? }.encode())
}

fn top(store: &Store, handles: &Handles, asked: &RecordsTopAsked) -> Answered {
    let tops = query(store, handles, &asked.query)?.top(&asked.key)?;
    let values = tops.into_iter().map(|top| RecordsTop { value: top.value.to_string(), count: top.count });
    Ok(RecordsTops { values: values.collect() }.encode())
}

fn aggregate(store: &Store, handles: &Handles, asked: RecordsAggregateAsked) -> Answered {
    let mut aggregate = Aggregate::new().by(asked.by).stats(asked.stats).distinct(asked.distinct);
    if let Some(every) = asked.every {
        aggregate = aggregate.every(Duration::from_millis(every));
    }
    let rows = query(store, handles, &asked.query)?.aggregate(&aggregate)?;
    let rows = rows.into_iter().map(|row| RecordsRow {
        at: unix_nanos(row.at),
        count: row.count,
        by: row.by.into_iter().map(|(key, value)| (key, value.to_string())).collect(),
        stats: row.stats.into_iter().map(|(key, stats)| (key, stats_of(stats))).collect(),
        distinct: row.distinct,
    });
    Ok(RecordsRows { rows: rows.collect() }.encode())
}

fn streams(store: &Store) -> Answered {
    let streams = store.all_events()?.streams()?.into_iter().map(|stream| RecordsStream {
        name: stream.name,
        keep: u64::try_from(stream.keep.as_millis()).unwrap_or(u64::MAX),
        oldest: unix_nanos(stream.oldest),
        newest: unix_nanos(stream.newest),
    });
    Ok(RecordsStreams { streams: streams.collect() }.encode())
}

fn field_names(store: &Store, handles: &Handles, asked: &RecordsFieldNamesAsked) -> Answered {
    let prefix = asked.prefix.as_deref().unwrap_or_default();
    Ok(RecordsNames { names: query(store, handles, &asked.query)?.field_names_prefix(prefix)? }.encode())
}

fn poll(store: &Store, asked: &RecordsPollAsked) -> Answered {
    let all = store.all_events()?;
    let batch = match asked.limit {
        Some(limit) => all.poll_limit(asked.cursor.as_deref(), usize::try_from(limit).unwrap_or(usize::MAX))?,
        None => all.poll(asked.cursor.as_deref())?,
    };
    let events = batch.events.into_iter().map(event_of).collect();
    Ok(RecordsBatch { events, cursor: batch.cursor, expired: batch.expired }.encode())
}

fn damaged(store: &Store) -> Answered {
    let damages = store.all_events()?.damaged()?.into_iter().map(|damage| RecordsDamage {
        part: damage.part.to_string(),
        id: damage.id.unsigned_abs(),
        stream: damage.stream,
        first: unix_nanos(damage.first),
        last: unix_nanos(damage.last),
        error: damage.error,
    });
    Ok(RecordsDamages { damages: damages.collect() }.encode())
}

/// Drops what `damaged` listed: its part and id say what goes.
fn drop_damage(store: &Store, asked: RecordsDamage) -> Answered {
    let part = match asked.part.as_str() {
        "head row" => Part::HeadRow,
        "segment" => Part::Segment,
        other => {
            return Err(Failure::invalid(format!("a damage of part {other:?}, where it is a head row or a segment")));
        }
    };
    let id = i64::try_from(asked.id).map_err(|_| Failure::invalid(format!("a damage of id {}", asked.id)))?;
    let (first, last) = (time_of(asked.first), time_of(asked.last));
    let damage = Damage { part, id, stream: asked.stream, first, last, error: asked.error };
    store.all_events()?.drop(&damage)?;
    Ok(Empty {}.encode())
}

/// The read a query asks for: of the stream its handle names, or of every
/// stream without one.
fn query(store: &Store, handles: &Handles, asked: &RecordsQuery) -> Result<List, Failure> {
    let mut list = match asked.handle {
        Some(handle) => handles.events(handle)?.list(),
        None => store.all_events()?.list(),
    };
    if let Some(since) = asked.since {
        list = list.since(Duration::from_millis(since));
    }
    if let Some(from) = asked.from {
        list = list.from(time_of(from));
    }
    if let Some(to) = asked.to {
        list = list.to(time_of(to));
    }
    if !asked.streams.is_empty() {
        list = list.streams(asked.streams.iter().cloned());
    }
    if !asked.names.is_empty() {
        list = list.names(asked.names.iter().cloned());
    }
    if let Some(level) = asked.min_level {
        list = list.min_level(level_of(level)?);
    }
    if let Some(trace) = &asked.trace_id {
        list = list.trace_id(TraceId(sized(trace)?));
    }
    for (key, json) in &asked.r#where {
        let value =
            RawValue::from_string(json.clone()).map_err(|_| Failure::invalid(format!("where {key:?}: no JSON")))?;
        list = list.filter(key, &value);
    }
    if let Some(text) = &asked.search {
        list = list.search(text);
    }
    if asked.newest {
        list = list.newest();
    }
    if let Some(limit) = asked.limit {
        list = list.limit(usize::try_from(limit).unwrap_or(usize::MAX));
    }
    if let Some(after) = &asked.after {
        list = list.after(after);
    }
    Ok(list)
}

fn event_of(event: Event) -> RecordsEvent {
    RecordsEvent {
        at: unix_nanos(event.at),
        stream: event.stream,
        name: event.name,
        level: event.level.map(|level| i64::from(level.number())),
        message: event.message,
        fields: object_of(&event.fields),
        context: object_of(&event.context),
        trace_id: event.trace_id.map(|trace| trace.0.to_vec()),
        span_id: event.span_id.map(|span| span.0.to_vec()),
    }
}

/// Fields as the JSON object they were written as; none when there is no
/// field.
fn object_of(fields: &Fields) -> Option<String> {
    if fields.is_empty() {
        return None;
    }
    let pairs: Vec<String> =
        fields.iter().map(|(key, json)| format!("{}:{json}", serde_json::to_string(key).unwrap_or_default())).collect();
    Some(format!("{{{}}}", pairs.join(",")))
}

fn stats_of(stats: Stats) -> RecordsStats {
    RecordsStats {
        samples: stats.samples,
        sum: number_of(stats.sum),
        min: number_of(stats.min),
        max: number_of(stats.max),
        avg: stats.avg,
    }
}

/// A number as JSON writes it: an integer exactly, however large, and a
/// float the shortest way that reads back as it.
fn number_of(number: Number) -> String {
    match number {
        Number::Integer(integer) => integer.to_string(),
        Number::Float(float) => {
            serde_json::Number::from_f64(float).map_or_else(|| "null".to_owned(), |n| n.to_string())
        }
    }
}

fn level_of(number: i64) -> Result<Level, Failure> {
    let number =
        i32::try_from(number).map_err(|_| Failure::invalid(format!("level {number}: past what slog counts")))?;
    Ok(Level::of(number))
}

/// An id of `N` bytes: a trace's sixteen, a span's eight.
fn sized<const N: usize>(bytes: &[u8]) -> Result<[u8; N], Failure> {
    bytes.try_into().map_err(|_| Failure::invalid(format!("an id of {} bytes, where it has {N}", bytes.len())))
}

/// A time of nanoseconds since 1970, before it when negative.
fn time_of(nanos: i64) -> SystemTime {
    match u64::try_from(nanos) {
        Ok(after) => UNIX_EPOCH + Duration::from_nanos(after),
        Err(_) => UNIX_EPOCH - Duration::from_nanos(nanos.unsigned_abs()),
    }
}

/// Starts the tail a `records.tail` asks for. It sends nothing until
/// [`Tailing::begin`], once the stream's RESPONSE went out.
pub(crate) fn tail(
    store: &Store,
    handles: &Handles,
    number: u32,
    body: &[u8],
    link: Link,
) -> Result<Arc<Tailing>, Failure> {
    let tail = query(store, handles, &RecordsQuery::decode(body)?)?.tail()?;
    let state = TailState { credit: link.credit, ..TailState::default() };
    let tailing = Arc::new_cyclic(|me: &Weak<Tailing>| {
        let waking = me.clone();
        tail.wake_with(Arc::new(move || {
            if let Some(tailing) = waking.upgrade() {
                tailing.wake();
            }
        }));
        let tails = Arc::downgrade(&handles.tails);
        Tailing { number, tails, link, tail: Mutex::new(Some(tail)), state: Mutex::new(state) }
    });
    lock(&handles.tails).insert(number, Arc::clone(&tailing));
    Ok(tailing)
}

/// A client's tail: a batch each time events arrive, read on the session's
/// workers one read at a time. Nothing is read while batches wait for the
/// client's credit, so a client slower than the writers skips what left the
/// store's hold, as a reader in this process does, and its next batch says
/// how many.
pub(crate) struct Tailing {
    number: u32,
    tails: Weak<Tails>,
    link: Link,
    /// The tail, until the stream ends.
    tail: Mutex<Option<Tail>>,
    state: Mutex<TailState>,
}

#[derive(Default)]
struct TailState {
    credit: u64,
    /// Events may have arrived that no read took yet.
    owed: bool,
    /// A read is queued or under way.
    reading: bool,
    /// Batches read that wait for the client's credit, a message each.
    waiting: VecDeque<Vec<u8>>,
    begun: bool,
    ended: bool,
}

impl Tailing {
    /// Reads a first time: the stream's RESPONSE went out.
    pub(crate) fn begin(self: &Arc<Self>) {
        self.lock().begun = true;
        self.wake();
    }

    /// Credit the client granted the stream: what waited goes, and a read
    /// follows once nothing waits.
    fn grant(self: &Arc<Self>, credit: u32) {
        self.lock().credit += u64::from(credit);
        self.flush();
        self.read_when_free(self.lock());
    }

    /// Events arrived, or the store closed.
    fn wake(self: &Arc<Self>) {
        let mut state = self.lock();
        state.owed = true;
        self.read_when_free(state);
    }

    /// Queues a read on the session's workers, unless one is under way, the
    /// stream has not begun or has ended, or batches wait for credit.
    fn read_when_free(self: &Arc<Self>, mut state: MutexGuard<'_, TailState>) {
        if !state.begun || state.ended || !state.owed || state.reading || !state.waiting.is_empty() {
            return;
        }
        state.reading = true;
        drop(state);
        let tailing = Arc::clone(self);
        (self.link.spawn)(Box::new(move || tailing.read()));
    }

    /// Reads what arrived until nothing more did, or until what was read
    /// waits for credit.
    fn read(&self) {
        loop {
            self.lock().owed = false;
            let read = match lock(&self.tail).as_mut() {
                Some(tail) => tail.next_within(Duration::ZERO),
                None => return,
            };
            match read {
                Ok(Some(batch)) => {
                    let messages = self.messages_of(batch);
                    self.lock().waiting.extend(messages);
                    self.flush();
                }
                Ok(None) => {}
                Err(error) if error.kind() == ErrorKind::Closed => return self.close(Ok(Empty {}.encode())),
                Err(error) => return self.close(Err(Failure::from(error))),
            }
            let mut state = self.lock();
            if state.ended || !state.owed || !state.waiting.is_empty() {
                state.reading = false;
                return;
            }
        }
    }

    /// A batch as messages within half the client's window and the body
    /// agreed, the first saying what was missed; an event that fits no
    /// message alone is missed too.
    fn messages_of(&self, batch: TailBatch) -> Vec<Vec<u8>> {
        let window = usize::try_from(self.link.credit / 2).unwrap_or(usize::MAX);
        let most = self.link.max_body.min(window).saturating_sub(ENVELOPE);
        let (mut messages, mut events, mut size, mut missed) = (Vec::new(), Vec::new(), 0, batch.missed);
        for event in batch.events.into_iter().map(event_of) {
            let weight = event.encode().len();
            if weight > most {
                missed += 1;
                continue;
            }
            if size + weight > most {
                let full = std::mem::take(&mut events);
                messages.push(RecordsTailBatch { events: full, missed: std::mem::take(&mut missed) }.encode());
                size = 0;
            }
            size += weight;
            events.push(event);
        }
        if !events.is_empty() || missed > 0 {
            messages.push(RecordsTailBatch { events, missed }.encode());
        }
        messages
    }

    /// Sends the batches that wait, in order, as far as the client's credit
    /// takes them.
    fn flush(&self) {
        let mut state = self.lock();
        while let Some(size) = state.waiting.front().map(|body| body.len() as u64) {
            if state.ended || size > state.credit {
                return;
            }
            state.credit -= size;
            if let Some(body) = state.waiting.pop_front() {
                (self.link.send)(body);
            }
        }
    }

    /// Ends the tail without a frame: the client cancelled it or left. Says
    /// whether it was still going.
    fn end(&self) -> bool {
        {
            let mut state = self.lock();
            if std::mem::replace(&mut state.ended, true) {
                return false;
            }
            state.waiting.clear();
        }
        drop(lock(&self.tail).take());
        if let Some(tails) = self.tails.upgrade() {
            lock(&tails).remove(&self.number);
        }
        true
    }

    /// Ends the tail and its stream with `last`, unless it ended already.
    fn close(&self, last: Answered) {
        if self.end() {
            (self.link.finish)(last);
        }
    }

    fn lock(&self) -> MutexGuard<'_, TailState> {
        lock(&self.state)
    }
}

/// Begins a writer of lines into the stream a handle names, its pieces to
/// come as the client's DATA.
pub(crate) fn lines(handles: &Handles, number: u32, body: &[u8], link: Link) -> Result<(), Failure> {
    let lines = handles.events(Handle::decode(body)?.handle)?.lines()?;
    lock(&handles.writers).insert(number, Writer { lines, link });
    Ok(())
}

/// A client's writer of lines, and what its stream needs of the session.
struct Writer {
    lines: Lines,
    link: Link,
}

impl Writer {
    /// Ends the writer once the client's last piece came, or one failed:
    /// what it was given is written and waited for, and the stream answers
    /// with what it dropped.
    fn end(self, taken: Result<(), Failure>) {
        let Writer { lines, link } = self;
        let dropped = taken.and_then(|()| lines.end().map_err(Failure::from));
        (link.finish)(dropped.map(|dropped| RecordsDropped { dropped }.encode()));
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[path = "records_tests.rs"]
mod tests;
