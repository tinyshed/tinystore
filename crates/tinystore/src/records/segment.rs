//! The segment row: what every block of a segment shares, written once.
//!
//! ```text
//! names     the events' names, each once: "log", "click"
//! contexts  the events' contexts, each once: a logger's `with` is one for all its lines
//! shapes    what an event carries and the keys of its fields in their order, each once
//!
//! row = version | names, contexts and shapes, through zstd | crc-32
//! ```
//!
//! A shape's keys name the columns its fields are kept in. A column is a key
//! and which time the shape has it, so that a key written twice keeps both
//! places:
//!
//! ```text
//! keys     user  tag  tag
//! columns  0     1    2        where another shape's first "tag" is column 1 too
//! ```
//!
//! A segment gives columns to the first shapes it meets, and keeps the
//! fields of an event of any shape past them whole, as the JSON of an object.
//!
//! Version 1, which v0.1.1 and those before it wrote, has no shape with an id.

use std::collections::HashMap;

use super::event::{Fields, Kept, MAX_EVENT, MAX_KEYS};
use super::head::Codec;
use super::text::{Decoder, put_blob, read_blob};
use crate::compress::checksum::crc32;
use crate::compress::varint::{Reader, put_count, put_text};
use crate::{Error, Result};

pub(super) const VERSION: u8 = 2;
/// v0.1.1's rows, which have no ids and read forever.
pub(super) const VERSION_1: u8 = 1;

/// What an event carries beside its time and its name.
pub(super) const HAS_LEVEL: u8 = 1;
pub(super) const HAS_MESSAGE: u8 = 1 << 1;
pub(super) const HAS_TRACE: u8 = 1 << 2;
pub(super) const HAS_SPAN: u8 = 1 << 3;
pub(super) const HAS_CONTEXT: u8 = 1 << 4;
/// Its fields are kept whole, as an object's JSON, and not in columns.
pub(super) const WHOLE_FIELDS: u8 = 1 << 5;
pub(super) const HAS_ID: u8 = 1 << 6;
const PRESENCE: u8 = (1 << 7) - 1;

/// The shapes a segment gives columns, and the columns it has at most.
const COLUMNED_SHAPES: usize = 64;
const MOST_COLUMNS: usize = 1024;
/// The shapes a segment holds at most, those kept whole among them.
const MOST_SHAPES: usize = COLUMNED_SHAPES + PRESENCE as usize + 1;
/// The names and the contexts a segment holds at most: one an event.
const MOST_EVENTS: usize = 16_384;
/// What a segment row may expand to: its contexts come from at most a
/// segment's weight of events.
const ROW_PLAIN: usize = 8 << 20;

/// What an event carries, and its fields' keys in their order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Shape {
    pub(super) presence: u8,
    pub(super) keys: Vec<String>,
    /// The column of each key.
    pub(super) columns: Vec<usize>,
}

impl Shape {
    pub(super) fn has(&self, part: u8) -> bool {
        self.presence & part != 0
    }
}

/// What a segment's blocks share.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Schema {
    pub(super) names: Vec<String>,
    pub(super) contexts: Vec<Fields>,
    pub(super) shapes: Vec<Shape>,
    /// How many columns the shapes' keys make.
    pub(super) columns: usize,
}

/// Which name, shape and context each event of a segment has; an event
/// without a context has the first, which its shape says not to read.
#[derive(Debug, Default)]
pub(super) struct Ids {
    pub(super) names: Vec<i64>,
    pub(super) shapes: Vec<i64>,
    pub(super) contexts: Vec<i64>,
}

/// What an event carries beside its fields.
fn presence_of(event: &Kept) -> u8 {
    let parts = [
        (event.level.is_some(), HAS_LEVEL),
        (event.message.is_some(), HAS_MESSAGE),
        (event.trace_id.is_some(), HAS_TRACE),
        (event.span_id.is_some(), HAS_SPAN),
        (!event.context.is_empty(), HAS_CONTEXT),
        (event.id.is_some(), HAS_ID),
    ];
    parts.iter().filter(|(has, _)| *has).fold(0, |bits, (_, bit)| bits | bit)
}

impl Schema {
    /// The schema of a segment's events, and which of its names, shapes and
    /// contexts each event has.
    pub(super) fn of(events: &[Kept]) -> (Schema, Ids) {
        let mut schema = Schema::default();
        let mut ids = Ids::default();
        let (mut names, mut contexts, mut shapes) = (HashMap::new(), HashMap::new(), HashMap::new());
        let mut columns: HashMap<(&str, usize), usize> = HashMap::new();
        for event in events {
            let name = *names.entry(event.name.as_str()).or_insert_with(|| {
                schema.names.push(event.name.clone());
                schema.names.len() - 1
            });
            let context = if event.context.is_empty() {
                0
            } else {
                *contexts.entry(&event.context).or_insert_with(|| {
                    schema.contexts.push(event.context.clone());
                    schema.contexts.len() - 1
                })
            };
            let keys: Vec<&str> = event.fields.iter().map(|(key, _)| key).collect();
            let shape = (presence_of(event), keys);
            let shape = match shapes.get(&shape) {
                Some(known) => *known,
                None => {
                    let (presence, keys) = schema.columned_or_whole(shape, &columns);
                    // the shape that keeps this presence's fields whole may be there already
                    *shapes.entry((presence, keys.clone())).or_insert_with(|| {
                        let of_keys = (0..keys.len()).map(|place| {
                            let next = columns.len();
                            *columns.entry((keys[place], occurrence(&keys, place))).or_insert(next)
                        });
                        let of_keys = of_keys.collect();
                        let keys = keys.iter().map(|key| (*key).to_owned()).collect();
                        schema.shapes.push(Shape { presence, keys, columns: of_keys });
                        schema.shapes.len() - 1
                    })
                }
            };
            ids.names.push(name as i64);
            ids.contexts.push(context as i64);
            ids.shapes.push(shape as i64);
        }
        schema.columns = columns.len();
        (schema, ids)
    }

    /// A shape as the segment keeps it: with its keys in columns while the
    /// segment has shapes and columns to give, with its fields whole after.
    fn columned_or_whole<'k>(
        &self,
        (presence, keys): (u8, Vec<&'k str>),
        columns: &HashMap<(&str, usize), usize>,
    ) -> (u8, Vec<&'k str>) {
        let new = (0..keys.len()).filter(|place| !columns.contains_key(&(keys[*place], occurrence(&keys, *place))));
        let columned = self.shapes.iter().filter(|shape| !shape.has(WHOLE_FIELDS)).count();
        if columned < COLUMNED_SHAPES && columns.len() + new.count() <= MOST_COLUMNS {
            (presence, keys)
        } else {
            (presence | WHOLE_FIELDS, Vec::new())
        }
    }

    /// The row of the schema.
    pub(super) fn encode(&self, codec: &mut Codec) -> Result<Vec<u8>> {
        let mut plain = Vec::new();
        put_count(&mut plain, self.names.len() as u64);
        self.names.iter().for_each(|name| put_text(&mut plain, name));
        put_count(&mut plain, self.contexts.len() as u64);
        for context in &self.contexts {
            context.put_pairs(&mut plain);
        }
        put_count(&mut plain, self.shapes.len() as u64);
        for shape in &self.shapes {
            plain.push(shape.presence);
            put_count(&mut plain, shape.keys.len() as u64);
            shape.keys.iter().for_each(|key| put_text(&mut plain, key));
        }
        let mut row = vec![VERSION];
        put_blob(&mut row, &plain, codec)?;
        let sum = crc32(&row);
        row.extend_from_slice(&sum.to_le_bytes());
        Ok(row)
    }

    /// The schema of a row: the one parser of a segment row.
    pub(super) fn parse(row: &[u8], codec: &mut Codec) -> Result<Schema> {
        let Some((framed, sum)) = row.split_last_chunk::<4>() else {
            return Err(Error::corrupt("a segment row shorter than its checksum"));
        };
        let parts = match framed.first() {
            Some(&VERSION) => PRESENCE,
            Some(&VERSION_1) => PRESENCE & !HAS_ID,
            _ => return Err(Error::corrupt("a segment row's version")),
        };
        if crc32(framed) != u32::from_le_bytes(*sum) {
            return Err(Error::corrupt("a segment row's checksum"));
        }
        let mut outer = Reader::new(&framed[1..]);
        let plain = read_blob(&mut outer, &mut Decoder { codec, left: ROW_PLAIN });
        outer.finish()?;
        let mut reader = Reader::new(&plain);
        let mut schema = Schema::default();
        let names = reader.count(MOST_EVENTS);
        schema.names = (0..names).map(|_| reader.text(MAX_EVENT).to_owned()).collect();
        let contexts = reader.count(MOST_EVENTS);
        schema.contexts = (0..contexts).map(|_| Fields::read_pairs(&mut reader)).collect();
        let shapes = reader.count(MOST_SHAPES);
        let mut columns: HashMap<(String, usize), usize> = HashMap::new();
        for _ in 0..shapes {
            let presence = reader.byte();
            if presence & !parts != 0 {
                reader.fail("a shape's presence");
            }
            let count = reader.count(MAX_KEYS);
            let keys: Vec<String> = (0..count).map(|_| reader.text(MAX_EVENT).to_owned()).collect();
            let mut of_keys = Vec::new();
            for (place, key) in keys.iter().enumerate() {
                let which = occurrence(&keys, place);
                let next = columns.len();
                of_keys.push(*columns.entry((key.clone(), which)).or_insert(next));
            }
            schema.shapes.push(Shape { presence, keys, columns: of_keys });
        }
        if columns.len() > MOST_COLUMNS {
            reader.fail("more columns than a segment has");
        }
        schema.columns = columns.len();
        reader.finish().map(|()| schema)
    }
}

/// Which time a shape has the key at `place`.
fn occurrence<K: PartialEq>(keys: &[K], place: usize) -> usize {
    keys[..place].iter().filter(|earlier| **earlier == keys[place]).count()
}

#[cfg(test)]
#[path = "segment_tests.rs"]
mod tests;
