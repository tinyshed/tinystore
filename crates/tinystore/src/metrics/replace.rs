//! `replace`: a gauge's samples of a stretch of time swapped for those a
//! collector read again, whether the store had packed them or not (MET-19).
//!
//! ```text
//! the series and its stretch checked ─► what the stretch holds read in a snapshot
//!   ─► the same samples: done, nothing written
//!   ─► else the store's memory held ─► one write: the name and the series settled as an ingest settles them,
//!      what the stretch holds read again, the groups its packed part reaches made again, its part of the head
//!      written again, the series' state
//! ```
//!
//! It is the one call that rewrites what packing sealed, and only a gauge's:
//! a counter's increase is added across blocks. `sealed_before` stays where it
//! was, so the stretch's part behind it goes into blocks and its part after
//! into the head, and every block stays older than the head.
//!
//! Of the groups whose times the packed part reaches, a live block wholly
//! outside it keeps its entry and its payload, as a merge keeps them. The
//! blocks that reach into it are decoded, their samples outside it kept and
//! the stretch's own put in, and cut again as packing cuts: each a new block,
//! its summary built from its samples, its payload a fresh id, the old
//! payloads removed. The groups are made again from those blocks, as many as
//! their bounds take, each new clock held before the old ones are let go.
//!
//! Everything is one write of metrics.db under maintenance's lock, so that no
//! pass packs or expires the series meanwhile, and a read sees the stretch as
//! it was or as it is now, never half.

use std::collections::BTreeMap;
use std::ops::Range;
use std::time::{Duration, SystemTime};

use rusqlite::{Connection, OptionalExtension, params};

use super::Metrics;
use super::budget::Budget;
use super::clock::{self, MOST_BLOCKS, Times};
use super::codec::Point;
use super::delete::RENEW_NEWEST;
use super::directory::{Block, Group, MOST_ENTRIES, Row, entry_len};
use super::engine::{AHEAD, Engine, execute};
use super::head::{self, Extent};
use super::ingest::{
    Batch, ONE_INGEST, POINT_HOLDS, SAMPLE_BYTES, SERIES_HOLDS, SET_NEWEST, State, VERSIONS, ordered, ready, within,
};
use super::instruments::refuse_the_stores_own;
use super::labels::Kept;
use super::ladder::unit_of;
use super::names::{KEEP, checked_keep, millis, write_changes};
use super::pack::{PAYLOAD_ID, block_of, cut, mask};
use super::publish::{earlier, give_clock_and_payloads, insert_group, oldest_block, shrink_head};
use super::registry::{Registering, kind_refused};
use super::retention::{expires, reclaim};
use super::series::{Kind, Series};
use super::snapshot::decode_block;
use super::timestamp::rfc3339;
use super::values::UNTRIED;
use crate::compress::frame::Zstd;
use crate::memory::PATIENCE;
use crate::sqlite::{Tx, sql_error};
use crate::sync::lock;
use crate::{Error, ErrorKind, Reservation, Result, unix_millis};

/// What a replace holds of the store's memory for each sample it decodes,
/// beside the bytes it copied, as a read does.
const SAMPLE_HELD: usize = 32;
/// What its errors and its wait for memory name it.
const REPLACE: &str = "metrics: a replace";

const FIND: &str = "select s.id, n.kind from series s join names n on n.id = s.name where s.identity = ?1";
/// The groups whose times reach into `[?2, ?3)`: from the last that starts
/// at or before `?2`, so that the index walks no older one.
const GROUPS: &str = "select id, start_ts, end_ts, clock, directory from groups where series_id = ?1
    and start_ts >= coalesce((select start_ts from groups where series_id = ?1 and start_ts <= ?2
        order by start_ts desc limit 1), ?2)
    and start_ts < ?3 and end_ts >= ?2 order by start_ts";
const PAYLOAD: &str = "select body from payloads where id = ?1";
const DROP_PAYLOAD: &str = "delete from payloads where id = ?1";
const DROP_GROUP: &str = "delete from groups where id = ?1";
/// `sealed_before` stays: what the replace wrote behind it went into blocks.
const SAVE_STATE: &str = "update series_state set version = version + 1, max_seen = ?2, ready = ?3,
    head_count = ?4, head_start = ?5, head_end = ?6, due = ?7, oldest = ?8 where series_id = ?1";

impl Metrics {
    /// Replaces a gauge's samples of `range`, `[from, to)`, with the series'
    /// own, whether the store had packed them or not: a time given takes its
    /// value, a time not there before is added, and a time of the range the
    /// series does not give goes. It is one write: a read sees the range as it
    /// was or as it is now, never half, and an aggregate over it answers from
    /// the samples given. Sent again with the same samples, it writes nothing.
    ///
    /// ```no_run
    /// # use std::time::{Duration, SystemTime};
    /// # fn main() -> tinystore::Result<()> {
    /// use tinystore::metrics::Series;
    /// let store = tinystore::Store::open("data", Default::default())?;
    /// let stats = store.metrics()?;
    /// let (now, day) = (SystemTime::now(), Duration::from_hours(24));
    /// // the last three days read again from the shop's database, a refund taken off yesterday
    /// let revenue = Series::gauge("revenue").label("shop", "msk");
    /// let corrected = revenue.sample(now - 3 * day, 1000.0).sample(now - 2 * day, 1000.0).sample(now - day, 900.0);
    /// stats.replace(&corrected, now - 3 * day..now)?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// Only a gauge's: a counter's or a histogram's is `invalid`, since its
    /// increase is added across what the store packed. A sample outside the
    /// range is `invalid`, a range that begins before what the name keeps
    /// `too old`, and one that ends more than ten minutes ahead of the store's
    /// clock `too new`. It takes as many samples as one `ingest`, and what the
    /// range holds is bounded as a query is: past either is `limit`.
    pub fn replace(&self, series: &Series, range: Range<SystemTime>) -> Result<()> {
        self.engine.replace(series, range)
    }

    /// A replace checked and its range read, before the call reserves the
    /// store's memory its write holds: an awaited replace waits for those
    /// bytes on no thread, then writes with `replace_prepared`.
    #[cfg(feature = "async")]
    pub(crate) fn prepare_replace(&self, series: &Series, range: Range<SystemTime>) -> Result<Replacing> {
        self.engine.prepare_replace(series, range)
    }

    #[cfg(feature = "async")]
    pub(crate) fn replace_prepared(&self, replacing: Replacing, held: Reservation) -> Result<()> {
        self.engine.replace_prepared(replacing, held)
    }
}

/// A replace checked, and whether a snapshot found its stretch already
/// holding exactly the samples given.
pub(crate) struct Replacing {
    given: Given,
    unchanged: bool,
    /// What its write holds of the store's memory.
    holds: u64,
}

impl Replacing {
    #[cfg(feature = "async")]
    pub(crate) fn holds(&self) -> u64 {
        self.holds
    }
}

impl Engine {
    pub(super) fn replace(&self, series: &Series, range: Range<SystemTime>) -> Result<()> {
        let replacing = self.prepare_replace(series, range)?;
        let held = self.memory.reserve(replacing.holds, REPLACE, PATIENCE)?;
        self.replace_prepared(replacing, held)
    }

    /// The call checked, and what its stretch holds read in one snapshot: a
    /// replace sent again finds the samples it gives there and writes nothing.
    pub(super) fn prepare_replace(&self, series: &Series, range: Range<SystemTime>) -> Result<Replacing> {
        let named = |error: Error| error.within(format!("metrics {series}"));
        let given = Given::of(series, &range).map_err(named)?;
        let keeps = self.names.foreseen(&BTreeMap::from([(series.name(), given.keep)]));
        let keep = keeps.get(series.name()).copied().unwrap_or(KEEP);
        within_keep(given.stretch, keep, self.now()).map_err(named)?;
        let mut budget = Budget::replacing();
        let mut zstd = self.codecs.take();
        let held =
            self.file.read(|connection| Held::read(connection, &mut zstd, &given.kept, given.stretch, &mut budget));
        self.codecs.give_back(zstd);
        let already = match held.map_err(named)? {
            Some(held) => held.holds_exactly(given.stretch, &given.points),
            None => given.points.is_empty(),
        };
        // a keep given is the write's to settle, as an ingest's is: it may move or be refused
        let unchanged = already && given.keep.is_none();
        let holds = budget.bytes
            + budget.decoded * SAMPLE_HELD
            + given.points.len() * POINT_HOLDS
            + given.kept.holds()
            + SERIES_HOLDS;
        Ok(Replacing { given, unchanged, holds: holds as u64 })
    }

    /// Writes a replace once the store's memory holds what its write holds,
    /// `_held`, until the write ends.
    pub(super) fn replace_prepared(&self, replacing: Replacing, _held: Reservation) -> Result<()> {
        if replacing.unchanged {
            return Ok(());
        }
        let Given { kept, keep, stretch, points } = replacing.given;
        let named = format!("metrics {kept}");
        let _one = lock(&self.maintaining);
        let settling = self.names.settling(&BTreeMap::from([(kept.name(), keep)]))?;
        let changes = settling.changes();
        let names = settling.names();
        let name = names.get(kept.name()).ok_or_else(|| Error::internal(format!("{named}: its name")))?;
        let (id, keep) = (name.id, millis(name.keeps()));
        let bytes = kept.bytes() + SAMPLE_BYTES * points.len();
        let mut batch = Batch { identity: kept.identity(), kept, kind: Kind::Gauge, name: id, keep, points };
        let (zstd, max_series) = (self.codecs.take(), self.max_series);
        let written = self.file.write(bytes, move |tx| {
            let mut zstd = zstd;
            let made = write_changes(tx, &changes)?;
            if let Some(id) = made.get(batch.kept.name()) {
                batch.name = *id;
            }
            let ready = rewrite(tx, &mut zstd, max_series, &batch, stretch)?;
            Ok((zstd, ready, made))
        });
        let (zstd, ready, made) = written.map_err(|error| error.within(named))?;
        self.codecs.give_back(zstd);
        settling.settled(made);
        if ready {
            self.wake();
        }
        Ok(())
    }
}

/// A stretch of time, `[from, to)`, in milliseconds.
#[derive(Clone, Copy, Debug)]
struct Stretch {
    from: i64,
    to: i64,
}

impl Stretch {
    fn holds(self, at: i64) -> bool {
        at >= self.from && at < self.to
    }

    /// Its part behind `sealed`, which blocks hold; none when it begins at or
    /// after it, or the series has packed nothing.
    fn packed(self, sealed: Option<i64>) -> Option<Stretch> {
        let sealed = sealed?;
        (self.from < sealed).then(|| Stretch { from: self.from, to: self.to.min(sealed) })
    }

    /// Whether a block's times reach into it.
    fn reaches(self, block: &Block) -> bool {
        block.times.start < self.to && block.times.end >= self.from
    }
}

/// What a replace was given, checked: its series, its stretch, and its
/// samples in the order of their times, of a time given twice the last.
struct Given {
    kept: Kept,
    keep: Option<Duration>,
    stretch: Stretch,
    points: Vec<Point>,
}

impl Given {
    fn of(series: &Series, range: &Range<SystemTime>) -> Result<Given> {
        if series.kind != Kind::Gauge {
            let kind = series.kind;
            let why =
                format!("a {kind}, where replace takes a gauge: a {kind}'s increase adds up what the store packed");
            return Err(Error::invalid(why));
        }
        refuse_the_stores_own(&series.name)?;
        let kept = Kept::of(&series.name, &series.labels)?;
        let keep = series.keep.map(checked_keep).transpose()?;
        let stretch = Stretch { from: unix_millis(range.start), to: unix_millis(range.end) };
        let (from, to) = (rfc3339(stretch.from), rfc3339(stretch.to));
        if stretch.from >= stretch.to {
            return Err(Error::invalid(format!("a range from {from} to {to}, which ends where it begins or before")));
        }
        let samples = series.samples.len();
        within(ONE_INGEST, "a replace", samples, kept.bytes() + SAMPLE_BYTES * samples)?;
        let points = series.samples.iter().map(|sample| Point { at: unix_millis(sample.at), value: sample.value });
        let points = ordered(points.collect());
        if let Some(outside) = points.iter().find(|point| !stretch.holds(point.at)) {
            let at = rfc3339(outside.at);
            return Err(Error::invalid(format!("sample at {at}, outside the range it replaces, from {from} to {to}")));
        }
        Ok(Given { kept, keep, stretch, points })
    }
}

/// Refuses a stretch that begins before what its name keeps, or ends more
/// than ten minutes ahead, as `ingest` refuses such a sample.
fn within_keep(stretch: Stretch, keep: Duration, now: i64) -> Result<()> {
    let (cutoff, horizon) = (now.saturating_sub(millis(keep)), now.saturating_add(millis(AHEAD)));
    if stretch.from < cutoff {
        let why = format!("a range from {}, older than its name keeps", rfc3339(stretch.from));
        return Err(Error::new(ErrorKind::TooOld, why));
    }
    if stretch.to - 1 > horizon {
        let why = format!("a range to {}, more than ten minutes ahead of the store's clock", rfc3339(stretch.to));
        return Err(Error::new(ErrorKind::TooNew, why));
    }
    Ok(())
}

/// The replace's one write, and whether its series' head now holds a block's
/// worth: the series found, or registered when it has none and samples come,
/// and what its stretch holds read again and replaced, unless it is already
/// the samples given.
fn rewrite(tx: &Tx<'_>, zstd: &mut Zstd, max_series: i64, batch: &Batch, stretch: Stretch) -> Result<bool> {
    let mut budget = Budget::replacing();
    let held = match Held::read(tx, zstd, &batch.kept, stretch, &mut budget)? {
        Some(held) if held.holds_exactly(stretch, &batch.points) => return Ok(false),
        Some(held) => held,
        None if batch.points.is_empty() => return Ok(false),
        None => {
            let mut registering = Registering::new(max_series);
            let (id, _) = registering.resolve(tx, batch)?;
            registering.finish(tx)?;
            Held::of(id, State::read(tx, id)?)
        }
    };
    held.replace(tx, zstd, batch, stretch)
}

/// What a series holds of a stretch, read by a snapshot or by the write.
struct Held {
    id: i64,
    state: State,
    /// The groups whose times the stretch's packed part reaches, by their
    /// rows' ids.
    groups: Vec<(i64, Group)>,
    /// Every sample of their live blocks that reach into it, in the order of
    /// their times: those outside it stay when the blocks are made again.
    packed: Vec<Point>,
    /// The head's samples, when the stretch reaches past what is packed.
    head: Vec<Point>,
}

impl Held {
    fn of(id: i64, state: State) -> Held {
        Held { id, state, groups: Vec::new(), packed: Vec::new(), head: Vec::new() }
    }

    /// The series of `kept` and what it holds of `stretch`, its bytes and the
    /// samples it decodes paid from `budget`; none when the store has no such
    /// series. One whose name is of another kind, or that stopped taking
    /// samples, is refused.
    fn read(
        connection: &Connection,
        zstd: &mut Zstd,
        kept: &Kept,
        stretch: Stretch,
        budget: &mut Budget,
    ) -> Result<Option<Held>> {
        let found = connection
            .prepare_cached(FIND)
            .and_then(|mut select| {
                select.query_row(params![kept.identity()], |row| Ok((row.get(0)?, row.get(1)?))).optional()
            })
            .map_err(|error| sql_error("find a series", error))?;
        let Some((id, kind)): Option<(i64, Option<String>)> = found else { return Ok(None) };
        if kind.as_deref() != Some(Kind::Gauge.as_str()) {
            return Err(kind_refused(Kind::Gauge, kind.as_deref()));
        }
        let mut held = Held::of(id, State::read(connection, id)?);
        if let Some(failure) = &held.state.failure {
            return Err(Error::new(ErrorKind::Suspended, format!("it stopped taking samples: {failure}")));
        }
        if let Some(packed) = stretch.packed(held.state.sealed_before) {
            held.read_blocks(connection, zstd, packed, budget)?;
        }
        if held.state.sealed_before.is_none_or(|sealed| stretch.to > sealed) {
            held.read_head(zstd, budget)?;
        }
        Ok(Some(held))
    }

    /// The groups `packed` reaches, and the samples of their live blocks that
    /// reach into it.
    fn read_blocks(
        &mut self,
        connection: &Connection,
        zstd: &mut Zstd,
        packed: Stretch,
        budget: &mut Budget,
    ) -> Result<()> {
        for (row_id, row) in group_rows(connection, self.id, packed)? {
            budget.take_bytes(row.directory.len())?;
            let (length, body) = clock::load(connection, row.clock, budget.bytes_left())?;
            budget.take_bytes(length)?;
            let times = clock::decode(&body.ok_or_else(|| Error::corrupt("a clock past its length"))?)?;
            let group = Group::decode(zstd, &row, times)?;
            for (slot, block) in group.blocks.iter().enumerate() {
                if group.is_live(slot) && packed.reaches(block) {
                    budget.take_decoded(block.times.count)?;
                    decode_reached(connection, zstd, block, budget, &mut self.packed)?;
                }
            }
            self.groups.push((row_id, group));
        }
        Ok(())
    }

    fn read_head(&mut self, zstd: &mut Zstd, budget: &mut Budget) -> Result<()> {
        match (&self.state.head, self.state.extent.count) {
            (None, 0) => Ok(()),
            (Some(bytes), count) if count > 0 => {
                budget.take_bytes(bytes.len())?;
                budget.take_decoded(count)?;
                let chunks = head::parse(self.id, self.state.extent, bytes)?;
                head::decode(zstd, &chunks, i64::MIN, i64::MAX, &mut self.head)
            }
            _ => Err(Error::corrupt("a head that is not its state's")),
        }
    }

    /// Whether the stretch holds exactly `given`, each time and each value's
    /// bits. What blocks hold lies before what the head holds.
    fn holds_exactly(&self, stretch: Stretch, given: &[Point]) -> bool {
        same(self.packed.iter().chain(&self.head).filter(|point| stretch.holds(point.at)), given)
    }

    /// Puts `batch`'s samples in place of what the stretch holds: the blocks
    /// its packed part reaches, then its part of the head, then the series'
    /// state; whether the head now holds a block's worth.
    fn replace(self, tx: &Tx<'_>, zstd: &mut Zstd, batch: &Batch, stretch: Stretch) -> Result<bool> {
        if self.state.version == i64::MAX {
            return Err(VERSIONS.run_out("its version cannot move once more"));
        }
        let sealed = self.state.sealed_before;
        let behind = batch.points.partition_point(|point| sealed.is_some_and(|sealed| point.at < sealed));
        let (packed_part, head_part) = batch.points.split_at(behind);
        if let Some(packed) = stretch.packed(sealed)
            && !same(self.packed.iter().filter(|point| packed.holds(point.at)), packed_part)
        {
            self.rewrite_blocks(tx, zstd, packed, packed_part)?;
        }
        let extent = if same(self.head.iter().filter(|point| stretch.holds(point.at)), head_part) {
            self.state.extent
        } else {
            self.rewrite_head(tx, zstd, stretch, head_part)?
        };
        self.save_state(tx, zstd, batch, extent)
    }

    /// Makes the groups `packed` reaches again: their live blocks wholly
    /// outside it as they are, and in place of those that reach into it new
    /// blocks of their samples outside it and `given`.
    fn rewrite_blocks(&self, tx: &Tx<'_>, zstd: &mut Zstd, packed: Stretch, given: &[Point]) -> Result<()> {
        let outside: Vec<Point> = self.packed.iter().filter(|point| !packed.holds(point.at)).copied().collect();
        let fresh = blocks_of(zstd, &head::merge(&outside, given))?;
        let live = self.groups.iter().flat_map(|(_, group)| {
            group.blocks.iter().enumerate().filter(|(slot, _)| group.is_live(*slot)).map(|(_, block)| block)
        });
        let (mut before, mut after, mut gone) = (Vec::new(), Vec::new(), Vec::new());
        for block in live {
            if packed.reaches(block) {
                gone.extend(block.external().then_some(block.payload));
            } else if block.times.end < packed.from {
                before.push(block.clone());
            } else {
                after.push(block.clone());
            }
        }
        before.extend(fresh);
        before.extend(after);
        let mut groups = grouped(self.id, before);
        for group in &mut groups {
            give_clock_and_payloads(tx, group)?;
        }
        for payload in gone {
            if execute(tx, DROP_PAYLOAD, params![payload], "remove a block's values")? != 1 {
                return Err(Error::corrupt("a block's payload is missing"));
            }
        }
        for (row, group) in &self.groups {
            execute(tx, DROP_GROUP, params![row], "remove a group made again")?;
            clock::let_go(tx, group.clock)?;
        }
        groups.iter().try_for_each(|group| insert_group(tx, zstd, group))
    }

    /// Writes the head with its samples of the stretch swapped for `given`,
    /// and gives its extent.
    fn rewrite_head(&self, tx: &Tx<'_>, zstd: &mut Zstd, stretch: Stretch, given: &[Point]) -> Result<Extent> {
        let outside: Vec<Point> = self.head.iter().filter(|point| !stretch.holds(point.at)).copied().collect();
        let points = head::merge(&outside, given);
        shrink_head(tx, self.id, head::encode(zstd, self.id, &[], &points)?)?;
        Ok(Extent::of(&points))
    }

    /// The series' state after the replace: its version moved, its newest
    /// time seen moved only forward, and its oldest sample and when it is due
    /// found again; a series left with nothing goes, as expiry reclaims one.
    fn save_state(&self, tx: &Tx<'_>, zstd: &mut Zstd, batch: &Batch, extent: Extent) -> Result<bool> {
        let max_seen = batch.points.last().map_or(self.state.max_seen, |point| point.at.max(self.state.max_seen));
        let head = (extent.count > 0).then_some((extent.start, extent.end));
        let block = oldest_block(tx, zstd, self.id)?;
        let Some(oldest) = earlier(block.map(|(start, _)| start), head.map(|(start, _)| start)) else {
            reclaim(tx, self.id)?;
            execute(tx, RENEW_NEWEST, params![batch.name, self.state.max_seen], "find a name's newest sample again")?;
            return Ok(false);
        };
        let due = earlier(block.map(|(_, end)| end), head.map(|(start, _)| start)).map(|at| expires(at, batch.keep));
        let ready = ready(extent, max_seen);
        let saved = params![
            self.id,
            max_seen,
            ready,
            extent.count as i64,
            head.map(|(start, _)| start),
            head.map(|(_, end)| end),
            due,
            oldest
        ];
        execute(tx, SAVE_STATE, saved, "write a series' state")?;
        execute(tx, SET_NEWEST, params![batch.name, max_seen], "move a name's newest sample")?;
        Ok(ready)
    }
}

/// Whether `held` is `given`, each time and each value's bits.
fn same<'p>(mut held: impl Iterator<Item = &'p Point>, given: &[Point]) -> bool {
    let mut given = given.iter();
    loop {
        match (held.next(), given.next()) {
            (None, None) => return true,
            (Some(one), Some(other)) if one.at == other.at && one.value.to_bits() == other.value.to_bits() => {}
            _ => return false,
        }
    }
}

fn group_rows(connection: &Connection, id: i64, packed: Stretch) -> Result<Vec<(i64, Row)>> {
    let failed = |error| sql_error("read a series' groups", error);
    let mut select = connection.prepare_cached(GROUPS).map_err(failed)?;
    let rows = select.query_map(params![id, packed.from, packed.to], |row| {
        let found =
            Row { series: id, start: row.get(1)?, end: row.get(2)?, clock: row.get(3)?, directory: row.get(4)? };
        Ok((row.get::<_, i64>(0)?, found))
    });
    rows.and_then(Iterator::collect).map_err(failed)
}

/// A block's samples onto `out`, its payload fetched and paid for when its
/// body lives in one.
fn decode_reached(
    connection: &Connection,
    zstd: &mut Zstd,
    block: &Block,
    budget: &mut Budget,
    out: &mut Vec<Point>,
) -> Result<()> {
    if !block.external() {
        return decode_block(zstd, block, out);
    }
    budget.take_bytes(block.body_len)?;
    let read = connection
        .prepare_cached(PAYLOAD)
        .and_then(|mut select| select.query_row(params![block.payload], |row| row.get::<_, Vec<u8>>(0)).optional());
    let body = read.map_err(|error| sql_error("read a block's values", error))?;
    let body = body.filter(|body| body.len() == block.body_len);
    let body = body.ok_or_else(|| Error::corrupt("a block's payload is missing or of another length"))?;
    decode_block(zstd, &Block { body, ..block.clone() }, out)
}

/// Samples as new blocks, cut where the units of their ladder begin as
/// packing cuts them, each with its summary.
fn blocks_of(zstd: &mut Zstd, points: &[Point]) -> Result<Vec<Block>> {
    let (Some(first), Some(last)) = (points.first(), points.last()) else { return Ok(Vec::new()) };
    let unit = unit_of(points.len(), first.at, last.at);
    let mut blocks = Vec::new();
    let mut scale = UNTRIED;
    for run in cut(points, unit) {
        let (block, hint) = block_of(zstd, run, Times::of(run), Kind::Gauge, scale)?;
        blocks.push(block);
        scale = hint;
    }
    Ok(blocks)
}

/// Blocks in the order of their times as groups, each as many as one takes:
/// 32 blocks, a clock of 64 KiB and a directory of 8 KiB.
fn grouped(series: i64, blocks: Vec<Block>) -> Vec<Group> {
    let mut groups = Vec::new();
    let mut taken: Vec<Block> = Vec::new();
    let (mut clock_bytes, mut entry_bytes) = (6, 0);
    for block in blocks {
        let clock = block.times.steps.len() + 32;
        // a new block's payload id is given when it is written: room for the longest
        let entry = entry_len(&block) + if block.external() && block.payload == 0 { PAYLOAD_ID } else { 0 };
        let full =
            taken.len() == MOST_BLOCKS || clock_bytes + clock > clock::MOST_BYTES || entry_bytes + entry > MOST_ENTRIES;
        if full && !taken.is_empty() {
            groups.push(group_of(series, std::mem::take(&mut taken)));
            (clock_bytes, entry_bytes) = (6, 0);
        }
        (clock_bytes, entry_bytes) = (clock_bytes + clock, entry_bytes + entry);
        taken.push(block);
    }
    if !taken.is_empty() {
        groups.push(group_of(series, taken));
    }
    groups
}

fn group_of(series: i64, blocks: Vec<Block>) -> Group {
    let (start, end) = (blocks[0].times.start, blocks[blocks.len() - 1].times.end);
    Group { series, start, end, clock: 0, live: mask(blocks.len()), blocks }
}

#[cfg(test)]
#[path = "replace_tests.rs"]
mod tests;
