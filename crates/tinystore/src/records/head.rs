//! The head row: one stream's events of one write, in the order they came,
//! under zstd. Its stream is its table row's, and nothing else is needed to
//! read it:
//!
//! ```text
//! version | count | zstd frame of the events | crc-32
//! event   = at | name | presence | level | message | trace | span | id | context | fields
//! ```
//!
//! Version 1, which v0.1.1 and those before it wrote, has no event with an id.

use super::event::{Fields, Kept, Level, MAX_EVENT, MAX_ID, SpanId, TraceId};
use crate::compress::checksum::crc32;
use crate::compress::frame::Zstd;
use crate::compress::varint::{Reader, put_count, put_int, put_text};
use crate::{Error, Result};

pub(super) const VERSION: u8 = 2;
/// v0.1.1's rows, which have no ids and read forever.
pub(super) const VERSION_1: u8 = 1;

use super::segment::{HAS_ID, HAS_LEVEL, HAS_MESSAGE, HAS_SPAN, HAS_TRACE};

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
/// How hard the text of a segment is squeezed, which is written once and
/// read for as long as its stream keeps it.
const PACKED_LEVEL: i32 = 7;

/// The zstd contexts of whoever encodes or parses rows, made at their first
/// use and kept for the next.
pub(super) struct Codec {
    rows: Zstd,
    packed: Zstd,
}

impl Default for Codec {
    fn default() -> Self {
        Codec { rows: Zstd::new(LEVEL), packed: Zstd::new(PACKED_LEVEL) }
    }
}

impl Codec {
    fn squeeze(&mut self, plain: &[u8]) -> Result<Vec<u8>> {
        self.rows.squeeze(plain).map_err(|error| error.within("records"))
    }

    /// Squeezes text of a segment, harder than a head row's.
    pub(super) fn squeeze_packed(&mut self, plain: &[u8]) -> Result<Vec<u8>> {
        self.packed.squeeze(plain).map_err(|error| error.within("records"))
    }

    /// Expands a frame into `most` bytes at most: a frame that says more is
    /// refused before anything is allocated for it.
    pub(super) fn expand(&mut self, frame: &[u8], most: usize) -> Result<Vec<u8>> {
        self.rows.expand(frame, most)
    }

    /// Lets the pooled contexts go: maintenance does it on each pass, so that
    /// a store that stopped writing holds none.
    pub(super) fn rest(&mut self) {
        *self = Codec::default();
    }
}

/// A row as its table keeps it.
pub(super) struct Row {
    pub(super) first: i64,
    pub(super) last: i64,
    pub(super) levels: i64,
    pub(super) count: usize,
    /// What its events weigh against a segment's bounds.
    pub(super) weight: usize,
    pub(super) body: Vec<u8>,
}

/// The arrival of the event at `place` in the head row `row`, which orders
/// every event of records.db by when the store wrote it: row ids only grow,
/// every stream and both heads take them in the order commits write rows,
/// and a row holds `ROW_EVENTS` at most. Ids reach 2⁵³ before it overflows.
pub(super) fn arrival(row: i64, place: usize) -> i64 {
    row.wrapping_mul(ROW_EVENTS as i64).wrapping_add(place as i64)
}

/// The row of `events`, which are one row's worth and not none.
pub(super) fn encode(events: &[Kept], codec: &mut Codec) -> Result<Row> {
    let (mut first, mut last, mut levels, mut weight) = (i64::MAX, i64::MIN, 0, 0);
    let mut plain = Vec::new();
    for event in events {
        (first, last) = (first.min(event.at), last.max(event.at));
        levels |= event.level.map_or(0, Level::bit);
        weight += event.weight();
        put_event(&mut plain, event);
    }
    let mut body = vec![VERSION];
    put_count(&mut body, events.len() as u64);
    body.extend_from_slice(&codec.squeeze(&plain)?);
    let sum = crc32(&body);
    body.extend_from_slice(&sum.to_le_bytes());
    Ok(Row { first, last, levels, count: events.len(), weight, body })
}

fn put_event(out: &mut Vec<u8>, event: &Kept) {
    put_int(out, event.at);
    put_text(out, &event.name);
    let presence = [
        (event.level.is_some(), HAS_LEVEL),
        (event.message.is_some(), HAS_MESSAGE),
        (event.trace_id.is_some(), HAS_TRACE),
        (event.span_id.is_some(), HAS_SPAN),
        (event.id.is_some(), HAS_ID),
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
    if let Some(id) = &event.id {
        put_text(out, id);
    }
    event.context.put_pairs(out);
    event.fields.put_pairs(out);
}

/// The events of a row in the order they were written: the one parser of a
/// head row.
pub(super) fn parse(body: &[u8], codec: &mut Codec) -> Result<Vec<Kept>> {
    let Some((framed, sum)) = body.split_last_chunk::<4>() else {
        return Err(Error::corrupt("a row shorter than its checksum"));
    };
    let presence = match framed.first() {
        Some(&VERSION) => HAS_LEVEL | HAS_MESSAGE | HAS_TRACE | HAS_SPAN | HAS_ID,
        Some(&VERSION_1) => HAS_LEVEL | HAS_MESSAGE | HAS_TRACE | HAS_SPAN,
        _ => return Err(Error::corrupt("a row's version")),
    };
    if crc32(framed) != u32::from_le_bytes(*sum) {
        return Err(Error::corrupt("a row's checksum"));
    }
    let mut head = Reader::new(&framed[1..]);
    let count = head.count(ROW_EVENTS);
    let frame = head.rest();
    head.finish()?;
    if count == 0 {
        return Err(Error::corrupt("a row of no event"));
    }
    let plain = codec.expand(frame, ROW_PLAIN)?;
    let mut reader = Reader::new(&plain);
    let events = (0..count).map(|_| read_event(&mut reader, presence)).collect();
    reader.finish().map(|()| events)
}

/// An event of a row whose version lets its events carry `parts`.
fn read_event(reader: &mut Reader<'_>, parts: u8) -> Kept {
    let at = reader.int();
    let name = reader.text(MAX_EVENT).to_owned();
    let presence = reader.byte();
    if presence & !parts != 0 {
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
    let id = has(HAS_ID).then(|| reader.text(MAX_ID).to_owned());
    let context = Fields::read_pairs(reader);
    let fields = Fields::read_pairs(reader);
    if at < 0 {
        reader.fail("an event's time before 1970");
    }
    Kept { at, id, name, level, message, fields, context, trace_id, span_id }
}

fn taken<const N: usize>(reader: &mut Reader<'_>) -> [u8; N] {
    <[u8; N]>::try_from(reader.take(N)).unwrap_or([0; N])
}

#[cfg(test)]
mod tests {
    use super::super::event::NewEvent;
    use super::*;
    use crate::ErrorKind;

    fn events() -> Vec<Kept> {
        let click = NewEvent::new("click").fields(&serde_json::json!({ "x": 812 }));
        let line = NewEvent::new("log")
            .level(Level::WARN)
            .message("slow request")
            .context(&serde_json::json!({ "request": "9f86d0" }))
            .trace(TraceId([7; 16]), SpanId([9; 8]));
        let push = NewEvent::new("push").id("72d3162e-cc78-11e3-81ab-4c9367dc0958");
        vec![click.kept(2_000), line.kept(1_000), push.kept(1_500)]
    }

    #[test]
    fn a_row_gives_back_its_events_in_the_order_they_were_written() {
        let mut codec = Codec::default();
        let written = events();
        let row = encode(&written, &mut codec).unwrap();
        assert_eq!((row.first, row.last, row.count, row.levels), (1_000, 2_000, 3, Level::WARN.bit()));
        assert_eq!(row.weight, written.iter().map(Kept::weight).sum::<usize>());
        assert_eq!(parse(&row.body, &mut codec).unwrap(), written);
    }

    /// What zstd costs a row of one small event, a logger's line, against
    /// the same events left as they are: run by hand with
    /// `--release -- --ignored --nocapture`.
    #[test]
    #[ignore = "a timing, run by hand"]
    fn one_small_event_under_zstd() {
        let line = |n: u32| {
            let fields = serde_json::json!({ "route": "/notes", "ms": 1200 + n, "user": 42 });
            let event = NewEvent::new("log").level(Level::INFO).message("request served").fields(&fields);
            event.kept(1_800_000_000_000_000_000 + i64::from(n))
        };
        let mut codec = Codec::default();
        for count in [1_u32, 8, 64, 1024] {
            let events: Vec<Kept> = (0..count).map(line).collect();
            let mut plain = Vec::new();
            events.iter().for_each(|event| put_event(&mut plain, event));
            let rounds = 200_000 / count;
            let started = std::time::Instant::now();
            let mut bytes = 0;
            for _ in 0..rounds {
                bytes = encode(&events, &mut codec).unwrap().body.len();
            }
            let each = started.elapsed().as_nanos() / u128::from(rounds) / u128::from(count);
            println!(
                "{count:>5} events a row: {:>5.1} B/event plain, {:>5.1} B/event in its row, {each} ns/event",
                plain.len() as f64 / f64::from(count),
                bytes as f64 / f64::from(count)
            );
        }
    }

    /// Events `request` and `signup` at 1 and 2 ns, as v0.1.1 (c107a3ce)
    /// wrote them in a row.
    const VERSION_1_ROW: &str = "010228b52ffd2017b9000002077265717565737400000004067369676e7570000000842ebcf3";

    // contract: REC-01
    #[test]
    fn a_row_of_version_1_reads_as_it_was_written_its_events_without_ids() {
        let row: Vec<u8> = (0..VERSION_1_ROW.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&VERSION_1_ROW[at..at + 2], 16).unwrap())
            .collect();
        let read = parse(&row, &mut Codec::default()).unwrap();
        assert_eq!(read, [NewEvent::new("request").kept(1), NewEvent::new("signup").kept(2)]);
    }

    #[test]
    fn a_changed_or_missing_byte_is_refused() {
        let mut codec = Codec::default();
        let row = encode(&events(), &mut codec).unwrap();
        for at in 0..row.body.len() {
            let mut changed = row.body.clone();
            changed[at] ^= 0x40;
            assert_eq!(parse(&changed, &mut codec).unwrap_err().kind(), ErrorKind::Corrupt, "byte {at}");
            assert_eq!(parse(&row.body[..at], &mut codec).unwrap_err().kind(), ErrorKind::Corrupt, "cut at {at}");
        }
    }
}
