//! What a page takes of one snapshot: a read's series one after another, or
//! an aggregate's groups, each part of a series copied only once the page
//! reaches it, and where the page stopped.
//!
//! A read stops before the part that would pass its `limit` or a bound. An
//! aggregate's group shares what the page has left among its series, takes
//! each as far as its share goes, and ends at the start of the bucket the
//! first of them stopped in, so that every bucket it answers counts every
//! series of the group.
//!
//! ```text
//! a group's series   a ──────────╳            a stopped inside 10:05's bucket
//!                    b ─────────────────╳
//! buckets            │10:00│10:05│10:10│
//! the page           [10:00 ……)               10:00's bucket, whole; next from 10:05
//! ```

use std::collections::BTreeMap;

use rusqlite::{OptionalExtension, params};

use super::{GROUPS, GROUPS_AND_BEFORE, HEAD, Part, Reading, State, Taken, TakenHead, Want, from_the_chunk_before};
use crate::metrics::aggregate::{Op, Shape};
use crate::metrics::budget::Budget;
use crate::metrics::clock;
use crate::metrics::directory::{Group, Row};
use crate::metrics::engine::Engine;
use crate::metrics::head;
use crate::metrics::matching::{self, Found};
use crate::metrics::page::{GroupPlace, Paging, Place};
use crate::metrics::query::Asked;
use crate::metrics::series::Labels;
use crate::sqlite::sql_error;
use crate::{Error, Result};

/// One group of an aggregate as a page took it: its series, each up to where
/// the page ends the group, and whether the page cut it there.
#[derive(Debug)]
pub(in crate::metrics) struct TakenGroup {
    pub(in crate::metrics) name: String,
    pub(in crate::metrics) labels: Labels,
    pub(in crate::metrics) members: Vec<Taken>,
    pub(in crate::metrics) more: bool,
}

/// How far a page lets a series go: a part is taken while what the page
/// decoded is under `decoded` and what it copies stays within `bytes`, and
/// always within the query's bounds.
#[derive(Clone, Copy, Debug)]
struct Room {
    decoded: usize,
    bytes: usize,
}

/// One group of an aggregate's page: its series, where they end, and how
/// many of the page's buckets that may take.
struct Ended {
    members: Vec<Taken>,
    end: i64,
    buckets: usize,
}

impl Engine {
    /// What a page of a read takes, as [`Engine::take`] takes a read's, and
    /// where it stopped: the place the next page begins, none when it took
    /// the rest of the range.
    pub(in crate::metrics) fn take_page(
        &self,
        asked: &Asked,
        paging: &Paging,
        budget: &mut Budget,
    ) -> Result<(Vec<Taken>, Option<Place>)> {
        let from = paging.series_from();
        self.snapshot(asked, Want::Samples, budget, |reading, asked| {
            let (found, beyond) = matching::find_from(reading.connection, asked, from.series, reading.budget)?;
            reading.page(self, found, asked, paging.limit, from).map(|(taken, stopped)| {
                let past = beyond.map(|series| Place { series, at: i64::MIN });
                (taken, stopped.or(past))
            })
        })
    }

    /// What a page of an aggregate takes: its groups in their order from
    /// where the page begins, and where the page after it begins.
    pub(in crate::metrics) fn take_groups(
        &self,
        asked: &Asked,
        shape: &Shape,
        paging: &Paging,
        budget: &mut Budget,
    ) -> Result<(Vec<TakenGroup>, Option<GroupPlace>)> {
        self.snapshot(asked, Want::Buckets(shape), budget, |reading, asked| {
            let found = matching::find(reading.connection, asked, reading.budget)?;
            let groups = grouped(found, shape)?;
            reading.groups_page(self, &groups, asked, shape, paging)
        })
    }
}

/// The series an aggregate found, by the group each falls in, the groups in
/// the order of their names and labels.
fn grouped(found: Vec<Found>, shape: &Shape) -> Result<BTreeMap<(String, Labels), Vec<Found>>> {
    let mut groups: BTreeMap<(String, Labels), Vec<Found>> = BTreeMap::new();
    for series in found {
        shape.check(&series)?;
        let group = shape.group_of(&series).map_err(|error| error.within(&series.kept))?;
        groups.entry(group).or_default().push(series);
    }
    Ok(groups)
}

impl Reading<'_, '_> {
    /// The series of a read's page in the order of their ids, each taken as
    /// far as the page has room; and where it stopped, before the part that
    /// would pass `limit` or a bound of the query.
    fn page(
        &mut self,
        engine: &Engine,
        found: Vec<Found>,
        asked: &Asked,
        limit: usize,
        from: Place,
    ) -> Result<(Vec<Taken>, Option<Place>)> {
        let room = Room { decoded: limit, bytes: usize::MAX };
        let mut taken = Vec::with_capacity(found.len());
        for found in found {
            let at = if found.id == from.series { from.at } else { i64::MIN };
            let mut series = Taken::of(engine, found, asked, at);
            let stopped = self.fill_lazily(&mut series, asked.now, Want::Samples, room);
            let stopped = stopped.map_err(|error| self.met(&series.found, error))?;
            let id = series.found.id;
            taken.push(series);
            if let Some(at) = stopped {
                return Ok((taken, Some(Place { series: id, at })));
            }
        }
        Ok((taken, None))
    }

    /// The groups of an aggregate's page, from the one where it begins: each
    /// whole while the page has room for it, and the one it stops in up to
    /// the start of a bucket.
    fn groups_page(
        &mut self,
        engine: &Engine,
        groups: &BTreeMap<(String, Labels), Vec<Found>>,
        asked: &Asked,
        shape: &Shape,
        paging: &Paging,
    ) -> Result<(Vec<TakenGroup>, Option<GroupPlace>)> {
        let begins = paging.group_from();
        let range = match begins {
            Some(place) => groups.range(place.group.clone()..),
            None => groups.range::<(String, Labels), _>(..),
        };
        let (mut page, mut left) = (Vec::new(), paging.limit);
        for (group, found) in range {
            let at = begins.filter(|place| place.group == *group).map_or(i64::MIN, |place| place.at);
            let place = |at| Some(GroupPlace { group: group.clone(), at });
            if left == 0 {
                return Ok((page, place(at)));
            }
            let Some(ended) = self.group_page(engine, found, asked, shape, (at, left), page.is_empty())? else {
                return Ok((page, place(at)));
            };
            left = left.saturating_sub(ended.buckets);
            let (name, labels) = group.clone();
            let more = ended.end < asked.to;
            page.push(TakenGroup { name, labels, members: ended.members, more });
            if more {
                return Ok((page, place(ended.end)));
            }
        }
        Ok((page, None))
    }

    /// One group of an aggregate's page from `at`, as many as `left` buckets:
    /// its series each taken as far as its share of what the page has left,
    /// ending at the start of the bucket the first of them stopped in. None
    /// when that leaves no whole bucket and the page holds a group already;
    /// on a page of its own, one bucket alone is taken, or is `limit`.
    fn group_page(
        &mut self,
        engine: &Engine,
        found: &[Found],
        asked: &Asked,
        shape: &Shape,
        (at, left): (i64, usize),
        alone: bool,
    ) -> Result<Option<Ended>> {
        let mut members = self.members(engine, found, asked, shape, at);
        let from = members.iter().map(|series| series.window.from).min().unwrap_or(asked.to);
        if from >= asked.to {
            // nothing of the group is left in the range
            return Ok(Some(Ended { members: ended_at(members, asked.to), end: asked.to, buckets: 0 }));
        }
        let most = shape.after_buckets(from, left, asked.to);
        let count = members.len();
        let share = (self.budget.decoded_left() / count, self.budget.bytes_left() / count);
        let mut end = most;
        for series in &mut members {
            series.window.to = most;
            let room = Room { decoded: self.budget.decoded + share.0, bytes: self.budget.bytes + share.1 };
            let stopped = self.fill_lazily(series, asked.now, Want::Buckets(shape), room);
            if let Some(stopped) = stopped.map_err(|error| self.met(&series.found, error))? {
                end = end.min(shape.bucket_of(stopped, from));
            }
        }
        if end > from {
            let buckets = shape.buckets_in(from, end);
            return Ok(Some(Ended { members: ended_at(members, end), end, buckets }));
        }
        drop(members);
        if !alone {
            return Ok(None);
        }
        self.one_bucket(engine, found, asked, shape, (at, from)).map(Some)
    }

    /// A group's series, in the order its buckets join them, each over the
    /// query's window from `at` on.
    fn members(&self, engine: &Engine, found: &[Found], asked: &Asked, shape: &Shape, at: i64) -> Vec<Taken> {
        let mut found = found.to_vec();
        found.sort_by_cached_key(|series| shape.order_of(series));
        found.into_iter().map(|series| Taken::of(engine, series, asked, at)).collect()
    }

    /// The first bucket of a group alone, its series taken whole over it
    /// with the query's bounds and nothing else on the page: past one, it is
    /// `limit`, since a bucket is never split.
    fn one_bucket(
        &mut self,
        engine: &Engine,
        found: &[Found],
        asked: &Asked,
        shape: &Shape,
        (at, from): (i64, i64),
    ) -> Result<Ended> {
        self.budget.reset();
        let end = shape.after_buckets(from, 1, asked.to);
        let mut members = self.members(engine, found, asked, shape, at);
        for series in &mut members {
            series.window.to = end;
            let filled = self.fill(series, asked.now, Want::Buckets(shape));
            filled.map_err(|error| self.met(&series.found, error))?;
        }
        Ok(Ended { members: ended_at(members, end), end, buckets: 1 })
    }

    /// A series' blocks and head a page takes for `want`, and the time it
    /// stopped at when it could not take them all.
    fn fill_lazily(&mut self, taken: &mut Taken, now: i64, want: Want<'_>, room: Room) -> Result<Option<i64>> {
        let (id, window) = (taken.found.id, taken.window);
        let state = self.state(id, window, now, -1)?;
        taken.trimmed = state.trimmed;
        // every block is older than the head, so a head that ends before the
        // window leaves it nothing
        if state.extent.count > 0 && state.extent.end < window.from {
            return Ok(None);
        }
        let (shape, before) = match want {
            Want::Buckets(shape) => {
                // a head that starts before the range holds the sample a first step begins at
                let in_head = state.extent.count > 0 && state.extent.start < window.from;
                let op = shape.series_op(&taken.found);
                (Some((shape, op)), Shape::steps(op) && window.from > window.cutoff && !in_head)
            }
            Want::Samples | Want::Newest => (None, false),
        };
        let stopped = match self.blocks_lazily(taken, shape, before, room)? {
            Some(at) => Some(at),
            None => self.head_lazily(taken, &state, want, room)?,
        };
        taken.until = stopped.unwrap_or(window.to);
        Ok(stopped)
    }

    /// Whether a page lets a series take a part of these costs.
    fn admits(&self, room: Room, bytes: usize, decoded: usize) -> bool {
        let within = self.budget.decoded < room.decoded && self.budget.bytes.saturating_add(bytes) <= room.bytes;
        within && self.budget.fits(bytes, decoded)
    }

    /// The live blocks of a series in its window, as [`Reading::blocks`]
    /// takes them, a group's directory and clock read only once the page
    /// reaches the group; and the time of the first block it could not take.
    fn blocks_lazily(
        &mut self,
        taken: &mut Taken,
        shape: Option<(&Shape, Op)>,
        before: bool,
        room: Room,
    ) -> Result<Option<i64>> {
        let (id, window) = (taken.found.id, taken.window);
        let connection = self.connection;
        let failed = |error| sql_error("read a series' groups", error);
        let mut select = connection.prepare_cached(if before { GROUPS_AND_BEFORE } else { GROUPS }).map_err(failed)?;
        let room_left = self.budget.bytes_left() as i64;
        let mut rows = select.query(params![id, window.from, window.to, window.cutoff, room_left]).map_err(failed)?;
        while let Some(row) = rows.next().map_err(failed)? {
            let start: i64 = row.get(0).map_err(failed)?;
            let length = usize::try_from(row.get::<_, i64>(3).map_err(failed)?).unwrap_or(usize::MAX);
            // a clock is paid at its most, so that one that does not fit never fails the page
            if !self.admits(room, length.saturating_add(clock::MOST_BYTES), 0) {
                return Ok(Some(start.max(window.from)));
            }
            self.budget.take_bytes(length)?;
            let directory: Option<Vec<u8>> = row.get(4).map_err(failed)?;
            let directory = directory.ok_or_else(|| Error::corrupt("a group's directory past its length"))?;
            let (end, clock) = (row.get(1).map_err(failed)?, row.get(2).map_err(failed)?);
            let row = Row { series: id, start, end, clock, directory };
            if let Some(at) = self.group_lazily(taken, &row, shape, before, room)? {
                return Ok(Some(at));
            }
        }
        Ok(None)
    }

    /// The live blocks of one group a page takes, the newest before the
    /// window first when `before` asks for it; and the start of the first it
    /// could not take.
    fn group_lazily(
        &mut self,
        taken: &mut Taken,
        row: &Row,
        shape: Option<(&Shape, Op)>,
        before: bool,
        room: Room,
    ) -> Result<Option<i64>> {
        let window = taken.window;
        let clock = self.clock(row.clock)?;
        let group = Group::decode(self.zstd, row, clock)?;
        for (slot, block) in group.blocks.into_iter().enumerate() {
            if group.live >> slot & 1 == 0 || block.times.start >= window.to {
                continue;
            }
            if block.times.end < window.from {
                if before && block.times.end >= window.cutoff {
                    carry(taken, Part { block, whole: true });
                }
                continue;
            }
            let whole = shape.is_some_and(|(shape, op)| shape.holds(op, &block, window));
            let bytes = if !whole && block.external() { block.body_len } else { 0 };
            let decoded = if whole { 0 } else { block.times.count };
            if !self.admits(room, bytes, decoded) {
                return Ok(Some(block.times.start.max(window.from)));
            }
            taken.blocks.push(self.part(block, whole)?);
        }
        Ok(None)
    }

    /// A series' head a page takes for `want`, chunk by chunk, and the start
    /// of the first chunk it could not take. Its bytes are copied whole, once
    /// the page has room for them.
    fn head_lazily(&mut self, taken: &mut Taken, state: &State, want: Want<'_>, room: Room) -> Result<Option<i64>> {
        let (id, extent, window) = (taken.found.id, state.extent, taken.window);
        if extent.count == 0 || extent.end < window.from || extent.start >= window.to {
            return Ok(None);
        }
        if !self.admits(room, state.head_len, 0) {
            return Ok(Some(extent.start.max(window.from)));
        }
        self.budget.take_bytes(state.head_len)?;
        let bytes = self.head_bytes(id, state.head_len)?;
        let chunks = head::parse(id, extent, &bytes)?;
        let from = match want {
            Want::Buckets(shape) if Shape::steps(shape.series_op(&taken.found)) => {
                from_the_chunk_before(&chunks, window)
            }
            Want::Samples | Want::Buckets(_) | Want::Newest => window.from,
        };
        let mut stopped = None;
        for chunk in chunks.iter().filter(|chunk| chunk.overlaps(from, window.to)) {
            if !self.admits(room, 0, chunk.head.count) {
                stopped = Some(chunk.head.start.max(window.from));
                break;
            }
            self.budget.take_decoded(chunk.head.count)?;
        }
        drop(chunks);
        taken.head = Some(TakenHead { extent, bytes, from });
        Ok(stopped)
    }

    /// A head's bytes, read apart from its state's row for a page, which
    /// copies them only once it reaches them.
    fn head_bytes(&self, id: i64, length: usize) -> Result<Vec<u8>> {
        let asked = params![id, i64::try_from(length).unwrap_or(i64::MAX)];
        let body: Option<Vec<u8>> = self
            .connection
            .prepare_cached(HEAD)
            .and_then(|mut select| select.query_row(asked, |row| row.get(0)).optional())
            .map_err(|error| sql_error("read a series' head", error))?;
        body.filter(|body| body.len() == length).ok_or_else(|| Error::corrupt("a head its state says it has"))
    }
}

/// Keeps the newest block before a series' window first among its parts,
/// in place of an older one: only its last sample counts, where the first
/// step of the window begins.
fn carry(taken: &mut Taken, part: Part) {
    match taken.blocks.last_mut() {
        Some(last) if last.block.times.end < taken.window.from => *last = part,
        _ => taken.blocks.push(part),
    }
}

/// A group's series ended at `end`, the start of a bucket: what each took
/// from there on is the next page's.
fn ended_at(mut members: Vec<Taken>, end: i64) -> Vec<Taken> {
    for series in &mut members {
        let from = series.window.from;
        series.blocks.retain(|part| part.block.times.start < end || part.block.times.end < from);
        (series.window.to, series.until) = (end, end);
    }
    members
}
