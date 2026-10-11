//! The block row: up to 1024 of a segment's events in the order of their
//! times, a column a kind of value, each column on its own:
//!
//! ```text
//! version | count | columns | each column's length | the columns | crc-32
//!
//! times      every event's
//! names      every event's, as its segment numbers them; none of a segment of one name
//! shapes     every event's; none of a segment of one shape
//! contexts   of the events that have one, as their segment numbers them
//! levels     of the events that have one
//! messages   of the events that have one
//! traces     of the events that have one, sixteen bytes each
//! spans      of the events that have one, eight bytes each
//! whole      the fields of the events whose shape has no columns, as an object's JSON
//! then a column a key: the values of the events whose shape has the key
//! ```
//!
//! A column is read with its segment's row and its own bytes, and with no
//! other column: what a read does not ask for is not expanded.

use super::bytes::{Reader, crc32, put_count};
use super::event::{Fields, Kept, Level, SpanId, TraceId};
use super::head::{Codec, ROW_EVENTS, Row};
use super::ints::{put_ints, read_ints};
use super::segment::{HAS_CONTEXT, HAS_LEVEL, HAS_MESSAGE, HAS_SPAN, HAS_TRACE, Schema, Shape, WHOLE_FIELDS};
use super::text::{Decoder, put_texts, read_texts};
use super::values::{put_values, read_values};
use crate::{Error, Result};

/// A block's first byte, which no head row has.
const VERSION: u8 = 2;

const TIMES: usize = 0;
const NAMES: usize = 1;
const SHAPES: usize = 2;
const CONTEXTS: usize = 3;
const LEVELS: usize = 4;
const MESSAGES: usize = 5;
const TRACES: usize = 6;
const SPANS: usize = 7;
const WHOLE: usize = 8;
/// The columns every block has before its keys'.
const FIXED: usize = 9;

/// The text a block's columns may expand to, all together.
const BLOCK_TEXT: usize = 4 << 20;

/// The block of `events`, which are in the order of their times; `ids` say
/// which of the segment's names, shapes and contexts each has.
pub(super) fn encode(schema: &Schema, events: &[Kept], ids: [&[i64]; 3], codec: &mut Codec) -> Result<Row> {
    let [names, shapes, contexts] = ids;
    let mut columns = vec![Vec::new(); FIXED + schema.columns];
    put_ints(&mut columns[TIMES], &events.iter().map(|event| event.at).collect::<Vec<_>>());
    if schema.names.len() > 1 {
        put_ints(&mut columns[NAMES], names);
    }
    if schema.shapes.len() > 1 {
        put_ints(&mut columns[SHAPES], shapes);
    }
    let shape_of = |place: usize| &schema.shapes[shapes[place] as usize];
    let with = |part: u8| (0..events.len()).filter(move |place| shape_of(*place).has(part));
    put_ints(&mut columns[CONTEXTS], &with(HAS_CONTEXT).map(|place| contexts[place]).collect::<Vec<_>>());
    let levels = events.iter().filter_map(|event| event.level).map(|level| i64::from(level.number()));
    put_ints(&mut columns[LEVELS], &levels.collect::<Vec<_>>());
    let said = events.iter().filter_map(|event| Some((event.message.as_deref()?, event.at)));
    let (messages, said_at): (Vec<&str>, Vec<i64>) = said.unzip();
    put_values(&mut columns[MESSAGES], &messages, &said_at, codec)?;
    events.iter().filter_map(|event| event.trace_id).for_each(|trace| columns[TRACES].extend_from_slice(&trace.0));
    events.iter().filter_map(|event| event.span_id).for_each(|span| columns[SPANS].extend_from_slice(&span.0));
    let whole: Vec<String> = with(WHOLE_FIELDS).map(|place| object_of(&events[place].fields)).collect();
    put_texts(&mut columns[WHOLE], &whole.iter().map(String::as_str).collect::<Vec<_>>(), codec)?;

    let mut values: Vec<(Vec<&str>, Vec<i64>)> = vec![Default::default(); schema.columns];
    for (place, event) in events.iter().enumerate() {
        for (column, (_, json)) in shape_of(place).columns.iter().zip(event.fields.iter()) {
            values[*column].0.push(json);
            values[*column].1.push(event.at);
        }
    }
    for (column, (values, times)) in values.iter().enumerate() {
        put_values(&mut columns[FIXED + column], values, times, codec)?;
    }

    let mut body = vec![VERSION];
    put_count(&mut body, events.len() as u64);
    put_count(&mut body, columns.len() as u64);
    columns.iter().for_each(|column| put_count(&mut body, column.len() as u64));
    columns.iter().for_each(|column| body.extend_from_slice(column));
    let sum = crc32(&body);
    body.extend_from_slice(&sum.to_le_bytes());
    let (first, last) = (events.first().map_or(0, |event| event.at), events.last().map_or(0, |event| event.at));
    let levels = events.iter().filter_map(|event| event.level).fold(0, |bits, level| bits | level.bit());
    let weight = events.iter().map(Kept::weight).sum();
    Ok(Row { first, last, levels, count: events.len(), weight, body })
}

/// Fields as the JSON of an object, each value as it was written.
fn object_of(fields: &Fields) -> String {
    let pairs = fields.iter().map(|(key, json)| format!("{}:{json}", serde_json::Value::from(key)));
    format!("{{{}}}", pairs.collect::<Vec<_>>().join(","))
}

/// A block's columns, checked and not read yet.
struct Opened<'b> {
    count: usize,
    columns: Vec<&'b [u8]>,
}

impl<'b> Opened<'b> {
    /// A block's columns by its header: the one parser of a block row.
    fn of(schema: &Schema, body: &'b [u8]) -> Result<Opened<'b>> {
        let Some((framed, sum)) = body.split_last_chunk::<4>() else {
            return Err(Error::corrupt("a block shorter than its checksum"));
        };
        if framed.first() != Some(&VERSION) {
            return Err(Error::corrupt("a block's version"));
        }
        if crc32(framed) != u32::from_le_bytes(*sum) {
            return Err(Error::corrupt("a block's checksum"));
        }
        let mut reader = Reader::new(&framed[1..]);
        let count = reader.count(ROW_EVENTS);
        if reader.count(FIXED + schema.columns) != FIXED + schema.columns {
            reader.fail("a block whose columns are not its segment's");
        }
        let lengths: Vec<usize> = (0..FIXED + schema.columns).map(|_| reader.count(framed.len())).collect();
        let columns = lengths.into_iter().map(|length| reader.take(length)).collect();
        reader.finish()?;
        if count == 0 {
            return Err(Error::corrupt("a block of no event"));
        }
        Ok(Opened { count, columns })
    }

    fn ints(&self, column: usize, count: usize, what: &'static str) -> Result<Vec<i64>> {
        let mut reader = Reader::new(self.columns[column]);
        let read = read_ints(&mut reader, count);
        reader.finish()?;
        if read.len() == count { Ok(read) } else { Err(Error::corrupt(what)) }
    }

    fn texts(&self, column: usize, count: usize, decoder: &mut Decoder<'_>, what: &'static str) -> Result<Vec<String>> {
        let mut reader = Reader::new(self.columns[column]);
        let read = read_texts(&mut reader, count, decoder);
        reader.finish()?;
        if read.len() == count { Ok(read) } else { Err(Error::corrupt(what)) }
    }

    /// A column of values, one a time of `times`, their events'.
    fn values(
        &self,
        column: usize,
        times: &[i64],
        decoder: &mut Decoder<'_>,
        what: &'static str,
    ) -> Result<Vec<String>> {
        let mut reader = Reader::new(self.columns[column]);
        let read = read_values(&mut reader, times, decoder);
        reader.finish()?;
        if read.len() == times.len() { Ok(read) } else { Err(Error::corrupt(what)) }
    }

    /// `count` ids of `N` bytes each.
    fn ids<const N: usize>(&self, column: usize, count: usize, what: &'static str) -> Result<Vec<[u8; N]>> {
        let (ids, rest) = self.columns[column].as_chunks::<N>();
        if ids.len() == count && rest.is_empty() { Ok(ids.to_vec()) } else { Err(Error::corrupt(what)) }
    }
}

/// A block's events in the order of their times.
pub(super) fn parse(schema: &Schema, body: &[u8], codec: &mut Codec) -> Result<Vec<Kept>> {
    let opened = Opened::of(schema, body)?;
    let count = opened.count;
    let mut decoder = Decoder { codec, left: BLOCK_TEXT };
    let times = opened.ints(TIMES, count, "a block's times")?;
    let ids = |column: usize, of: usize, what: &'static str| match of {
        1 => opened.ints(column, 0, what).map(|_| vec![0; count]),
        _ => opened.ints(column, count, what),
    };
    let names = ids(NAMES, schema.names.len(), "a block's names")?;
    let shapes = shapes_of(schema, &ids(SHAPES, schema.shapes.len(), "a block's shapes")?)?;
    let with = |part: u8| shapes.iter().filter(|shape| shape.has(part)).count();

    let mut contexts = opened.ints(CONTEXTS, with(HAS_CONTEXT), "a block's contexts")?.into_iter();
    let mut levels = opened.ints(LEVELS, with(HAS_LEVEL), "a block's levels")?.into_iter();
    let said_at: Vec<i64> =
        shapes.iter().zip(&times).filter(|(shape, _)| shape.has(HAS_MESSAGE)).map(|(_, at)| *at).collect();
    let mut messages = opened.values(MESSAGES, &said_at, &mut decoder, "a block's messages")?.into_iter();
    let mut traces = opened.ids::<16>(TRACES, with(HAS_TRACE), "a block's traces")?.into_iter();
    let mut spans = opened.ids::<8>(SPANS, with(HAS_SPAN), "a block's spans")?.into_iter();
    let mut whole = opened.texts(WHOLE, with(WHOLE_FIELDS), &mut decoder, "a block's whole fields")?.into_iter();
    let mut held: Vec<Vec<i64>> = vec![Vec::new(); schema.columns];
    for (shape, at) in shapes.iter().zip(&times) {
        shape.columns.iter().for_each(|column| held[*column].push(*at));
    }
    let mut values = Vec::with_capacity(schema.columns);
    for (column, times) in held.iter().enumerate() {
        values.push(opened.values(FIXED + column, times, &mut decoder, "a block's values")?.into_iter());
    }

    let broken = || Error::corrupt("a block whose columns do not hold its events");
    let mut events = Vec::with_capacity(count);
    for (place, shape) in shapes.iter().enumerate() {
        let name = usize::try_from(names[place]).ok().and_then(|id| schema.names.get(id)).ok_or_else(broken)?;
        let context = if shape.has(HAS_CONTEXT) {
            let id = contexts.next().and_then(|id| usize::try_from(id).ok());
            id.and_then(|id| schema.contexts.get(id)).cloned().ok_or_else(broken)?
        } else {
            Fields::default()
        };
        let level =
            if shape.has(HAS_LEVEL) { levels.next().and_then(|number| i32::try_from(number).ok()) } else { None };
        if shape.has(HAS_LEVEL) && level.is_none() {
            return Err(broken());
        }
        let fields = if shape.has(WHOLE_FIELDS) {
            Fields::read(&whole.next().ok_or_else(broken)?).map_err(|_| broken())?
        } else {
            let pairs =
                shape.keys.iter().zip(&shape.columns).map(|(key, column)| Some((key.clone(), values[*column].next()?)));
            Fields::from_pairs(pairs.collect::<Option<_>>().ok_or_else(broken)?)
        };
        if times[place] < 0 {
            return Err(Error::corrupt("an event's time before 1970"));
        }
        events.push(Kept {
            at: times[place],
            name: name.clone(),
            level: level.map(Level::of),
            message: if shape.has(HAS_MESSAGE) { Some(messages.next().ok_or_else(broken)?) } else { None },
            fields,
            context,
            trace_id: if shape.has(HAS_TRACE) { Some(TraceId(traces.next().ok_or_else(broken)?)) } else { None },
            span_id: if shape.has(HAS_SPAN) { Some(SpanId(spans.next().ok_or_else(broken)?)) } else { None },
        });
    }
    Ok(events)
}

/// Each column of a block, for a measurement.
#[cfg(test)]
fn columns_of<'b>(schema: &Schema, body: &'b [u8]) -> Result<Vec<&'b [u8]>> {
    Ok(Opened::of(schema, body)?.columns)
}

/// The shape of each event, by its id in the segment.
fn shapes_of<'s>(schema: &'s Schema, ids: &[i64]) -> Result<Vec<&'s Shape>> {
    let shape = |id: &i64| usize::try_from(*id).ok().and_then(|id| schema.shapes.get(id));
    ids.iter().map(shape).collect::<Option<_>>().ok_or_else(|| Error::corrupt("a block's shape past its segment's"))
}

#[cfg(test)]
#[path = "block_tests.rs"]
mod tests;
