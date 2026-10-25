//! An ingest sent again after packing ran: its samples behind what their
//! series has packed are the ones packed there, bit for bit, and change
//! nothing (MET-16). Any other sample behind it would change a packed block,
//! and is refused (NOT-08).
//!
//! It runs on the writer, since what is packed moves only there, and only for
//! a call that reaches behind what its series has packed: the blocks holding
//! those times are decoded, one at a time, and no other.

use rusqlite::{Connection, OptionalExtension, params};

use super::codec::Point;
use super::directory::{Block, Row};
use super::publish::read_group;
use super::snapshot::decode_block;
use super::timestamp::rfc3339;
use crate::compress::frame::Zstd;
use crate::sqlite::sql_error;
use crate::{Error, ErrorKind, Result};

/// The groups that may hold samples of `[?2, ?3]`: from the last that starts
/// at or before `?2`, so that the index walks no older one.
const GROUPS: &str = "select start_ts, end_ts, clock, directory from groups where series_id = ?1
    and start_ts >= coalesce((select start_ts from groups where series_id = ?1 and start_ts <= ?2
        order by start_ts desc limit 1), ?2)
    and start_ts <= ?3 order by start_ts";
const PAYLOAD: &str = "select body from payloads where id = ?1";

/// What series `id` has packed, every sample before `sealed`, as a write
/// compares a call's samples with it.
pub(super) struct Packed<'c, 'z> {
    connection: &'c Connection,
    zstd: &'z mut Zstd,
    id: i64,
    sealed: i64,
    /// The block being compared, decoded.
    decoded: Vec<Point>,
}

impl<'c, 'z> Packed<'c, 'z> {
    pub(super) fn of(connection: &'c Connection, zstd: &'z mut Zstd, id: i64, sealed: i64) -> Self {
        Packed { connection, zstd, id, sealed, decoded: Vec::new() }
    }

    /// Takes the samples of `points`, in the order of their times, that are
    /// behind what is packed, once each is the sample packed at its time;
    /// the first that is not refuses the call.
    pub(super) fn drop_repeats(mut self, points: &mut Vec<Point>) -> Result<()> {
        let behind = points.partition_point(|point| point.at < self.sealed);
        if behind == 0 {
            return Ok(());
        }
        self.holds(&points[..behind])?;
        points.drain(..behind);
        Ok(())
    }

    /// Refuses the first of `behind` that is not the sample packed at its
    /// time. Blocks follow one another in time, so a sample before the next
    /// block holding times of `behind` lies between two blocks: no sample.
    fn holds(&mut self, behind: &[Point]) -> Result<()> {
        let mut next = 0;
        for row in self.groups(behind[0].at, behind[behind.len() - 1].at)? {
            let group = read_group(self.connection, self.zstd, &row)?;
            for (slot, block) in group.blocks.iter().enumerate().filter(|(slot, _)| group.is_live(*slot)) {
                let reached = next + behind[next..].partition_point(|point| point.at <= block.times.end);
                if let Some(missed) = behind[next..reached].iter().find(|point| point.at < block.times.start) {
                    return Err(self.refused(missed, None));
                }
                if reached > next {
                    self.decode(block).map_err(|error| error.within(format!("its block {slot} from {}", row.start)))?;
                    self.compare(&behind[next..reached])?;
                    next = reached;
                }
            }
        }
        behind.get(next).map_or(Ok(()), |missed| Err(self.refused(missed, None)))
    }

    fn groups(&self, first: i64, last: i64) -> Result<Vec<Row>> {
        let id = self.id;
        let failed = |error| sql_error("read a series' groups", error);
        let mut select = self.connection.prepare_cached(GROUPS).map_err(failed)?;
        let rows = select.query_map(params![id, first, last], |row| {
            Ok(Row { series: id, start: row.get(0)?, end: row.get(1)?, clock: row.get(2)?, directory: row.get(3)? })
        });
        rows.and_then(Iterator::collect).map_err(failed)
    }

    /// A block's samples into `decoded`, its payload fetched when its body
    /// lives in one.
    fn decode(&mut self, block: &Block) -> Result<()> {
        self.decoded.clear();
        if !block.external() {
            return decode_block(self.zstd, block, &mut self.decoded);
        }
        let read = self
            .connection
            .prepare_cached(PAYLOAD)
            .and_then(|mut select| select.query_row(params![block.payload], |row| row.get::<_, Vec<u8>>(0)).optional());
        let body = read.map_err(|error| sql_error("read a block's values", error))?;
        let body = body.filter(|body| body.len() == block.body_len);
        let body = body.ok_or_else(|| Error::corrupt("a block's payload is missing or of another length"))?;
        decode_block(self.zstd, &Block { body, ..block.clone() }, &mut self.decoded)
    }

    /// Refuses the first of `given` that is not, bit for bit, the sample of
    /// the decoded block at its time.
    fn compare(&self, given: &[Point]) -> Result<()> {
        for point in given {
            let kept = self.decoded.binary_search_by_key(&point.at, |kept| kept.at).ok().map(|at| self.decoded[at]);
            if kept.is_none_or(|kept| kept.value.to_bits() != point.value.to_bits()) {
                return Err(self.refused(point, kept));
            }
        }
        Ok(())
    }

    /// A sample behind what its series has packed that is not the one packed
    /// there, `kept` that one when there is one.
    fn refused(&self, point: &Point, kept: Option<Point>) -> Error {
        let (at, packed) = (rfc3339(point.at), rfc3339(self.sealed));
        let held = if kept.is_some() { "another value" } else { "no sample" };
        let why = format!("sample at {at}, behind what its series has packed, which holds {held} there");
        Error::new(ErrorKind::TooOld, why).naming("at", at).naming("packed", packed)
    }
}

#[cfg(test)]
#[path = "repeat_tests.rs"]
mod tests;
