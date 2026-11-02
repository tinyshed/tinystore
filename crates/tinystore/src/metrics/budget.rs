//! What one query may spend, each bound apart: the series it finds, the bytes
//! it copies out of the file, the samples it decodes, and the samples or
//! buckets it answers. Past one is a `limit` that names it, never a smaller
//! answer; `explain` says what a call would spend of each before it runs.

use crate::Result;
use crate::limits::{Limit, QUERY_ANSWER, QUERY_BYTES, QUERY_SAMPLES, QUERY_SERIES};

const SERIES: Limit = QUERY_SERIES;
/// The bytes a query copies out of the file: labels, directories, clocks,
/// payloads and heads.
const BYTES: Limit = QUERY_BYTES;
/// A block answered from its summary decodes none.
const DECODED: Limit = QUERY_SAMPLES;
const ANSWER: Limit = QUERY_ANSWER;

/// A bound's most, counted in memory.
const fn most(bound: &Limit) -> usize {
    bound.bound as usize
}

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
    passed: Option<(&'static Limit, usize)>,
    /// A replace's budget, spent on what the range it replaces holds, whose
    /// refusal asks for a shorter range.
    replacing: bool,
}

impl Budget {
    /// The budget of a gauge's `replace`, bounded as a query is.
    pub(super) fn replacing() -> Budget {
        Budget { replacing: true, ..Budget::default() }
    }

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
        Some((bound.name, wanted, most(bound)))
    }

    pub(super) fn series_limit(&self) -> usize {
        most(&SERIES)
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
        most(&BYTES) - self.bytes
    }

    /// The samples a query may still decode.
    pub(super) fn decoded_left(&self) -> usize {
        most(&DECODED).saturating_sub(self.decoded)
    }

    /// Spends nothing more: what a page took of a group it then leaves out
    /// is given back, so that a bucket alone is tried with the whole bounds.
    pub(super) fn reset(&mut self) {
        *self = Budget { planning: self.planning, ..Budget::default() };
    }

    /// Whether `bytes` more copied and `samples` more decoded stay within
    /// their bounds: a page asks before it takes a part, and stops rather
    /// than fail.
    pub(super) fn fits(&self, bytes: usize, samples: usize) -> bool {
        self.bytes.saturating_add(bytes) <= most(&BYTES) && self.decoded.saturating_add(samples) <= most(&DECODED)
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
    fn take(&mut self, spent: usize, more: usize, bound: &'static Limit) -> Result<usize> {
        let wanted = spent.saturating_add(more);
        if wanted <= most(bound) {
            return Ok(wanted);
        }
        if self.planning {
            self.passed.get_or_insert((bound, wanted));
            if *bound == DECODED || *bound == ANSWER {
                return Ok(wanted);
            }
        }
        let refused = if self.replacing {
            format!("a replace past {} {}: replace a shorter range, in several calls", bound.bound, bound.name)
        } else {
            format!(
                "a query past {} {}: ask for less, a shorter range, a wider `every`, fewer series, or a page at a \
                 time with `limit`",
                bound.bound, bound.name
            )
        };
        Err(bound.refuse(wanted as u64, refused))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // contract: ERR-05, MET-07
    #[test]
    fn a_limit_names_its_bound_what_was_wanted_and_the_bound() {
        let mut budget = Budget { decoded: most(&DECODED) - 10, ..Budget::default() };
        budget.take_decoded(10).unwrap();
        let error = budget.take_decoded(240).unwrap_err();
        assert_eq!(error.kind(), crate::ErrorKind::Limit);
        let facts = (error.fact("limit"), error.fact("wanted"), error.fact("bound"));
        assert_eq!(facts, (Some("samples decoded"), Some("1048816"), Some("1048576")));
        assert_eq!(budget.decoded, most(&DECODED), "a refused take spends nothing");
    }

    // contract: MET-07
    #[test]
    fn a_plan_counts_on_past_the_samples_a_call_decodes_and_stops_at_the_bytes_it_reads() {
        let mut plan = Budget::planning();
        plan.take_decoded(most(&DECODED)).unwrap();
        plan.take_decoded(240).unwrap();
        plan.take_decoded(240).unwrap();
        assert_eq!(plan.passed(), Some(("samples decoded", most(&DECODED) + 480, most(&DECODED))));
        let error = plan.take_bytes(most(&BYTES) + 1).unwrap_err();
        assert_eq!(error.fact("limit"), Some("bytes read"));
        assert_eq!(plan.passed().map(|passed| passed.0), Some("samples decoded"), "the first bound passed stays");
        assert_eq!(Budget::default().passed(), None);
    }

    // contract: ERR-05, MET-19
    #[test]
    fn a_replace_past_a_bound_of_a_query_asks_for_a_shorter_range() {
        let mut replacing = Budget::replacing();
        replacing.take_decoded(most(&DECODED)).unwrap();
        let error = replacing.take_decoded(1).unwrap_err();
        let facts = (error.fact("limit"), error.fact("wanted"), error.fact("bound"));
        assert_eq!(facts, (Some("samples decoded"), Some("1048577"), Some("1048576")));
        assert!(error.to_string().contains("a replace past 1048576 samples decoded: replace a shorter range"));
        let error = replacing.take_bytes(most(&BYTES) + 1).unwrap_err();
        assert_eq!((error.kind(), error.fact("limit")), (crate::ErrorKind::Limit, Some("bytes read")));
    }
}
