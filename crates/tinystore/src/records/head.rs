//! The head row: one stream's events of one write, in the order they came,
//! under zstd. Its stream is its table row's, and nothing else is needed to
//! read it:
//!
//! ```text
//! version | count | zstd frame of the events | crc-32
//! event   = at | name | presence | level | message | trace | span | context | fields
//! ```

use std::time::{Duration, UNIX_EPOCH};

use super::bytes::{Reader, crc32, put_count, put_int, put_text};
use super::event::{Event, Fields, Level, MAX_EVENT, MAX_KEYS, NewEvent, SpanId, TraceId};
use crate::{Error, Result};

const VERSION: u8 = 1;

const HAS_LEVEL: u8 = 1;
const HAS_MESSAGE: u8 = 1 << 1;
const HAS_TRACE: u8 = 1 << 2;
const HAS_SPAN: u8 = 1 << 3;

/// The events one row holds at most.
pub(super) const ROW_EVENTS: usize = 1024;
/// What a row's events weigh before the row is cut; one event past it still
/// makes a row.
pub(super) const ROW_WEIGHT: usize = 256 << 10;
/// What a row's frame may expand to: a row cut at its weight, or one event of
/// the most an event weighs.
const ROW_PLAIN: usize = ROW_WEIGHT + MAX_EVENT;

/// How hard a head row is squeezed: zstd's default, which the Go engine
/// measured for rows that live an hour.
const LEVEL: i32 = 3;

/// The zstd contexts of whoever encodes or parses rows, made at their first
/// use and kept for the next.
#[derive(Default)]
pub(super) struct Codec {
    compressor: Option<zstd::bulk::Compressor<'static>>,
    decompressor: Option<zstd::bulk::Decompressor<'static>>,
}

impl Codec {
    fn squeeze(&mut self, plain: &[u8]) -> Result<Vec<u8>> {
        let failed = |error| Error::internal("records: zstd could not write a row").with_source(error);
        if self.compressor.is_none() {
            self.compressor = Some(zstd::bulk::Compressor::new(LEVEL).map_err(failed)?);
        }
        self.compressor.as_mut().map_or_else(|| Ok(Vec::new()), |compressor| compressor.compress(plain).map_err(failed))
    }

    /// Expands a frame into `most` bytes at most: a frame that says more is
    /// refused before anything is allocated for it.
    fn expand(&mut self, frame: &[u8], most: usize) -> Result<Vec<u8>> {
        if self.decompressor.is_none() {
            let made = zstd::bulk::Decompressor::new();
            self.decompressor =
                Some(made.map_err(|error| Error::internal("records: zstd could not start").with_source(error))?);
        }
        let Some(decompressor) = self.decompressor.as_mut() else { return Ok(Vec::new()) };
        decompressor.decompress(frame, most).map_err(|_| Error::corrupt("a head row's frame"))
    }

    /// Lets the contexts go, as maintenance does for a store that stopped
    /// writing.
    pub(super) fn rest(&mut self) {
        *self = Codec::default();
    }
}

/// An event past its checks, with its time.
pub(super) struct Checked {
    pub(super) at: i64,
    pub(super) event: NewEvent,
}

/// A row as its table keeps it.
pub(super) struct Row {
    pub(super) first: i64,
    pub(super) last: i64,
    pub(super) levels: i64,
    pub(super) count: usize,
    pub(super) body: Vec<u8>,
}

/// The row of `events`, which are one row's worth and not none.
pub(super) fn encode(events: &[Checked], codec: &mut Codec) -> Result<Row> {
    let (mut first, mut last, mut levels) = (i64::MAX, i64::MIN, 0);
    let mut plain = Vec::new();
    for Checked { at, event } in events {
        (first, last) = (first.min(*at), last.max(*at));
        levels |= event.level.map_or(0, Level::bit);
        put_event(&mut plain, *at, event);
    }
    let mut body = vec![VERSION];
    put_count(&mut body, events.len() as u64);
    body.extend_from_slice(&codec.squeeze(&plain)?);
    let sum = crc32(&body);
    body.extend_from_slice(&sum.to_le_bytes());
    Ok(Row { first, last, levels, count: events.len(), body })
}

fn put_event(out: &mut Vec<u8>, at: i64, event: &NewEvent) {
    put_int(out, at);
    put_text(out, &event.name);
    let presence = [
        (event.level.is_some(), HAS_LEVEL),
        (event.message.is_some(), HAS_MESSAGE),
        (event.trace_id.is_some(), HAS_TRACE),
        (event.span_id.is_some(), HAS_SPAN),
    ];
    out.push(presence.iter().filter(|(has, _)| *has).fold(0, |bits, (_, bit)| bits | bit));
    if let Some(level) = event.level {
        put_int(out, i64::from(level.number()));
    }
    if let Some(message) = &event.message {
        put_text(out, message);
    }
    if let Some(trace) = event.trace_id {
        out.extend_from_slice(&trace.0);
    }
    if let Some(span) = event.span_id {
        out.extend_from_slice(&span.0);
    }
    put_fields(out, &event.context);
    put_fields(out, &event.fields);
}

fn put_fields(out: &mut Vec<u8>, fields: &Fields) {
    put_count(out, fields.len() as u64);
    for (key, json) in fields.iter() {
        put_text(out, key);
        put_text(out, json);
    }
}

/// The events of a row with their times, in the order they were written:
/// the one parser of a head row.
pub(super) fn parse(stream: &str, body: &[u8], codec: &mut Codec) -> Result<Vec<(i64, Event)>> {
    let Some((framed, sum)) = body.split_last_chunk::<4>() else {
        return Err(Error::corrupt("a head row shorter than its checksum"));
    };
    if framed.first() != Some(&VERSION) {
        return Err(Error::corrupt("a head row's version"));
    }
    if crc32(framed) != u32::from_le_bytes(*sum) {
        return Err(Error::corrupt("a head row's checksum"));
    }
    let mut head = Reader::new(&framed[1..]);
    let count = head.count(ROW_EVENTS);
    let frame = head.rest();
    head.finish()?;
    if count == 0 {
        return Err(Error::corrupt("a head row of no event"));
    }
    let plain = codec.expand(frame, ROW_PLAIN)?;
    let mut reader = Reader::new(&plain);
    let events = (0..count).map(|_| read_event(&mut reader, stream)).collect();
    reader.finish().map(|()| events)
}

fn read_event(reader: &mut Reader<'_>, stream: &str) -> (i64, Event) {
    let at = reader.int();
    let name = reader.text(MAX_EVENT).to_owned();
    let presence = reader.byte();
    if presence & !(HAS_LEVEL | HAS_MESSAGE | HAS_TRACE | HAS_SPAN) != 0 {
        reader.fail("an event's presence");
    }
    let has = |bit: u8| presence & bit != 0;
    let level = has(HAS_LEVEL).then(|| reader.int()).map(|number| match i32::try_from(number) {
        Ok(number) => Level::of(number),
        Err(_) => {
            reader.fail("a level out of range");
            Level::INFO
        }
    });
    let message = has(HAS_MESSAGE).then(|| reader.text(MAX_EVENT).to_owned());
    let trace_id = has(HAS_TRACE).then(|| TraceId(taken(reader)));
    let span_id = has(HAS_SPAN).then(|| SpanId(taken(reader)));
    let context = read_fields(reader);
    let fields = read_fields(reader);
    if at < 0 {
        reader.fail("an event's time before 1970");
    }
    let event = Event {
        at: UNIX_EPOCH + Duration::from_nanos(at.max(0) as u64),
        stream: stream.to_owned(),
        name,
        level,
        message,
        fields,
        context,
        trace_id,
        span_id,
    };
    (at, event)
}

fn taken<const N: usize>(reader: &mut Reader<'_>) -> [u8; N] {
    <[u8; N]>::try_from(reader.take(N)).unwrap_or([0; N])
}

fn read_fields(reader: &mut Reader<'_>) -> Fields {
    let count = reader.count(MAX_KEYS);
    let pairs = (0..count).map(|_| (reader.text(MAX_EVENT).to_owned(), reader.text(MAX_EVENT).to_owned()));
    Fields::from_pairs(pairs.collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorKind;

    fn events() -> Vec<Checked> {
        let plain = NewEvent::new("click").fields(&serde_json::json!({ "x": 812 }));
        let line = NewEvent::new("log")
            .level(Level::WARN)
            .message("slow request")
            .context(&serde_json::json!({ "request": "9f86d0" }))
            .trace(TraceId([7; 16]), SpanId([9; 8]));
        vec![Checked { at: 2_000, event: plain }, Checked { at: 1_000, event: line }]
    }

    #[test]
    fn a_row_gives_back_its_events_in_the_order_they_were_written() {
        let mut codec = Codec::default();
        let row = encode(&events(), &mut codec).unwrap();
        assert_eq!((row.first, row.last, row.count, row.levels), (1_000, 2_000, 2, Level::WARN.bit()));
        let read = parse("web", &row.body, &mut codec).unwrap();
        assert_eq!(read.iter().map(|(at, _)| *at).collect::<Vec<_>>(), [2_000, 1_000]);
        let (click, line) = (&read[0].1, &read[1].1);
        assert_eq!((click.stream.as_str(), click.name.as_str(), click.level), ("web", "click", None));
        assert_eq!(click.fields.json("x"), Some("812"));
        assert_eq!((line.level, line.message.as_deref()), (Some(Level::WARN), Some("slow request")));
        assert_eq!(line.context.json("request"), Some(r#""9f86d0""#));
        assert_eq!((line.trace_id, line.span_id), (Some(TraceId([7; 16])), Some(SpanId([9; 8]))));
    }

    #[test]
    fn a_changed_or_missing_byte_is_refused() {
        let mut codec = Codec::default();
        let row = encode(&events(), &mut codec).unwrap();
        for at in 0..row.body.len() {
            let mut changed = row.body.clone();
            changed[at] ^= 0x40;
            assert_eq!(parse("web", &changed, &mut codec).unwrap_err().kind(), ErrorKind::Corrupt, "byte {at}");
            let cut = &row.body[..at];
            assert_eq!(parse("web", cut, &mut codec).unwrap_err().kind(), ErrorKind::Corrupt, "cut at {at}");
        }
    }
}
