//! `read`: the samples of each series a query finds, in its range, decoded
//! from what one snapshot of the file copied.

use super::budget::Budget;
use super::codec::Point;
use super::engine::Engine;
use super::query::Asked;
use super::series::{Sample, Series};
use super::snapshot::{Taken, Want, decode_block};
use crate::clock::from_unix_millis;
use crate::compress::frame::Zstd;
use crate::{Error, Result};

/// What a read reserves of the store's memory for each sample it decodes,
/// beside the bytes it copied.
const SAMPLE_HELD: u64 = 32;
pub(super) use crate::memory::PATIENCE;

impl Engine {
    pub(super) fn read(&self, asked: &Asked) -> Result<Vec<Series>> {
        let mut budget = Budget::default();
        let taken = self.take(asked, Want::Samples, &mut budget)?;
        let weight = budget.bytes as u64 + budget.decoded as u64 * SAMPLE_HELD;
        let _held = self.memory.reserve(weight, "metrics: a read", PATIENCE)?;
        let mut zstd = self.codecs.take();
        let answer = self.answer(&mut zstd, taken, &mut budget);
        self.codecs.give_back(zstd);
        answer
    }

    /// Decodes what a snapshot took and keeps each series' samples in its
    /// window, in the order of their times; a series without one is left out.
    fn answer(&self, zstd: &mut Zstd, taken: Vec<Taken>, budget: &mut Budget) -> Result<Vec<Series>> {
        let mut answer = Vec::new();
        let mut points = Vec::new();
        for series in taken {
            crate::cancel::check(|| "a read".to_owned())?;
            points.clear();
            let decoded = series.blocks.iter().try_for_each(|part| decode_block(zstd, &part.block, &mut points));
            let decoded = decoded.and_then(|()| series.decode_head(zstd, &mut points));
            decoded.and_then(|()| in_order(&points)).map_err(|error| self.met(&series.found, error))?;
            if let Some(read) = within(&series, &points) {
                budget.take_answer(read.samples.len())?;
                answer.push(read);
            }
        }
        Ok(answer)
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

/// A series' samples in its window; none when it has none there.
fn within(series: &Taken, points: &[Point]) -> Option<Series> {
    let window = series.window;
    let within: Vec<Sample> = points
        .iter()
        .filter(|point| point.at >= window.from && point.at < window.to)
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
