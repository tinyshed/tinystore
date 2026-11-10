//! What a page takes of one snapshot: a read's series one after another, or
//! an aggregate's groups, each part of a series copied only once the page
//! reaches it, and where the page stopped.
//!
//! A read stops before the part that would pass its `limit` or a bound, and
//! passes over a part whose samples its window falls between. An
//! aggregate's page holds whole groups, chosen from every series the query
//! finds, as many series as a query finds at most, so that pages go past
//! that bound a group at a time. A group shares what the page has left among
//! its series, takes each as far as its share goes, and ends at the start of
//! the bucket the first of them stopped in, so that every bucket it answers
//! counts every series of the group; or, sooner, at the first bucket with
//! samples past the page's `limit`, which counts no bucket without any. A
//! group with no sample in the range answers nothing and is left out, and a
//! page whose groups all were goes on past them, as a walk of `next` would.
//!
//! ```text
//! a group's series   a ──────────╳            a stopped inside 10:05's bucket
//!                    b ─────────────────╳
//! buckets            │10:00│10:05│10:10│
//! the page           [10:00 ……)               10:00's bucket, whole; next from 10:05
//! ```

use std::collections::BTreeMap;

use rusqlite::{Connection, OptionalExtension, params};

use super::chosen::{Choice, Chosen};
use super::sampled::Sampled;
use super::{
    GROUPS, GROUPS_AND_BEFORE, HEAD, Part, Reading, State, Taken, TakenHead, Want, Window, from_the_chunk_before,
};
use crate::metrics::aggregate::{Op, Shape};
use crate::metrics::budget::Budget;
use crate::metrics::clock::{self, Times};
use crate::metrics::directory::{Group, Row};
use crate::metrics::engine::Engine;
use crate::metrics::head::{self, Chunk};
use crate::metrics::labels;
use crate::metrics::matching::{self, Found};
use crate::metrics::page::{GroupPlace, Paging, Place, Start};
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
/// always within the query's bounds; on an aggregate's page, while it starts
/// before the end the buckets `sampled` give the page.
#[derive(Debug)]
struct Room<'p, 's> {
    decoded: usize,
    bytes: usize,
    sampled: Option<&'p mut Sampled<'s>>,
    /// The series' first part on the page is still to come. Its times are
    /// read even when it is dense, so that the first bucket the page holds
    /// has samples and no page comes back empty.
    first: bool,
}

impl Room<'_, '_> {
    /// Where a series' window ends on the page: at `to`, or before where the
    /// buckets met so far end the page.
    fn end(&self, to: i64) -> i64 {
        self.sampled.as_ref().map_or(to, |sampled| sampled.end().min(to))
    }
}

/// One group of an aggregate's page: its series, where they end, and how
/// many buckets with samples it holds, which the page's `limit` counts.
struct Ended {
    members: Vec<Taken>,
    end: i64,
    buckets: usize,
}

/// What a page took of a group: its buckets up to where they end, or none,
/// since it stopped at the start of the bucket it could not take.
enum Paged {
    Ended(Ended),
    Stopped(i64),
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
    /// where the page begins, and where the page after it begins. When the
    /// groups one query finds hold no sample in the range, the page goes on
    /// past them from its `next`, from a snapshot of its own each time, so
    /// that only the last page takes no group.
    pub(in crate::metrics) fn take_groups(
        &self,
        asked: &Asked,
        shape: &Shape,
        paging: &Paging,
        budget: &mut Budget,
    ) -> Result<(Vec<TakenGroup>, Option<GroupPlace>)> {
        let mut paging = paging.clone();
        loop {
            let (page, next) = self.groups_once(asked, shape, &paging, budget)?;
            match next {
                Some(next) if page.is_empty() => {
                    crate::cancel::check(|| "an aggregate".to_owned())?;
                    // the page holds nothing of the groups it passed over
                    budget.reset();
                    paging.from = Start::Group(next);
                }
                next => return Ok((page, next)),
            }
        }
    }

    /// The groups one snapshot gives a page of an aggregate, and where the
    /// page after it begins.
    fn groups_once(
        &self,
        asked: &Asked,
        shape: &Shape,
        paging: &Paging,
        budget: &mut Budget,
    ) -> Result<(Vec<TakenGroup>, Option<GroupPlace>)> {
        self.snapshot(asked, Want::Buckets(shape), budget, |reading, asked| {
            let most = reading.budget.series_limit();
            let (groups, beyond) = match chosen(reading.connection, asked, shape, paging, most)? {
                Choice::Groups(groups, beyond) => (groups, beyond),
                Choice::TooMany(group, series) => return Err(reading.budget.too_many(series).within(named(&group))),
            };
            reading.budget.take_group(groups.values().map(Vec::len).sum())?;
            let beyond = beyond.map(|group| GroupPlace { group, at: i64::MIN });
            reading.groups_page(self, &groups, asked, shape, paging, beyond)
        })
    }
}

/// The groups of an aggregate's page, from every series its query finds:
/// each is checked against the `op`, and those of groups before the one
/// where the page begins are left out.
fn chosen(
    connection: &Connection,
    asked: &Asked,
    shape: &Shape,
    paging: &Paging,
    most: usize,
) -> Result<Choice<(String, Labels), Found>> {
    let begins = paging.group_from().map(|place| &place.group);
    let mut chosen = Chosen::new(most);
    matching::find_every(connection, asked, |found| {
        for series in found {
            shape.check(&series)?;
            let group = shape.group_of(&series).map_err(|error| error.within(&series.kept))?;
            if begins.is_none_or(|begins| group >= *begins) {
                chosen.add(group, series);
            }
        }
        Ok(())
    })?;
    Ok(chosen.choice())
}

/// A group as Prometheus prints a series: `rpc_ms{fn="lookup"}`.
fn named((name, labels): &(String, Labels)) -> String {
    let mut named = String::new();
    labels::format(&mut named, name, labels.iter()).ok();
    named
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
        let mut room = Room { decoded: limit, bytes: usize::MAX, sampled: None, first: false };
        let mut taken = Vec::with_capacity(found.len());
        for found in found {
            let at = if found.id == from.series { from.at } else { i64::MIN };
            let mut series = Taken::of(engine, found, asked, at);
            let stopped = self.fill_lazily(&mut series, asked.now, Want::Samples, &mut room);
            let stopped = stopped.map_err(|error| self.met(&series.found, error))?;
            let id = series.found.id;
            taken.push(series);
            if let Some(at) = stopped {
                return Ok((taken, Some(Place { series: id, at })));
            }
        }
        Ok((taken, None))
    }

    /// The groups chosen for an aggregate's page, from the one where it
    /// begins: each whole while the page has room for it, and the one it
    /// stops in up to the start of a bucket; past them the page ends at
    /// `beyond`, the first group it could not hold. A group with no sample
    /// in the range is left out, since it answers no bucket.
    fn groups_page(
        &mut self,
        engine: &Engine,
        groups: &BTreeMap<(String, Labels), Vec<Found>>,
        asked: &Asked,
        shape: &Shape,
        paging: &Paging,
        beyond: Option<GroupPlace>,
    ) -> Result<(Vec<TakenGroup>, Option<GroupPlace>)> {
        let begins = paging.group_from();
        let (mut page, mut left) = (Vec::new(), paging.limit);
        for (group, found) in groups {
            let at = begins.filter(|place| place.group == *group).map_or(i64::MIN, |place| place.at);
            let place = |at| Some(GroupPlace { group: group.clone(), at });
            if left == 0 {
                return Ok((page, place(at)));
            }
            let ended = match self.group_page(engine, found, asked, shape, (at, left))? {
                Paged::Ended(ended) => ended,
                Paged::Stopped(_) if !page.is_empty() => return Ok((page, place(at))),
                Paged::Stopped(_) => self.one_bucket(engine, found, asked, shape, at)?,
            };
            if ended.buckets == 0 {
                // it ended with the range, and met no sample on the way
                continue;
            }
            left = left.saturating_sub(ended.buckets);
            let (name, labels) = group.clone();
            let more = ended.end < asked.to;
            page.push(TakenGroup { name, labels, members: ended.members, more });
            if more {
                return Ok((page, place(ended.end)));
            }
        }
        Ok((page, beyond))
    }

    /// One group of an aggregate's page from `at`, as many as `left` buckets
    /// with samples: its series each taken as far as its share of what the
    /// page has left, ending at the first bucket with samples past `left`, at
    /// the range's end, or at the start of the bucket the first of them
    /// stopped in; stopped there when that leaves no bucket with samples.
    fn group_page(
        &mut self,
        engine: &Engine,
        found: &[Found],
        asked: &Asked,
        shape: &Shape,
        (at, left): (i64, usize),
    ) -> Result<Paged> {
        let mut members = self.members(engine, found, asked, shape, at);
        let from = members.iter().map(|series| series.window.from).min().unwrap_or(asked.to);
        if from >= asked.to {
            // nothing of the group is left in the range
            return Ok(Paged::Ended(Ended { members: ended_at(members, asked.to), end: asked.to, buckets: 0 }));
        }
        let count = members.len();
        let share = (self.budget.decoded_left() / count, self.budget.bytes_left() / count);
        let mut sampled = Sampled::new(shape, from, left, asked.to);
        let mut stopped_at = asked.to;
        for series in &mut members {
            series.window.to = sampled.end().min(stopped_at);
            let (decoded, bytes) = (self.budget.decoded + share.0, self.budget.bytes + share.1);
            let mut room = Room { decoded, bytes, sampled: Some(&mut sampled), first: true };
            let stopped = self.fill_lazily(series, asked.now, Want::Buckets(shape), &mut room);
            if let Some(stopped) = stopped.map_err(|error| self.met(&series.found, error))? {
                stopped_at = stopped_at.min(shape.bucket_of(stopped, from));
            }
        }
        let end = sampled.end().min(stopped_at);
        let buckets = sampled.before(end);
        if buckets == 0 && end < asked.to {
            return Ok(Paged::Stopped(end));
        }
        Ok(Paged::Ended(Ended { members: ended_at(members, end), end, buckets }))
    }

    /// A group's series, in the order its buckets join them, each over the
    /// query's window from `at` on.
    fn members(&self, engine: &Engine, found: &[Found], asked: &Asked, shape: &Shape, at: i64) -> Vec<Taken> {
        let mut found = found.to_vec();
        found.sort_by_cached_key(|series| shape.order_of(series));
        found.into_iter().map(|series| Taken::of(engine, series, asked, at)).collect()
    }

    /// The first bucket with samples of a group alone on a page, taken with
    /// the query's whole bounds; when even they stop before it, it is taken
    /// whole, which passes one and is `limit`, since a bucket is never split.
    fn one_bucket(&mut self, engine: &Engine, found: &[Found], asked: &Asked, shape: &Shape, at: i64) -> Result<Ended> {
        self.budget.reset();
        let stopped = match self.group_page(engine, found, asked, shape, (at, 1))? {
            Paged::Ended(ended) => return Ok(ended),
            Paged::Stopped(stopped) => stopped,
        };
        self.budget.reset();
        let end = shape.after_buckets(stopped, 1, asked.to);
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
    fn fill_lazily(&mut self, taken: &mut Taken, now: i64, want: Want<'_>, room: &mut Room) -> Result<Option<i64>> {
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
    fn admits(&self, room: &Room, bytes: usize, decoded: usize) -> bool {
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
        room: &mut Room,
    ) -> Result<Option<i64>> {
        let (id, window) = (taken.found.id, taken.window);
        let connection = self.connection;
        let failed = |error| sql_error("read a series' groups", error);
        let mut select = connection.prepare_cached(if before { GROUPS_AND_BEFORE } else { GROUPS }).map_err(failed)?;
        let room_left = self.budget.bytes_left() as i64;
        let mut rows = select.query(params![id, window.from, window.to, window.cutoff, room_left]).map_err(failed)?;
        while let Some(row) = rows.next().map_err(failed)? {
            let start: i64 = row.get(0).map_err(failed)?;
            if start >= room.end(window.to) {
                break;
            }
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
        room: &mut Room,
    ) -> Result<Option<i64>> {
        let window = taken.window;
        let clock = self.clock(row.clock)?;
        let group = Group::decode(self.zstd, row, clock)?;
        for (slot, block) in group.blocks.into_iter().enumerate() {
            if group.live >> slot & 1 == 0 || block.times.start >= room.end(window.to) {
                continue;
            }
            if block.times.end < window.from {
                if before && block.times.end >= window.cutoff {
                    carry(taken, Part { block, whole: true });
                }
                continue;
            }
            // a read takes no block that holds none of its window's samples
            if shape.is_none() && falls_between(&block.times, window)? {
                continue;
            }
            let whole = shape.is_some_and(|(shape, op)| shape.holds(op, &block, window));
            let bytes = if !whole && block.external() { block.body_len } else { 0 };
            let decoded = if whole { 0 } else { block.times.count };
            if !self.admits(room, bytes, decoded) {
                return Ok(Some(block.times.start.max(window.from)));
            }
            if let Some(sampled) = room.sampled.as_deref_mut() {
                let (from, end, count) = (block.times.start.max(window.from), block.times.end, block.times.count);
                // a block answered whole lies in one bucket of the window
                if whole {
                    sampled.meet(block.times.start);
                } else if room.first || !sampled.meet_run(from, end, count) {
                    // a sample past the window may lie in the bucket the range's end cuts
                    block.times.walk(|_, at| {
                        if (window.from..window.to).contains(&at) {
                            sampled.meet(at)
                        }
                    })?;
                }
                room.first = false;
            }
            taken.blocks.push(self.part(block, whole)?);
        }
        Ok(None)
    }

    /// A series' head a page takes for `want`, chunk by chunk, and the start
    /// of the first chunk it could not take. Its bytes are copied whole, once
    /// the page has room for them.
    fn head_lazily(
        &mut self,
        taken: &mut Taken,
        state: &State,
        want: Want<'_>,
        room: &mut Room,
    ) -> Result<Option<i64>> {
        let (id, extent, window) = (taken.found.id, state.extent, taken.window);
        // a head past where the page's buckets end it is not copied at all
        if extent.count == 0 || extent.end < window.from || extent.start >= room.end(window.to) {
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
            if chunk.head.start >= room.end(window.to) {
                break;
            }
            if matches!(want, Want::Samples) && self.chunk_falls_between(chunk, window)? {
                // the one chunk the window touches: the head holds none of a read's samples
                return Ok(None);
            }
            if !self.admits(room, 0, chunk.head.count) {
                stopped = Some(chunk.head.start.max(window.from));
                break;
            }
            self.budget.take_decoded(chunk.head.count)?;
            let in_window = chunk.head.end >= window.from;
            if let Some(sampled) = room.sampled.as_deref_mut().filter(|_| in_window) {
                let (from, end, count) = (chunk.head.start.max(window.from), chunk.head.end, chunk.head.count);
                if room.first || !sampled.meet_run(from, end, count) {
                    // a chunk's times alone, its values decoded after the read
                    let mut times = Vec::with_capacity(count);
                    chunk.times(self.zstd, &mut times)?;
                    let within = times.iter().filter(|point| (window.from..window.to).contains(&point.at));
                    within.for_each(|point| sampled.meet(point.at));
                }
                room.first = false;
            }
        }
        drop(chunks);
        taken.head = Some(TakenHead { extent, bytes, from });
        Ok(stopped)
    }

    /// Whether a read's window falls between two samples of a head's chunk
    /// that reaches across it, as [`falls_between`] asks of a block.
    fn chunk_falls_between(&mut self, chunk: &Chunk<'_>, window: Window) -> Result<bool> {
        if chunk.head.start >= window.from || chunk.head.end < window.to {
            return Ok(false);
        }
        let mut times = Vec::with_capacity(chunk.head.count);
        chunk.times(self.zstd, &mut times)?;
        Ok(!times.iter().any(|point| (window.from..window.to).contains(&point.at)))
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

/// Whether a read's window falls between two samples of a block it touches
/// that reaches across it, from before it to its end or past, so that the
/// block holds none of the window's samples. One that starts or ends inside
/// the window holds a sample there, its first or its last.
fn falls_between(times: &Times, window: Window) -> Result<bool> {
    if times.start >= window.from || times.end < window.to {
        return Ok(false);
    }
    let mut inside = false;
    times.walk(|_, at| inside |= (window.from..window.to).contains(&at))?;
    Ok(!inside)
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
