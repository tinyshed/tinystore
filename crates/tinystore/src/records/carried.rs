//! What a stream's events carry, none of it read from a packed event: each
//! name with how many events have it, and each key of their fields and of
//! their contexts with how many events carry it and the kinds of value it
//! holds.
//!
//! ```text
//! orders   order.paid 100
//! fields   amount number 100   currency text 100   order_id integer 100   paid bool 100
//! ```
//!
//! Packing counts a segment's events as it makes the segment, and its keys'
//! rows keep the counts; a merge counts its events again into the holder's,
//! and expiry and a drop remove a segment's rows with it, so that the counts
//! follow what the stream holds. A listing sums its segments' rows and counts
//! the events that wait in its heads from their rows, from one moment of the
//! file. A segment an earlier release packed has no counts: a step of upkeep
//! counts its events from its blocks, and a listing leaves it out till then.
//!
//! What waits is counted at a listing rather than at its append, so that an
//! append pays nothing: a head holds an hour's events at most, or a segment's
//! worth and what a minute adds, and is read at about a microsecond an event.

use std::collections::HashMap;
use std::fmt;
use std::sync::PoisonError;

use rusqlite::{Connection, OptionalExtension, params};

use super::block;
use super::engine::{Records, execute, nanos};
use super::event::{Fields, Kept};
use super::events::AllEvents;
use super::head::{self, Codec, ROW_EVENTS};
use super::pack::{CONTEXT_KEY, FIELD_KEY, HELD, NAME_KEY, PATIENCE, insert_keys};
use super::read::EVENT_HELD;
use super::repair::Part;
use super::segment::Schema;
use crate::Result;
use crate::limits::COUNT_BYTES;
use crate::sqlite::sql_error;

/// What a stream's events carry, by name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StreamFields {
    pub event_names: Vec<NameCount>,
    pub fields: Vec<FieldInfo>,
    pub context: Vec<FieldInfo>,
}

/// A name a stream's events have, and how many have it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NameCount {
    pub name: String,
    pub count: u64,
}

/// A key of a stream's events' fields or contexts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldInfo {
    pub name: String,
    /// The kinds of value it holds, in the order `Kind` lists them.
    pub kinds: Vec<Kind>,
    /// How many events carry it: one that writes it twice, once.
    pub count: u64,
}

/// What a value is, as its JSON is spelled.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    /// A number with a fraction or an exponent: `12.5`, `1e3`.
    Float,
    /// A whole number: `42`, `-7`.
    Integer,
    Text,
    Bool,
    Object,
    List,
    Null,
}

impl Kind {
    const ALL: [Kind; 7] = [Kind::Float, Kind::Integer, Kind::Text, Kind::Bool, Kind::Object, Kind::List, Kind::Null];

    /// The kind of a value's JSON, read from its first character and, for a
    /// number, whether it has a fraction or an exponent.
    pub(super) fn of(json: &str) -> Kind {
        let json = json.trim_start();
        match json.as_bytes().first() {
            Some(b'{') => Kind::Object,
            Some(b'[') => Kind::List,
            Some(b'"') => Kind::Text,
            Some(b't' | b'f') => Kind::Bool,
            Some(b'n') => Kind::Null,
            _ if json.contains(['.', 'e', 'E']) => Kind::Float,
            _ => Kind::Integer,
        }
    }

    /// Its bit in a key's `kinds`, which the file keeps: never moved.
    fn bit(self) -> u8 {
        match self {
            Kind::Float => 1,
            Kind::Integer => 1 << 1,
            Kind::Text => 1 << 2,
            Kind::Bool => 1 << 3,
            Kind::Object => 1 << 4,
            Kind::List => 1 << 5,
            Kind::Null => 1 << 6,
        }
    }

    /// The kinds a key's `kinds` has the bits of.
    fn all_in(bits: u8) -> Vec<Kind> {
        Kind::ALL.into_iter().filter(|kind| bits & kind.bit() != 0).collect()
    }

    /// How the wire and every SDK spell it.
    pub fn name(self) -> &'static str {
        match self {
            Kind::Float => "float",
            Kind::Integer => "integer",
            Kind::Text => "text",
            Kind::Bool => "bool",
            Kind::Object => "object",
            Kind::List => "list",
            Kind::Null => "null",
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl AllEvents {
    /// What the events of `stream` carry, none of it read from a packed
    /// event: each name with how many events have it, and each key of their
    /// fields and of their contexts with the kinds of value it holds and how
    /// many events carry it, each by name. It counts every event the stream
    /// holds, as `streams` finds them: one older than the stream's keep
    /// among them while it shares a segment with newer ones, since expiry
    /// removes a segment whole. A stream with no events carries nothing, and
    /// a name no stream could have is `invalid`.
    ///
    /// ```no_run
    /// # fn main() -> tinystore::Result<()> {
    /// let store = tinystore::Store::open("data", Default::default())?;
    /// let orders = store.all_events()?.fields("orders")?;
    /// for field in &orders.fields {
    ///     println!("{} {:?} in {} events", field.name, field.kinds, field.count);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn fields(&self, stream: &str) -> Result<StreamFields> {
        self.records.carried(stream).map_err(|error| error.within(format!("events {stream}")))
    }
}

/// A key's count in a segment or a listing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Count {
    pub(super) events: u64,
    pub(super) kinds: u8,
    /// The serial of the last event that counted it.
    last: u64,
}

/// A segment's key as its row in `segment_keys` keeps it.
pub(super) type KeyRow = (u8, String, Count);

/// What events carry, counted as they are added; `bytes` is what its keys
/// weigh, which a listing bounds.
#[derive(Debug, Default)]
pub(super) struct Tally {
    names: HashMap<String, Count>,
    fields: HashMap<String, Count>,
    context: HashMap<String, Count>,
    added: u64,
    bytes: usize,
}

/// What a key costs a tally beside its text.
const KEY_HELD: usize = 64;

impl Tally {
    /// Counts an event: its name, and each key of its fields and its
    /// context once, with the kinds of its values.
    pub(super) fn add(&mut self, event: &Kept) {
        self.added += 1;
        let serial = self.added;
        count_of(&mut self.names, &mut self.bytes, &event.name).events += 1;
        add_pairs(&mut self.fields, &mut self.bytes, &event.fields, serial);
        add_pairs(&mut self.context, &mut self.bytes, &event.context, serial);
    }

    /// Adds what a segment's row says of a key.
    fn add_row(&mut self, kind: u8, key: &str, events: u64, kinds: u8) {
        let map = match kind {
            NAME_KEY => &mut self.names,
            FIELD_KEY => &mut self.fields,
            _ => &mut self.context,
        };
        let count = count_of(map, &mut self.bytes, key);
        (count.events, count.kinds) = (count.events + events, count.kinds | kinds);
    }

    /// Its keys as a segment's rows keep them, in their order.
    pub(super) fn rows(self) -> Vec<KeyRow> {
        let kinds = [(NAME_KEY, self.names), (FIELD_KEY, self.fields), (CONTEXT_KEY, self.context)];
        let rows =
            kinds.into_iter().flat_map(|(kind, map)| map.into_iter().map(move |(key, count)| (kind, key, count)));
        let mut rows: Vec<KeyRow> = rows.collect();
        rows.sort_by(|(a, a_key, _), (b, b_key, _)| (a, a_key).cmp(&(b, b_key)));
        rows
    }

    /// What a listing gives, each by name.
    fn carried(self) -> StreamFields {
        let fields = |map: HashMap<String, Count>| {
            let infos = map.into_iter().map(|(name, count)| FieldInfo {
                name,
                kinds: Kind::all_in(count.kinds),
                count: count.events,
            });
            let mut infos: Vec<FieldInfo> = infos.collect();
            infos.sort_by(|a, b| a.name.cmp(&b.name));
            infos
        };
        let names = self.names.into_iter().map(|(name, count)| NameCount { name, count: count.events });
        let mut names: Vec<NameCount> = names.collect();
        names.sort_by(|a, b| a.name.cmp(&b.name));
        StreamFields { event_names: names, fields: fields(self.fields), context: fields(self.context) }
    }

    /// Past the most a listing holds of different keys, `limit`.
    fn bounded(&self) -> Result<()> {
        let most = COUNT_BYTES.bound as usize;
        if self.bytes <= most {
            return Ok(());
        }
        let refused = format!("past the {most} bytes of different keys a stream's fields hold");
        Err(COUNT_BYTES.refuse(self.bytes as u64, refused))
    }
}

/// A key's count in `map`, made at none the first time.
fn count_of<'m>(map: &'m mut HashMap<String, Count>, bytes: &mut usize, key: &str) -> &'m mut Count {
    if !map.contains_key(key) {
        *bytes += key.len() + KEY_HELD;
        map.insert(key.to_owned(), Count::default());
    }
    map.get_mut(key).unwrap_or_else(|| unreachable!("a key just made"))
}

/// Counts the keys of one event's fields or context: each once, with every
/// kind its values have.
fn add_pairs(map: &mut HashMap<String, Count>, bytes: &mut usize, pairs: &Fields, serial: u64) {
    for (key, json) in pairs.iter() {
        let count = count_of(map, bytes, key);
        if count.last != serial {
            (count.events, count.last) = (count.events + 1, serial);
        }
        count.kinds |= Kind::of(json).bit();
    }
}

/// The keys of a stream's segments with their counts, summed: a row a key
/// and the kinds of value one of its segments found it with. A segment an
/// earlier release packed is left out till upkeep counts it.
const SEGMENTS_CARRY: &str = "select k.kind, k.key, k.kinds, sum(k.events)
    from segments s join segment_keys k on k.segment = s.id
    where s.stream = ?1 and s.holder is null and s.last_at >= ?2 and k.events is not null
    group by k.kind, k.key, k.kinds";
const HEADS_OF: &str = "select id, count, body from heads where stream = ?1 and last_at >= ?2";

/// A head row as a listing reads it: its id, its events and its body.
type HeadRow = (i64, u64, Vec<u8>);

impl Records {
    /// What a stream's events carry: read its segments' counts and its head
    /// rows in one moment of the file → count the rows' events after it, a
    /// row at a time, the rows' bytes and a row's events held of the store's
    /// memory as a read's are.
    fn carried(&self, name: &str) -> Result<StreamFields> {
        crate::text::check_name("a stream's", name)?;
        let Some(stream) = self.stream(name) else { return Ok(StreamFields::default()) };
        let cutoff = self.now().saturating_sub(nanos(stream.keeps()));
        let failed = |error| sql_error("read what a stream's events carry", error);
        let (mut tally, heads) = self.file.read(|connection| {
            let tally = segments_carry(connection, stream.id, cutoff)?;
            let mut select = connection.prepare_cached(HEADS_OF).map_err(failed)?;
            let rows = select.query_map(params![stream.id, cutoff], |row| {
                Ok((row.get(0)?, row.get::<_, i64>(1)?.unsigned_abs(), row.get(2)?))
            });
            Ok((tally, rows.and_then(Iterator::collect::<rusqlite::Result<Vec<HeadRow>>>).map_err(failed)?))
        })?;
        let bytes: usize = heads.iter().map(|(_, _, body)| body.len()).sum();
        let row_events = heads.iter().map(|(_, events, _)| *events).max().unwrap_or(0).min(ROW_EVENTS as u64);
        let _held =
            self.memory.reserve_for_read(bytes as u64 + row_events * EVENT_HELD, "records: fields", PATIENCE)?;
        let mut codec = Codec::default();
        for (id, _, body) in heads {
            // a head row that no longer reads is damage, as a read over it lists it
            let events = head::parse(&body, &mut codec).map_err(|error| self.met((Part::HeadRow, id), name, error))?;
            events.iter().for_each(|event| tally.add(event));
            tally.bounded()?;
        }
        Ok(tally.carried())
    }
}

/// The keys of a stream's segments that reach past `cutoff`, with their
/// counts summed.
fn segments_carry(connection: &Connection, stream: i64, cutoff: i64) -> Result<Tally> {
    let failed = |error| sql_error("read what a stream's events carry", error);
    let mut select = connection.prepare_cached(SEGMENTS_CARRY).map_err(failed)?;
    let mut rows = select.query(params![stream, cutoff]).map_err(failed)?;
    let mut tally = Tally::default();
    while let Some(row) = rows.next().map_err(failed)? {
        let (kind, key, kinds): (u8, String, u8) =
            (row.get(0).map_err(failed)?, row.get(1).map_err(failed)?, row.get(2).map_err(failed)?);
        let events: i64 = row.get(3).map_err(failed)?;
        tally.add_row(kind, &key, events.unsigned_abs(), kinds);
        tally.bounded()?;
    }
    Ok(tally)
}

/// The segments one pass of upkeep counts at most, of those an earlier
/// release packed: each read whole, as a merge reads its members.
const UNCOUNTED_A_PASS: usize = 16;
/// The first segments whose keys have no counts, each with what its events
/// weigh; none for one that holds no events of its own.
const UNCOUNTED: &str = "select k.segment, s.weight from
    (select distinct segment from segment_keys where events is null order by segment limit cast(?1 as integer)) k
    left join segments s on s.id = k.segment and s.holder is null";
const HOLDER: &str = "select stream, body from segments where id = ?1 and holder is null";
const BLOCKS: &str = "select id, body from blocks where segment = ?1 order by id";
const IS_HOLDER: &str = "select exists (select 1 from segments where id = ?1 and holder is null)";
const DROP_KEYS: &str = "delete from segment_keys where segment = ?1";

/// A segment to count: its stream, its row and its blocks.
type Uncounted = (i64, Vec<u8>, Vec<(i64, Vec<u8>)>);

impl Records {
    /// A step of upkeep: counts the events of a few segments an earlier
    /// release packed, whose keys have no counts, one at a time and held of
    /// the store's memory as a merge's are. A segment that no longer reads
    /// is said once and passed over. Says how many it counted.
    pub(super) fn count_uncounted(&self) -> Result<usize> {
        let mut counted = 0;
        for (segment, weight) in self.uncounted()? {
            let Some(weight) = weight else {
                self.write_counts(segment, Vec::new())?;
                continue;
            };
            let _held = self.memory.reserve(weight.unsigned_abs() * HELD, "records: counting a segment", PATIENCE)?;
            let Some((stream, row, blocks)) = self.segment_bytes(segment)? else { continue };
            match counts_of(&row, &blocks, &mut Codec::default()) {
                Ok(rows) => {
                    self.write_counts(segment, rows)?;
                    counted += 1;
                }
                Err(error) => drop(self.met((Part::Segment, segment), &self.name_of(stream), error)),
            }
        }
        Ok(counted)
    }

    /// The first segments whose keys have no counts, past those met
    /// damaged, with what each weighs.
    fn uncounted(&self) -> Result<Vec<(i64, Option<i64>)>> {
        let failed = |error| sql_error("find the segments to count", error);
        let passed = self.damaged.lock().unwrap_or_else(PoisonError::into_inner).len();
        let found: Vec<(i64, Option<i64>)> = self.file.read(|connection| {
            let mut select = connection.prepare_cached(UNCOUNTED).map_err(failed)?;
            let found = select.query_map([(UNCOUNTED_A_PASS + passed) as i64], |row| Ok((row.get(0)?, row.get(1)?)));
            found.and_then(Iterator::collect).map_err(failed)
        })?;
        let fine = found.into_iter().filter(|(segment, _)| !self.is_damaged(Part::Segment, *segment));
        Ok(fine.take(UNCOUNTED_A_PASS).collect())
    }

    /// A segment's stream, row and blocks, read in one moment; none once it
    /// holds no events of its own.
    fn segment_bytes(&self, segment: i64) -> Result<Option<Uncounted>> {
        let failed = |error| sql_error("read a segment to count", error);
        self.file.read(|connection| {
            let holder = connection
                .prepare_cached(HOLDER)
                .and_then(|mut select| select.query_row([segment], |row| Ok((row.get(0)?, row.get(1)?))).optional());
            let Some((stream, row)) = holder.map_err(failed)? else { return Ok(None) };
            let mut select = connection.prepare_cached(BLOCKS).map_err(failed)?;
            let blocks = select.query_map([segment], |row| Ok((row.get(0)?, row.get(1)?)));
            Ok(Some((stream, row, blocks.and_then(Iterator::collect).map_err(failed)?)))
        })
    }

    /// Writes a segment's keys with their counts in place of those without,
    /// while it holds its own events; one gone meanwhile keeps no keys.
    fn write_counts(&self, segment: i64, rows: Vec<KeyRow>) -> Result<()> {
        self.file.write(0, move |tx| {
            let is_holder =
                tx.prepare_cached(IS_HOLDER).and_then(|mut select| select.query_row([segment], |row| row.get(0)));
            let is_holder: bool = is_holder.map_err(|error| sql_error("find a segment to count", error))?;
            execute(tx, DROP_KEYS, [segment], "remove a segment's keys without counts")?;
            if is_holder { insert_keys(tx, segment, &rows) } else { Ok(()) }
        })
    }
}

/// A segment's keys with their counts, from its row and its blocks.
fn counts_of(row: &[u8], blocks: &[(i64, Vec<u8>)], codec: &mut Codec) -> Result<Vec<KeyRow>> {
    let schema = Schema::parse(row, codec)?;
    let mut tally = Tally::default();
    for (block, body) in blocks {
        block::parse(&schema, (*block, body), codec)?.iter().for_each(|(_, event)| tally.add(event));
    }
    Ok(tally.rows())
}

#[cfg(test)]
#[path = "carried_tests.rs"]
mod tests;
