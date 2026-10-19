use std::collections::HashMap;

use super::*;
use crate::search::fixture::Random;

/// Blocks of lists kept in memory, by key and place.
#[derive(Default)]
struct Memory(HashMap<(i64, usize), Vec<Posting>>);

impl Fetch for Memory {
    fn block(&mut self, list: &List, index: usize) -> Result<Vec<Posting>> {
        Ok(self.0[&(list.key, index)].clone())
    }
}

/// A list of `postings` cut into blocks of random sizes, the last of them
/// in hand when `head` says, as a key's head is.
fn list(key: i64, postings: &[Posting], random: &mut Random, memory: &mut Memory) -> List {
    let scale = 0.05 + random.below(500) as f64 / 100.0;
    let average = 1.0 + random.below(300) as f64;
    let mut list = List::new(key, Part(0), scale, average, Vec::new());
    let mut at = 0;
    while at < postings.len() {
        let size = (1 + random.below(128) as usize).min(postings.len() - at);
        let block = &postings[at..at + size];
        let head = at + size == postings.len() && random.below(3) == 0;
        if head {
            let held = list.in_hand(block.to_vec());
            list.push(held);
        } else {
            let entry = crate::search::postings::entry_of(block);
            memory.0.insert((key, list.blocks.len()), block.to_vec());
            let stored = list.stored(entry.first, entry.last, entry.most_tf, entry.least_len);
            list.push(stored);
        }
        at += size;
    }
    list
}

fn postings(random: &mut Random, span: u32) -> Vec<Posting> {
    let density = 1 + random.below(40);
    let mut doc = 0;
    let mut postings = Vec::new();
    loop {
        doc += 1 + random.below(density * 2) as u32;
        if doc > span {
            return postings;
        }
        let tf = 1 + random.skewed(6) as u32;
        postings.push(Posting { doc, tf, len: 1 + random.below(400) as u32 });
    }
}

// contract: SRCH-01
#[test]
fn the_best_by_their_blocks_bounds_are_the_best_by_every_posting() {
    for seed in 0..400 {
        let mut random = Random::new(seed);
        let span = 1 + random.below(30_000) as u32;
        let keys = 1 + random.below(8) as i64;
        let k = 1 + random.below(60) as usize;
        let mut memory = Memory::default();
        let lists: Vec<Vec<Posting>> = (0..keys).map(|_| postings(&mut random, span)).collect();
        let mut fast: Vec<List> = Vec::new();
        let mut slow: Vec<List> = Vec::new();
        for (key, postings) in (0..).zip(&lists) {
            let mut cut = Random::new(seed * 31 + key as u64);
            fast.push(list(key, postings, &mut cut, &mut memory));
            let mut cut = Random::new(seed * 31 + key as u64);
            slow.push(list(key, postings, &mut cut, &mut Memory::default()));
        }
        fast.retain(|list| !list.is_empty());
        slow.retain(|list| !list.is_empty());
        let found = best(&mut fast, &mut memory, k, None).unwrap();
        let expected = every(&mut slow, &mut memory, k, None).unwrap();
        assert_eq!(found, expected, "seed {seed}");
    }
}

/// Lists of `postings`, each cut into blocks as its seed says, the blocks kept in `memory`.
fn cut(seed: u64, postings: &[Vec<Posting>], memory: &mut Memory) -> Vec<List> {
    let mut lists: Vec<List> = (0..)
        .zip(postings)
        .map(|(key, postings)| list(key, postings, &mut Random::new(seed * 31 + key as u64), memory))
        .collect();
    lists.retain(|list| !list.is_empty());
    lists
}

// contract: SRCH-03
#[test]
fn the_best_a_filter_passes_are_the_best_of_every_posting_it_passes_on_either_path() {
    for seed in 0..300 {
        let mut random = Random::new(seed);
        let span = 1 + random.below(20_000) as u32;
        let k = 1 + random.below(40) as usize;
        let share = 1 + random.below(200);
        let passing: Vec<u32> = (1..=span).filter(|_| random.below(share) == 0).collect();
        let postings: Vec<Vec<Posting>> = (0..1 + random.below(6)).map(|_| postings(&mut random, span)).collect();
        let mut memory = Memory::default();
        let mut wide = cut(seed, &postings, &mut memory);
        let mut narrow = cut(seed, &postings, &mut Memory::default());
        let mut slow = cut(seed, &postings, &mut Memory::default());
        let expected = every(&mut slow, &mut memory, k, Some(&passing)).unwrap();
        assert_eq!(best(&mut wide, &mut memory, k, Some(&passing)).unwrap(), expected, "seed {seed}, crossed out");
        assert_eq!(best_of(&mut narrow, &mut memory, &passing, k).unwrap(), expected, "seed {seed}, scored alone");
    }
}

// contract: SRCH-01
#[test]
fn of_equal_scores_the_lower_document_comes_first() {
    let mut memory = Memory::default();
    let same: Vec<Posting> = (1..=300).map(|doc| Posting { doc, tf: 1, len: 10 }).collect();
    let mut lists = vec![List::new(1, Part(0), 1.0, 10.0, Vec::new())];
    let held = lists[0].in_hand(same);
    lists[0].push(held);
    let found = best(&mut lists, &mut memory, 3, None).unwrap();
    assert_eq!(found.iter().map(|(_, doc)| *doc).collect::<Vec<_>>(), [1, 2, 3]);
}
