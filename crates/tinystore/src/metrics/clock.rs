//! The times of a group's blocks, kept apart from their values in a clock
//! that series written at the same ticks share: one row of `clocks`, found by
//! its digest and its bytes compared before it is, counted by the groups that
//! hold it.
//!
//! ```text
//! clock = 1 | blocks | each block: start | end − start | samples | steps' length | steps | crc-32
//! steps = nothing for a block evenly spaced, else the shortest of:
//!   plain       0 | unit | each step in units
//!   runs        1 | unit | each run: its length, its step in units
//!   exceptions  2 | unit | the commonest step | each other: how far after the last, its step
//! ```

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};

use super::binary::{advance, distance};
use super::codec::{MOST_SAMPLES, Point};
use super::engine::{execute, query_one};
use crate::compress::checksum::Crc32;
use crate::compress::varint::{Reader, put_count, put_int};
use crate::sqlite::sql_error;
use crate::{Error, Result};

/// What a clock row holds at most.
pub(super) const MOST_BYTES: usize = 65_536;
/// The blocks a group holds at most.
pub(super) const MOST_BLOCKS: usize = 32;
const VERSION: u8 = 1;
/// What a block's steps hold at most: a varint a step and a few bytes more.
const MOST_STEPS: usize = 10 * MOST_SAMPLES + 32;

const PLAIN: u8 = 0;
const RUNS: u8 = 1;
const EXCEPTIONS: u8 = 2;

/// A block's times as its group's clock keeps them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Times {
    pub(super) start: i64,
    pub(super) end: i64,
    pub(super) count: usize,
    /// Its steps, empty for a block evenly spaced.
    pub(super) steps: Vec<u8>,
}

impl Times {
    pub(super) fn of(points: &[Point]) -> Times {
        let (first, last) = (points[0].at, points[points.len() - 1].at);
        Times { start: first, end: last, count: points.len(), steps: steps_of(points) }
    }

    /// Gives each time of the block to `each`, after checking that they
    /// increase and lead from its start to its end in its count.
    pub(super) fn walk(&self, mut each: impl FnMut(usize, i64)) -> Result<()> {
        if self.count == 0 || self.count > MOST_SAMPLES || self.end < self.start {
            return Err(Error::corrupt("a block's extent"));
        }
        each(0, self.start);
        if self.count == 1 {
            return if self.steps.is_empty() && self.end == self.start {
                Ok(())
            } else {
                Err(Error::corrupt("a block of one time"))
            };
        }
        let mut at = self.start;
        let mut step_to = |index: usize, delta: u64| -> Result<()> {
            if delta == 0 || delta > distance(at, i64::MAX) {
                return Err(Error::corrupt("a clock's times do not increase, or pass the end of time"));
            }
            at = advance(at, delta);
            each(index, at);
            Ok(())
        };
        if self.steps.is_empty() {
            let (span, gaps) = (distance(self.start, self.end), (self.count - 1) as u64);
            if span == 0 || !span.is_multiple_of(gaps) {
                return Err(Error::corrupt("an even clock's step"));
            }
            (1..self.count).try_for_each(|index| step_to(index, span / gaps))?;
        } else {
            walk_steps(&self.steps, self.count, &mut step_to)?;
        }
        if at != self.end {
            return Err(Error::corrupt("a clock that does not end at its block's end"));
        }
        Ok(())
    }

    /// Fills the times of a block's points.
    pub(super) fn fill(&self, run: &mut [Point]) -> Result<()> {
        if run.len() != self.count {
            return Err(Error::corrupt("a block's values are not as many as its times"));
        }
        self.walk(|index, at| run[index].at = at)
    }
}

/// Calls `step_to` with each step of a block of `count` samples, in its
/// units, from its stored steps.
fn walk_steps(steps: &[u8], count: usize, step_to: &mut impl FnMut(usize, u64) -> Result<()>) -> Result<()> {
    let mut reader = Reader::new(steps);
    let mode = reader.byte();
    let unit = reader.uvarint();
    if unit == 0 {
        return Err(Error::corrupt("a clock's unit"));
    }
    let mut scaled = |index: usize, units: u64| match units.checked_mul(unit) {
        Some(delta) => step_to(index, delta),
        None => Err(Error::corrupt("a clock's step overflows")),
    };
    match mode {
        PLAIN => (1..count).try_for_each(|index| scaled(index, reader.uvarint()))?,
        RUNS => {
            let mut index = 1;
            while index < count {
                let run = reader.count(count - index);
                let units = reader.uvarint();
                if run == 0 {
                    return Err(Error::corrupt("an empty run of a clock"));
                }
                (index..index + run).try_for_each(|index| scaled(index, units))?;
                index += run;
            }
        }
        EXCEPTIONS => {
            let step = reader.uvarint();
            let mut exceptions = HashMap::new();
            let mut position = 0_usize;
            while reader.check().is_ok() && !reader.remaining().is_empty() {
                let gap = reader.count(count - 1 - position);
                if gap == 0 {
                    return Err(Error::corrupt("a clock's exception does not move on"));
                }
                position += gap;
                exceptions.insert(position, reader.uvarint());
            }
            (1..count).try_for_each(|index| scaled(index, exceptions.get(&index).copied().unwrap_or(step)))?;
        }
        _ => return Err(Error::corrupt("a clock's mode")),
    }
    reader.finish()
}

/// A block's steps: nothing when they are all one, else the shortest of
/// writing them plain, as runs, or as exceptions to the commonest.
fn steps_of(points: &[Point]) -> Vec<u8> {
    if points.len() < 2 {
        return Vec::new();
    }
    let deltas: Vec<u64> = points.windows(2).map(|pair| distance(pair[0].at, pair[1].at)).collect();
    let mut seen: HashMap<u64, usize> = HashMap::new();
    let mut unit = 0;
    for &delta in &deltas {
        *seen.entry(delta).or_default() += 1;
        unit = common_divisor(unit, delta);
    }
    if seen.len() == 1 {
        return Vec::new();
    }
    let step = seen.iter().max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0))).map_or(0, |(delta, _)| *delta);
    let mut plain = vec![PLAIN];
    put_count(&mut plain, unit);
    deltas.iter().for_each(|delta| put_count(&mut plain, delta / unit));
    let mut runs = vec![RUNS];
    put_count(&mut runs, unit);
    for run in deltas.chunk_by(|a, b| a == b) {
        put_count(&mut runs, run.len() as u64);
        put_count(&mut runs, run[0] / unit);
    }
    let mut exceptions = vec![EXCEPTIONS];
    put_count(&mut exceptions, unit);
    put_count(&mut exceptions, step / unit);
    let mut previous = 0;
    for (index, delta) in deltas.iter().enumerate().filter(|(_, delta)| **delta != step) {
        put_count(&mut exceptions, (index + 1 - previous) as u64);
        put_count(&mut exceptions, delta / unit);
        previous = index + 1;
    }
    [plain, runs, exceptions].into_iter().min_by_key(Vec::len).unwrap_or_default()
}

fn common_divisor(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// A group's clock: each block's extent and steps, behind their count and a
/// checksum.
pub(super) fn encode(blocks: &[Times]) -> Vec<u8> {
    let mut out = vec![VERSION, blocks.len() as u8];
    for block in blocks {
        put_int(&mut out, block.start);
        put_count(&mut out, distance(block.start, block.end));
        put_count(&mut out, block.count as u64);
        put_count(&mut out, block.steps.len() as u64);
        out.extend_from_slice(&block.steps);
    }
    let sum = Crc32::new().update(&out).finish();
    out.extend_from_slice(&sum.to_le_bytes());
    out
}

/// A group's clock read back, each block's times checked.
pub(super) fn decode(body: &[u8]) -> Result<Vec<Times>> {
    decoded(body).map_err(|error| error.within("a clock"))
}

fn decoded(body: &[u8]) -> Result<Vec<Times>> {
    if body.len() < 6 || body.len() > MOST_BYTES {
        return Err(Error::corrupt("its length"));
    }
    let (content, sum) = body.split_at(body.len() - 4);
    if Crc32::new().update(content).finish().to_le_bytes() != sum {
        return Err(Error::corrupt("its checksum"));
    }
    let mut reader = Reader::new(content);
    if reader.byte() != VERSION {
        return Err(Error::corrupt("its version"));
    }
    let count = usize::from(reader.byte());
    if count == 0 || count > MOST_BLOCKS {
        return Err(Error::corrupt("its count of blocks"));
    }
    let mut blocks: Vec<Times> = Vec::with_capacity(count);
    for _ in 0..count {
        let start = reader.int();
        let span = reader.uvarint();
        let samples = reader.count(MOST_SAMPLES);
        let steps = reader.counted(MOST_STEPS).to_vec();
        reader.check()?;
        if span >= distance(start, i64::MAX) || samples == 0 {
            return Err(Error::corrupt("a block's extent"));
        }
        let block = Times { start, end: advance(start, span), count: samples, steps };
        if blocks.last().is_some_and(|before| block.start <= before.end) {
            return Err(Error::corrupt("its blocks overlap"));
        }
        block.walk(|_, _| {})?;
        blocks.push(block);
    }
    reader.finish()?;
    Ok(blocks)
}

const FIND: &str = "select id, body from clocks where digest = ?1";
const HOLD: &str = "update clocks set refs = refs + 1 where id = ?1";
const MAKE: &str = "insert into clocks (digest, refs, body) values (?1, 1, ?2) returning id";
const LET_GO: &str = "update clocks set refs = refs - 1 where id = ?1 and refs > 0";
const FORGET: &str = "delete from clocks where id = ?1 and refs = 0";
const LOAD: &str = "select length(body), case when length(body) <= ?2 then body end from clocks where id = ?1";

/// The id of the clock of `body`, one more group holding it: the one the
/// file has, its bytes compared, or a new one.
pub(super) fn hold(connection: &Connection, body: &[u8]) -> Result<i64> {
    let digest: [u8; 32] = Sha256::digest(body).into();
    let failed = |error| sql_error("find a shared clock", error);
    let found: Option<(i64, Vec<u8>)> = connection
        .prepare_cached(FIND)
        .and_then(|mut find| find.query_row(params![digest], |row| Ok((row.get(0)?, row.get(1)?))).optional())
        .map_err(failed)?;
    match found {
        Some((id, saved)) if saved == body => {
            execute(connection, HOLD, params![id], "hold a shared clock")?;
            Ok(id)
        }
        Some(_) => Err(Error::corrupt("two clocks of one digest")),
        None => query_one(connection, MAKE, params![digest, body], "write a clock"),
    }
}

/// One group fewer holding a clock, which goes with the last.
pub(super) fn let_go(connection: &Connection, id: i64) -> Result<()> {
    if execute(connection, LET_GO, params![id], "let a clock go")? != 1 {
        return Err(Error::corrupt("a clock no group holds"));
    }
    execute(connection, FORGET, params![id], "remove a clock no group holds").map(drop)
}

/// A clock's bytes, when they fit `room`; its length either way.
pub(super) fn load(connection: &Connection, id: i64, room: usize) -> Result<(usize, Option<Vec<u8>>)> {
    let room = i64::try_from(room.min(MOST_BYTES)).unwrap_or(i64::MAX);
    let row = connection
        .prepare_cached(LOAD)
        .and_then(|mut load| {
            load.query_row(params![id, room], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<Vec<u8>>>(1)?)))
                .optional()
        })
        .map_err(|error| sql_error("read a clock", error))?;
    let (length, body) = row.ok_or_else(|| Error::corrupt("a group's clock is missing"))?;
    Ok((usize::try_from(length).unwrap_or(usize::MAX), body))
}

#[cfg(test)]
#[path = "clock_tests.rs"]
mod tests;
