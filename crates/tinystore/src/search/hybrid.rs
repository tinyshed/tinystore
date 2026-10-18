//! A search by words and a vector at once: each half's best, their scores
//! summed at a weight, or the words' best ordered by their vectors.

use std::collections::HashMap;

use super::vectors::ranked;

/// The hits each half of a search by both is asked for at least, or the
/// search's `offset + limit` if more: the sum then ranks two hundred
/// candidates, enough that a document strong in one half and weak in the
/// other still reaches it.
pub(crate) const HALF: usize = 100;

/// Every document either half found, by the sum of (1 − `weight`) × its
/// words' score and `weight` × its vector's, each scaled to 0–1 by its
/// half's lowest and highest and 0 where that half did not find it; of
/// equal sums, the document set first comes first.
pub(crate) fn fuse(words: &[(f64, u32)], vectors: &[(f64, u32)], weight: f64) -> Vec<(f64, u32)> {
    let (words, vectors) = (scaled(words), scaled(vectors));
    let mut docs: Vec<u32> = words.keys().chain(vectors.keys()).copied().collect();
    docs.sort_unstable();
    docs.dedup();
    let sum = |doc: u32| {
        let (word, vector) = (words.get(&doc).copied().unwrap_or(0.0), vectors.get(&doc).copied().unwrap_or(0.0));
        (1.0 - weight) * word + weight * vector
    };
    let mut fused: Vec<(f64, u32)> = docs.into_iter().map(|doc| (sum(doc), doc)).collect();
    fused.sort_by(ranked);
    fused
}

/// A half's scores from 0, its lowest, to 1, its highest; 1 each when they
/// are all equal, since each is then the half's best.
fn scaled(half: &[(f64, u32)]) -> HashMap<u32, f64> {
    let lowest = half.iter().map(|(score, _)| *score).fold(f64::INFINITY, f64::min);
    let highest = half.iter().map(|(score, _)| *score).fold(f64::NEG_INFINITY, f64::max);
    let scale = |score: f64| if highest > lowest { (score - lowest) / (highest - lowest) } else { 1.0 };
    half.iter().map(|(score, doc)| (*doc, scale(*score))).collect()
}

/// The words' hits ordered by their vectors' cosines, the document set first
/// first of equal ones; then those without a vector, in the words' order,
/// scored −1, the least a cosine can be.
pub(crate) fn rerank(words: &[(f64, u32)], cosines: &HashMap<u32, f64>) -> Vec<(f64, u32)> {
    let mut reranked: Vec<(f64, u32)> =
        words.iter().filter_map(|(_, doc)| cosines.get(doc).map(|cosine| (*cosine, *doc))).collect();
    reranked.sort_by(ranked);
    reranked.extend(words.iter().filter(|(_, doc)| !cosines.contains_key(doc)).map(|(_, doc)| (-1.0, *doc)));
    reranked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sum_scales_each_half_to_its_own_lowest_and_highest() {
        let words = [(9.0, 1), (5.0, 2), (1.0, 3)];
        let vectors = [(1.0, 3), (0.75, 4), (0.5, 1)];
        let fused = fuse(&words, &vectors, 0.5);
        assert_eq!(fused, [(0.5, 1), (0.5, 3), (0.25, 2), (0.25, 4)], "of equal sums, the lower number first");
        assert_eq!(fuse(&words, &vectors, 0.0)[0], (1.0, 1), "a weight of 0 is the words alone");
        assert_eq!(fuse(&words, &vectors, 1.0)[0], (1.0, 3), "and of 1, the vector alone");
    }

    #[test]
    fn a_half_of_equal_scores_scales_each_to_one() {
        assert_eq!(fuse(&[(3.0, 7)], &[], 0.5), [(0.5, 7)]);
        assert_eq!(fuse(&[], &[(0.2, 1), (0.2, 2)], 0.5), [(0.5, 1), (0.5, 2)]);
    }

    #[test]
    fn a_rerank_orders_the_words_hits_by_their_cosines_and_finds_nothing_else() {
        let words = [(9.0, 1), (5.0, 2), (3.0, 3), (1.0, 4)];
        let cosines = HashMap::from([(1, 0.2), (2, 0.9), (4, 0.2), (8, 1.0)]);
        assert_eq!(rerank(&words, &cosines), [(0.9, 2), (0.2, 1), (0.2, 4), (-1.0, 3)]);
    }
}
