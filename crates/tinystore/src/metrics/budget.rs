//! What one query may spend, each bound apart: the series it finds, the bytes
//! it copies out of the file, the samples it decodes, and the samples or
//! buckets it answers. Past one is a `limit` that names it, never a smaller
//! answer; `explain` says what a call would spend of each before it runs.

use crate::{Error, Result};

/// The series a query finds at most.
const MOST_SERIES: usize = 1000;
/// The bytes a query copies out of the file at most: labels, directories,
/// clocks, payloads and heads.
const MOST_BYTES: usize = 16 << 20;
/// The samples a query decodes at most; a block answered from its summary
/// decodes none.
const MOST_DECODED: usize = 1 << 20;
/// The samples or buckets an answer holds at most.
const MOST_ANSWER: usize = 100_000;

/// What a query has spent so far.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Budget {
    pub(super) series: usize,
    pub(super) bytes: usize,
    pub(super) decoded: usize,
    pub(super) answer: usize,
}

impl Budget {
    pub(super) fn series_limit(&self) -> usize {
        MOST_SERIES
    }

    pub(super) fn take_series(&mut self) -> Result<()> {
        self.series = take(self.series, 1, MOST_SERIES, "series")?;
        Ok(())
    }

    pub(super) fn take_bytes(&mut self, bytes: usize) -> Result<()> {
        self.bytes = take(self.bytes, bytes, MOST_BYTES, "bytes read")?;
        Ok(())
    }

    /// The bytes a query may still copy, which a statement asks a row's
    /// bytes be within before it copies them.
    pub(super) fn bytes_left(&self) -> usize {
        MOST_BYTES - self.bytes
    }

    pub(super) fn take_decoded(&mut self, samples: usize) -> Result<()> {
        self.decoded = take(self.decoded, samples, MOST_DECODED, "samples decoded")?;
        Ok(())
    }

    pub(super) fn take_answer(&mut self, samples: usize) -> Result<()> {
        self.answer = take(self.answer, samples, MOST_ANSWER, "answer")?;
        Ok(())
    }
}

/// `spent + more`, or a `limit` naming `bound`, what the call wanted and the
/// bound, by the names every SDK reads.
fn take(spent: usize, more: usize, most: usize, bound: &'static str) -> Result<usize> {
    let wanted = spent.saturating_add(more);
    if wanted <= most {
        return Ok(wanted);
    }
    Err(Error::limit(format!(
        "a query past {most} {bound}: ask for less, a shorter range, a wider `every`, fewer series"
    ))
    .naming("limit", bound)
    .naming("wanted", wanted.to_string())
    .naming("bound", most.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_limit_names_its_bound_what_was_wanted_and_the_bound() {
        let mut budget = Budget { decoded: MOST_DECODED - 10, ..Budget::default() };
        budget.take_decoded(10).unwrap();
        let error = budget.take_decoded(240).unwrap_err();
        assert_eq!(error.kind(), crate::ErrorKind::Limit);
        let facts = (error.fact("limit"), error.fact("wanted"), error.fact("bound"));
        assert_eq!(facts, (Some("samples decoded"), Some("1048816"), Some("1048576")));
        assert_eq!(budget.decoded, MOST_DECODED, "a refused take spends nothing");
    }
}
