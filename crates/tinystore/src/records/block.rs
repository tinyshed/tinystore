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

use std::collections::BTreeSet;

use super::bytes::{Reader, crc32, put_count};
use super::event::{Fields, Kept, Level, SpanId, TraceId};
use super::head::{Codec, ROW_EVENTS, Row};
use super::ints::{put_ints, read_ints};
use super::segment::{HAS_CONTEXT, HAS_LEVEL, HAS_MESSAGE, HAS_SPAN, HAS_TRACE, Schema, Shape, WHOLE_FIELDS};
use super::text::{Decoder, Keep, put_texts, read_texts, wants};
use super::values::{put_values, read_values, values_equal};
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

    /// A column of texts, those `keep` drops left empty.
    fn texts(
        &self,
        (column, what): (usize, &'static str),
        count: usize,
        decoder: &mut Decoder<'_>,
        keep: Keep<'_>,
    ) -> Result<Vec<String>> {
        let mut reader = Reader::new(self.columns[column]);
        let read = read_texts(&mut reader, count, decoder, keep);
        reader.finish()?;
        if read.len() == count { Ok(read) } else { Err(Error::corrupt(what)) }
    }

    /// A column of values, one a time of `times`, their events', those
    /// `keep` drops left empty.
    fn values(
        &self,
        (column, what): (usize, &'static str),
        times: &[i64],
        decoder: &mut Decoder<'_>,
        keep: Keep<'_>,
    ) -> Result<Vec<String>> {
        let mut reader = Reader::new(self.columns[column]);
        let read = read_values(&mut reader, times, decoder, keep);
        reader.finish()?;
        if read.len() == times.len() { Ok(read) } else { Err(Error::corrupt(what)) }
    }

    /// Which values of a column, one a time of `times`, are written as `json`.
    fn equal(&self, column: usize, times: &[i64], decoder: &mut Decoder<'_>, json: &str) -> Result<Vec<bool>> {
        let mut reader = Reader::new(self.columns[column]);
        let equal = values_equal(&mut reader, times, decoder, json);
        reader.finish()?;
        if equal.len() == times.len() { Ok(equal) } else { Err(Error::corrupt("a block's values")) }
    }

    /// The id in its segment of each event's name or shape: a segment of one
    /// writes no column of them.
    fn segment_ids(&self, column: usize, of: usize, what: &'static str) -> Result<Vec<i64>> {
        match of {
            1 => self.ints(column, 0, what).map(|_| vec![0; self.count]),
            _ => self.ints(column, self.count, what),
        }
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
    Ok(rebuild(schema, &opened, codec, None)?.into_iter().map(|(_, event)| event).collect())
}

/// The events of a block that `keep` keeps, every one without it, with their
/// places: each column is read whole, and an event dropped is never built.
fn rebuild(schema: &Schema, opened: &Opened<'_>, codec: &mut Codec, keep: Keep<'_>) -> Result<Vec<(usize, Kept)>> {
    let mut columns = Columns::read(schema, opened, codec, keep)?;
    let mut events = Vec::with_capacity(keep.map_or(opened.count, |keep| keep.iter().filter(|kept| **kept).count()));
    for place in 0..opened.count {
        if keep.is_none_or(|keep| keep[place]) {
            events.push((place, columns.event(place)?));
        } else {
            columns.pass(place)?;
        }
    }
    Ok(events)
}

/// A block's columns, read whole and taken an event at a time.
struct Columns<'s> {
    schema: &'s Schema,
    times: Vec<i64>,
    names: Vec<i64>,
    shapes: Vec<&'s Shape>,
    contexts: std::vec::IntoIter<i64>,
    levels: std::vec::IntoIter<i64>,
    messages: std::vec::IntoIter<String>,
    traces: std::vec::IntoIter<[u8; 16]>,
    spans: std::vec::IntoIter<[u8; 8]>,
    whole: std::vec::IntoIter<String>,
    values: Vec<std::vec::IntoIter<String>>,
}

impl<'s> Columns<'s> {
    /// A block's columns, those of the events `keep` drops left empty.
    fn read(schema: &'s Schema, opened: &Opened<'_>, codec: &mut Codec, keep: Keep<'_>) -> Result<Columns<'s>> {
        let count = opened.count;
        let mut decoder = Decoder { codec, left: BLOCK_TEXT };
        let times = opened.ints(TIMES, count, "a block's times")?;
        let names = opened.segment_ids(NAMES, schema.names.len(), "a block's names")?;
        let shapes = shapes_of(schema, &opened.segment_ids(SHAPES, schema.shapes.len(), "a block's shapes")?)?;
        let with = |part: u8| shapes.iter().filter(|shape| shape.has(part)).count();
        // the times, and what a read keeps, of the events that have a value in a column
        let of = |has: &dyn Fn(&Shape) -> bool| {
            let places = (0..count).filter(|place| has(shapes[*place]));
            let (at, kept): (Vec<i64>, Vec<bool>) = places.map(|place| (times[place], wants(keep, place))).unzip();
            (at, keep.map(|_| kept))
        };
        let (said_at, said) = of(&|shape| shape.has(HAS_MESSAGE));
        let (_, whole) = of(&|shape| shape.has(WHOLE_FIELDS));
        let mut values = Vec::with_capacity(schema.columns);
        for column in 0..schema.columns {
            let (at, kept) = of(&|shape| shape.columns.contains(&column));
            values.push(
                opened.values((FIXED + column, "a block's values"), &at, &mut decoder, kept.as_deref())?.into_iter(),
            );
        }
        let messages = opened.values((MESSAGES, "a block's messages"), &said_at, &mut decoder, said.as_deref())?;
        let whole =
            opened.texts((WHOLE, "a block's whole fields"), with(WHOLE_FIELDS), &mut decoder, whole.as_deref())?;
        Ok(Columns {
            schema,
            contexts: opened.ints(CONTEXTS, with(HAS_CONTEXT), "a block's contexts")?.into_iter(),
            levels: opened.ints(LEVELS, with(HAS_LEVEL), "a block's levels")?.into_iter(),
            messages: messages.into_iter(),
            traces: opened.ids::<16>(TRACES, with(HAS_TRACE), "a block's traces")?.into_iter(),
            spans: opened.ids::<8>(SPANS, with(HAS_SPAN), "a block's spans")?.into_iter(),
            whole: whole.into_iter(),
            values,
            times,
            names,
            shapes,
        })
    }

    /// Takes the values of the event at `place` without building it.
    fn pass(&mut self, place: usize) -> Result<()> {
        let shape = self.shapes[place];
        let taken = [
            !shape.has(HAS_CONTEXT) || self.contexts.next().is_some(),
            !shape.has(HAS_LEVEL) || self.levels.next().is_some(),
            !shape.has(HAS_MESSAGE) || self.messages.next().is_some(),
            !shape.has(HAS_TRACE) || self.traces.next().is_some(),
            !shape.has(HAS_SPAN) || self.spans.next().is_some(),
            !shape.has(WHOLE_FIELDS) || self.whole.next().is_some(),
        ];
        let fields = shape.columns.iter().all(|column| self.values[*column].next().is_some());
        if taken.contains(&false) || !fields {
            return Err(broken());
        }
        Ok(())
    }

    /// The event at `place`, built of its values.
    fn event(&mut self, place: usize) -> Result<Kept> {
        let shape = self.shapes[place];
        let schema = self.schema;
        let name = usize::try_from(self.names[place]).ok().and_then(|id| schema.names.get(id)).ok_or_else(broken)?;
        let context = if shape.has(HAS_CONTEXT) {
            let id = self.contexts.next().and_then(|id| usize::try_from(id).ok());
            id.and_then(|id| schema.contexts.get(id)).cloned().ok_or_else(broken)?
        } else {
            Fields::default()
        };
        let level = if shape.has(HAS_LEVEL) {
            Some(self.levels.next().and_then(|number| i32::try_from(number).ok()).ok_or_else(broken)?)
        } else {
            None
        };
        let fields = if shape.has(WHOLE_FIELDS) {
            Fields::read(&self.whole.next().ok_or_else(broken)?).map_err(|_| broken())?
        } else {
            let values = &mut self.values;
            let pairs =
                shape.keys.iter().zip(&shape.columns).map(|(key, column)| Some((key.clone(), values[*column].next()?)));
            Fields::from_pairs(pairs.collect::<Option<_>>().ok_or_else(broken)?)
        };
        if self.times[place] < 0 {
            return Err(Error::corrupt("an event's time before 1970"));
        }
        Ok(Kept {
            at: self.times[place],
            name: name.clone(),
            level: level.map(Level::of),
            message: if shape.has(HAS_MESSAGE) { Some(self.messages.next().ok_or_else(broken)?) } else { None },
            fields,
            context,
            trace_id: if shape.has(HAS_TRACE) { Some(TraceId(self.traces.next().ok_or_else(broken)?)) } else { None },
            span_id: if shape.has(HAS_SPAN) { Some(SpanId(self.spans.next().ok_or_else(broken)?)) } else { None },
        })
    }
}

fn broken() -> Error {
    Error::corrupt("a block whose columns do not hold its events")
}

/// What a read asks of a block's events, which the block answers from the
/// columns it names before it reads the others.
pub(super) struct Wanted<'q> {
    /// The range in nanoseconds, both ends in it.
    pub(super) first: i64,
    pub(super) last: i64,
    pub(super) names: &'q [String],
    pub(super) min_level: Option<Level>,
    pub(super) trace: Option<TraceId>,
    /// Each a key and the JSON a field or the context holds it with.
    pub(super) filters: &'q [(String, String)],
}

/// The events of a block a read may keep, with their places. The times and
/// the columns the read asks about are read first; when they leave no event,
/// the block's other columns are never expanded:
///
/// ```text
/// context host = "nowhere"   times, shapes, contexts → none left → no message or field read
/// ```
pub(super) fn parse_wanted(
    schema: &Schema,
    body: &[u8],
    codec: &mut Codec,
    wanted: &Wanted<'_>,
) -> Result<Vec<(usize, Kept)>> {
    let opened = Opened::of(schema, body)?;
    let keep = kept(schema, &opened, codec, wanted)?;
    if !keep.contains(&true) {
        return Ok(Vec::new());
    }
    rebuild(schema, &opened, codec, Some(&keep))
}

/// Which of a block's events may meet what a read asks, by the columns it
/// names; the read checks those it keeps whole.
fn kept(schema: &Schema, opened: &Opened<'_>, codec: &mut Codec, wanted: &Wanted<'_>) -> Result<Vec<bool>> {
    let times = opened.ints(TIMES, opened.count, "a block's times")?;
    let mut keep: Vec<bool> = times.iter().map(|at| (wanted.first..=wanted.last).contains(at)).collect();
    if !wanted.names.is_empty() {
        let names = opened.segment_ids(NAMES, schema.names.len(), "a block's names")?;
        let name = |id: &i64| usize::try_from(*id).ok().and_then(|id| schema.names.get(id));
        both(&mut keep, names.iter().map(|id| name(id).is_some_and(|name| wanted.names.contains(name))));
    }
    if (wanted.min_level.is_none() && wanted.trace.is_none() && wanted.filters.is_empty()) || !keep.contains(&true) {
        return Ok(keep);
    }
    let shapes = shapes_of(schema, &opened.segment_ids(SHAPES, schema.shapes.len(), "a block's shapes")?)?;
    let with = |part: u8| shapes.iter().filter(|shape| shape.has(part)).count();
    if let Some(least) = wanted.min_level {
        let mut levels = opened.ints(LEVELS, with(HAS_LEVEL), "a block's levels")?.into_iter();
        let grave =
            |shape: &&Shape| shape.has(HAS_LEVEL) && levels.next().is_some_and(|n| n >= i64::from(least.number()));
        both(&mut keep, shapes.iter().map(grave).collect::<Vec<_>>());
    }
    if let Some(trace) = wanted.trace {
        let mut traces = opened.ids::<16>(TRACES, with(HAS_TRACE), "a block's traces")?.into_iter();
        both(
            &mut keep,
            shapes.iter().map(|shape| shape.has(HAS_TRACE) && traces.next() == Some(trace.0)).collect::<Vec<_>>(),
        );
    }
    if !wanted.filters.is_empty() {
        let contexts = opened.ints(CONTEXTS, with(HAS_CONTEXT), "a block's contexts")?;
        let mut decoder = Decoder { codec, left: BLOCK_TEXT };
        for filter in wanted.filters {
            let column = Column { opened, schema, shapes: &shapes, times: &times };
            both(&mut keep, column.holding(&contexts, filter, &mut decoder)?);
        }
    }
    Ok(keep)
}

/// A block's columns as a narrowing reads them.
struct Column<'a, 'b> {
    opened: &'a Opened<'b>,
    schema: &'a Schema,
    shapes: &'a [&'a Shape],
    times: &'a [i64],
}

impl Column<'_, '_> {
    /// Which events hold `key` with the value `json`: in a field or in their
    /// context, or maybe in fields kept whole, which the read checks whole.
    fn holding(
        &self,
        contexts: &[i64],
        (key, json): &(String, String),
        decoder: &mut Decoder<'_>,
    ) -> Result<Vec<bool>> {
        let mut context_ids = contexts.iter();
        let context =
            |id: Option<&i64>| id.and_then(|id| usize::try_from(*id).ok()).and_then(|id| self.schema.contexts.get(id));
        let mut holds: Vec<bool> = self
            .shapes
            .iter()
            .map(|shape| {
                let in_context = shape.has(HAS_CONTEXT)
                    && context(context_ids.next())
                        .is_some_and(|fields| fields.iter().any(|(name, value)| name == key && value == json));
                shape.has(WHOLE_FIELDS) || in_context
            })
            .collect();
        let columns: BTreeSet<usize> = self
            .shapes
            .iter()
            .flat_map(|shape| {
                shape.keys.iter().zip(&shape.columns).filter(|(name, _)| *name == key).map(|(_, column)| *column)
            })
            .collect();
        for column in columns {
            let places: Vec<usize> =
                (0..self.shapes.len()).filter(|place| self.shapes[*place].columns.contains(&column)).collect();
            let at: Vec<i64> = places.iter().map(|place| self.times[*place]).collect();
            let equal = self.opened.equal(FIXED + column, &at, decoder, json)?;
            places.iter().zip(equal).for_each(|(place, equal)| holds[*place] |= equal);
        }
        Ok(holds)
    }
}

/// Keeps what `keep` and `has` both say.
fn both(keep: &mut [bool], has: impl IntoIterator<Item = bool>) {
    keep.iter_mut().zip(has).for_each(|(keep, has)| *keep &= has);
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
