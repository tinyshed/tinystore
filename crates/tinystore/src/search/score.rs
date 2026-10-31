//! BM25 over an index's fields, in whole points.
//!
//! A posting adds weight × idf × BM25's saturated frequency to its
//! document's score, rounded to a point of 2⁻³². Points add exactly in any
//! order, so a document scores the same whether its postings are read from
//! the head, a key's own row or its blocks, and packing or a purge, which
//! moves them and changes the order a search adds them in, changes no score
//! and no answer. A block's bound is the same rounding of its largest
//! frequency over its shortest length: rounding keeps the order of two
//! values far more than its error apart, so a bound is never below a
//! posting of its block.

const K1: f64 = 1.2;
const B: f64 = 0.75;
/// Points a score of 1.
const POINT: f64 = 4_294_967_296.0;
/// What a stem found weighs beside the word as written in the same field.
const STEMS: f64 = 0.75;
/// What the words a prefix begins weigh beside the word as written in the
/// same field: less than a stem, since a word its reader typed whole is
/// likelier the one meant than any it begins.
pub(crate) const PREFIXED: f64 = 0.5;

/// A field's words as written, or their stems: each its own lengths, keys
/// and weight. A field's number `n` is parts `2n` and `2n + 1`; a filter's
/// `n`, whose keys are its values and score nothing, is part `2³⁰ + n`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct Part(pub(crate) u32);

/// The first filter's part, past every field's.
const FILTERS: u32 = 1 << 30;

impl Part {
    pub(crate) fn filter(number: u32) -> Part {
        Part(FILTERS + number)
    }

    pub(crate) fn is_filter(self) -> bool {
        self.0 >= FILTERS
    }

    /// Whether its keys are words as written, which a misspelt word is read as.
    pub(crate) fn is_written(self) -> bool {
        !self.is_filter() && self.0.is_multiple_of(2)
    }

    pub(crate) fn written(field: u32) -> Part {
        Part(field * 2)
    }

    pub(crate) fn stems(field: u32) -> Part {
        Part(field * 2 + 1)
    }

    pub(crate) fn field(self) -> u32 {
        self.0 / 2
    }

    pub(crate) fn is_stems(self) -> bool {
        self.0 % 2 == 1
    }

    /// What a word found here weighs, from its field's weight.
    pub(crate) fn weight(self, field: f64) -> f64 {
        if self.is_stems() { field * STEMS } else { field }
    }
}

/// How much a key tells documents apart: more the fewer of `documents` hold it.
pub(crate) fn idf(documents: u64, holding: u64) -> f64 {
    let (n, df) = (documents as f64, holding as f64);
    (1.0 + (n - df + 0.5) / (df + 0.5)).ln()
}

/// What a key `tf` times in a part of `len` words adds, before its weight
/// and idf.
fn saturated(tf: u32, len: u32, average: f64) -> f64 {
    let tf = f64::from(tf);
    tf * (K1 + 1.0) / (tf + K1 * (1.0 - B + B * f64::from(len) / average))
}

/// A posting's points, its key's weight × idf being `scale`.
pub(crate) fn points(scale: f64, tf: u32, len: u32, average: f64) -> u64 {
    // `as` saturates: an absurd weight caps a score rather than wrapping it.
    (scale * saturated(tf, len, average) * POINT).round() as u64
}

/// A score as a hit says it.
pub(crate) fn score(points: u64) -> f64 {
    points as f64 / POINT
}

/// A part's average length, from its words in every document.
pub(crate) fn average(tokens: u64, documents: u64) -> f64 {
    (tokens as f64 / documents.max(1) as f64).max(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bound_from_a_blocks_largest_frequency_and_shortest_length_is_never_below_its_postings() {
        for scale in [0.001, 0.37, 1.0, 7.5, 312.0] {
            for average in [1.0, 3.7, 120.0, 9_000.0] {
                for most_tf in 1..40 {
                    for least_len in [0, 1, 2, 5, 17, 300, 65_535] {
                        let bound = points(scale, most_tf, least_len, average);
                        for tf in [1, most_tf / 2 + 1, most_tf] {
                            for len in [least_len, least_len + 1, least_len.saturating_mul(3)] {
                                assert!(points(scale, tf, len, average) <= bound);
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn a_rarer_key_tells_more() {
        assert!(idf(1000, 1) > idf(1000, 10));
        assert!(idf(1000, 1000) > 0.0);
    }
}
