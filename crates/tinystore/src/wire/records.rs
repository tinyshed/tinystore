//! records over protocol 2: a stream of events by handle, appended whole and
//! read a page at a time, and every stream read without one. Fields and a
//! context travel as the JSON object each is, every value spelled as it was
//! written, so that what a client wrote comes back byte for byte.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::value::RawValue;

use super::codec::Message;
use super::protocol::{
    Empty, Failure, Handle, RecordsAggregateAsked, RecordsAppend, RecordsBatch, RecordsCount, RecordsEvent,
    RecordsFieldNamesAsked, RecordsNames, RecordsNewEvent, RecordsOpen, RecordsPage, RecordsPollAsked, RecordsQuery,
    RecordsRow, RecordsRows, RecordsStats, RecordsStream, RecordsStreams, RecordsTop, RecordsTopAsked, RecordsTops,
    method,
};
use crate::records::{Aggregate, Event, Events, Fields, Level, List, NewEvent, Number, SpanId, Stats, TraceId};
use crate::{Store, unix_nanos};

pub(crate) type Answered = Result<Vec<u8>, Failure>;

/// The streams a connection opened, by handle.
#[derive(Default)]
pub(crate) struct Handles {
    open: Mutex<HashMap<u64, Events>>,
    last: AtomicU64,
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

#[cfg(test)]
#[path = "records_tests.rs"]
mod tests;
