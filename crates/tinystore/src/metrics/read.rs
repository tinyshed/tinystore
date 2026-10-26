//! `read`: the samples of each series a query finds, in its range, decoded
//! from what one snapshot of the file copied; or a page of them, which stops
//! before what would pass its `limit` or a bound, as `page` tells.

use super::budget::Budget;
use super::codec::Point;
use super::engine::Engine;
use super::page::{Paging, Place};
use super::query::Asked;
use super::series::{Sample, Series};
use super::snapshot::{Taken, Want, decode_block};
use crate::clock::from_unix_millis;
use crate::compress::frame::Zstd;
use crate::{Error, Result, unix_millis};

/// What a read reserves of the store's memory for each sample it decodes,
/// beside the bytes it copied.
const SAMPLE_HELD: u64 = 32;
pub(super) use crate::memory::PATIENCE;

impl Engine {
    /// The series of a read, whole; or those of a page, and the place the
    /// page after it begins, none on the last.
    pub(super) fn read(&self, asked: &Asked, paging: Option<&Paging>) -> Result<(Vec<Series>, Option<Place>)> {
        let mut budget = Budget::default();
        let (taken, stopped) = match paging {
            None => (self.take(asked, Want::Samples, &mut budget)?, None),
            Some(paging) => self.take_page(asked, paging, &mut budget)?,
        };
        let weight = budget.bytes as u64 + budget.decoded as u64 * SAMPLE_HELD;
        let _held = self.memory.reserve_for_read(weight, "metrics: a read", PATIENCE)?;
        let mut zstd = self.codecs.take();
        let answer = match paging {
            None => self.answer(&mut zstd, taken, &mut budget).map(|series| (series, None)),
            Some(paging) => self.page(&mut zstd, taken, paging.limit, stopped),
        };
        self.codecs.give_back(zstd);
        answer
    }

    /// Decodes what a snapshot took and keeps each series' samples in its
    /// window, in the order of their times; a series without one is left out.
    fn answer(&self, zstd: &mut Zstd, taken: Vec<Taken>, budget: &mut Budget) -> Result<Vec<Series>> {
        let mut answer = Vec::new();
        let mut points = Vec::new();
        for series in taken {
            self.decode(zstd, &series, &mut points)?;
            if let Some(read) = within(&series, &points) {
                budget.take_answer(read.samples.len())?;
                answer.push(read);
            }
        }
        Ok(answer)
    }

    /// A page's series: each taken series' samples in its window and before
    /// where the take stopped it, until the page holds `limit`. The series
    /// the page cuts says `more`, and its first sample left out is where the
    /// next page begins; otherwise that is where the take stopped.
    fn page(
        &self,
        zstd: &mut Zstd,
        taken: Vec<Taken>,
        limit: usize,
        stopped: Option<Place>,
    ) -> Result<(Vec<Series>, Option<Place>)> {
        let (mut page, mut points, mut left) = (Vec::new(), Vec::new(), limit);
        for series in taken {
            self.decode(zstd, &series, &mut points)?;
            let Some(mut read) = within(&series, &points) else { continue };
            if read.samples.len() > left {
                let next = Place { series: series.found.id, at: unix_millis(read.samples[left].at) };
                read.samples.truncate(left);
                read.more = true;
                if left > 0 {
                    page.push(read);
                }
                return Ok((page, Some(next)));
            }
            left -= read.samples.len();
            read.more = series.until < series.window.to;
            page.push(read);
        }
        Ok((page, stopped))
    }

    /// What a snapshot took of one series, decoded onto `points` in the order
    /// of their times.
    fn decode(&self, zstd: &mut Zstd, series: &Taken, points: &mut Vec<Point>) -> Result<()> {
        crate::cancel::check(|| "a read".to_owned())?;
        points.clear();
        let decoded = series.blocks.iter().try_for_each(|part| decode_block(zstd, &part.block, points));
        let decoded = decoded.and_then(|()| series.decode_head(zstd, points));
        decoded.and_then(|()| in_order(points)).map_err(|error| self.met(&series.found, error))
    }
}

/// Refuses samples that a series' bytes gave out of the order of their
/// times.
fn in_order(points: &[Point]) -> Result<()> {
    if points.windows(2).any(|pair| pair[1].at <= pair[0].at) {
        return Err(Error::corrupt("samples out of the order of their times"));
    }
    Ok(())
}

/// A series' samples in its window, before where a page stopped taking it;
/// none when it has none there.
fn within(series: &Taken, points: &[Point]) -> Option<Series> {
    let from = series.window.from;
    let within: Vec<Sample> = points
        .iter()
        .filter(|point| point.at >= from && point.at < series.until)
        .map(|point| Sample { at: from_unix_millis(point.at), value: point.value })
        .collect();
    if within.is_empty() {
        return None;
    }
    let kept = &series.found.kept;
    let mut read = Series::of(kept.name().to_owned(), series.found.kind);
    read.labels = kept.labels();
    read.samples = within;
    Some(read)
}
