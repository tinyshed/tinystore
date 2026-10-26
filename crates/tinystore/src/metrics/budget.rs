//! What one query may spend, each bound apart: the series it finds, the bytes
//! it copies out of the file, the samples it decodes, and the samples or
//! buckets it answers. Past one is a `limit` that names it, never a smaller
//! answer; `explain` says what a call would spend of each before it runs.

use crate::{Error, Result};

/// A bound of a query, by the name every SDK reads in a `limit`.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Bound {
    pub(super) name: &'static str,
    pub(super) most: usize,
}

/// The series a query finds at most.
const SERIES: Bound = Bound { name: "series", most: 1000 };
/// The bytes a query copies out of the file at most: labels, directories,
/// clocks, payloads and heads.
const BYTES: Bound = Bound { name: "bytes read", most: 16 << 20 };
/// The samples a query decodes at most; a block answered from its summary
/// decodes none.
const DECODED: Bound = Bound { name: "samples decoded", most: 1 << 20 };
/// The samples or buckets an answer holds at most.
const ANSWER: Bound = Bound { name: "answer", most: 100_000 };

/// What a query has spent so far.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Budget {
    pub(super) series: usize,
    pub(super) bytes: usize,
    pub(super) decoded: usize,
    pub(super) answer: usize,
    /// A plan's budget goes on counting the samples a call would decode or
    /// answer past their bounds, which costs it nothing, and stops where the
    /// call would at the series it finds and the bytes it reads, which are
    /// its own work.
    planning: bool,
    /// The first bound a plan found passed, and what was wanted of it there.
    passed: Option<(&'static Bound, usize)>,
}

impl Budget {
    /// The budget of `explain`, which notes the bound a call would stop at
    /// rather than stop there.
    pub(super) fn planning() -> Budget {
        Budget { planning: true, ..Budget::default() }
    }

    pub(super) fn is_planning(&self) -> bool {
        self.planning
    }

    /// The bound a plan found passed first, what the call would want of it,
    /// and the bound.
    pub(super) fn passed(&self) -> Option<(&'static str, usize, usize)> {
        let (bound, wanted) = self.passed?;
        let wanted = match *bound {
            DECODED => self.decoded,
            ANSWER => self.answer,
            _ => wanted,
        };
        Some((bound.name, wanted, bound.most))
    }

    pub(super) fn series_limit(&self) -> usize {
        SERIES.most
    }

    pub(super) fn take_series(&mut self) -> Result<()> {
        self.series = self.take(self.series, 1, &SERIES)?;
        Ok(())
    }

    pub(super) fn take_bytes(&mut self, bytes: usize) -> Result<()> {
        self.bytes = self.take(self.bytes, bytes, &BYTES)?;
        Ok(())
    }

    /// The bytes a query may still copy, which a statement asks a row's
    /// bytes be within before it copies them.
    pub(super) fn bytes_left(&self) -> usize {
        BYTES.most - self.bytes
    }

    /// Whether `bytes` more copied and `samples` more decoded stay within
    /// their bounds: a page asks before it takes a part, and stops rather
    /// than fail.
    pub(super) fn fits(&self, bytes: usize, samples: usize) -> bool {
        self.bytes.saturating_add(bytes) <= BYTES.most && self.decoded.saturating_add(samples) <= DECODED.most
    }

    pub(super) fn take_decoded(&mut self, samples: usize) -> Result<()> {
        self.decoded = self.take(self.decoded, samples, &DECODED)?;
        Ok(())
    }

    pub(super) fn take_answer(&mut self, samples: usize) -> Result<()> {
        self.answer = self.take(self.answer, samples, &ANSWER)?;
        Ok(())
    }

    /// `spent + more`, or a `limit` naming the bound, what the call wanted
    /// and the bound, by the names every SDK reads.
    fn take(&mut self, spent: usize, more: usize, bound: &'static Bound) -> Result<usize> {
        let wanted = spent.saturating_add(more);
        if wanted <= bound.most {
            return Ok(wanted);
        }
        if self.planning {
            self.passed.get_or_insert((bound, wanted));
            if *bound == DECODED || *bound == ANSWER {
                return Ok(wanted);
            }
        }
        let most = bound.most;
        Err(Error::limit(format!(
            "a query past {most} {}: ask for less, a shorter range, a wider `every`, fewer series",
            bound.name
        ))
        .naming("limit", bound.name)
        .naming("wanted", wanted.to_string())
        .naming("bound", most.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // contract: ERR-05, MET-07
    #[test]
    fn a_limit_names_its_bound_what_was_wanted_and_the_bound() {
        let mut budget = Budget { decoded: DECODED.most - 10, ..Budget::default() };
        budget.take_decoded(10).unwrap();
        let error = budget.take_decoded(240).unwrap_err();
        assert_eq!(error.kind(), crate::ErrorKind::Limit);
        let facts = (error.fact("limit"), error.fact("wanted"), error.fact("bound"));
        assert_eq!(facts, (Some("samples decoded"), Some("1048816"), Some("1048576")));
        assert_eq!(budget.decoded, DECODED.most, "a refused take spends nothing");
    }

    // contract: MET-07
    #[test]
    fn a_plan_counts_on_past_the_samples_a_call_decodes_and_stops_at_the_bytes_it_reads() {
        let mut plan = Budget::planning();
        plan.take_decoded(DECODED.most).unwrap();
        plan.take_decoded(240).unwrap();
        plan.take_decoded(240).unwrap();
        assert_eq!(plan.passed(), Some(("samples decoded", DECODED.most + 480, DECODED.most)));
        let error = plan.take_bytes(BYTES.most + 1).unwrap_err();
        assert_eq!(error.fact("limit"), Some("bytes read"));
        assert_eq!(plan.passed().map(|passed| passed.0), Some("samples decoded"), "the first bound passed stays");
        assert_eq!(Budget::default().passed(), None);
    }
}
