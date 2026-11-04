//! The groups a page of an aggregate holds, chosen from every series its
//! query finds: the first groups by name and labels whose series together
//! are as many as a query finds at most, and the first group after them,
//! where the next page begins. A bucket counts every series of its group, so
//! a page holds a group whole or not at all.
//!
//! Series come in the order of their ids, which is no group's, so a group
//! stays until series of groups before it push it out:
//!
//! ```text
//! 4 series at most   met a1, c1, b1, c2, b2, a2
//!                    b2 makes five: c, the last, goes out with its two
//! chosen             a: a1 a2   b: b1 b2   the next page begins at c
//! ```

use std::cmp::Ordering;
use std::collections::BTreeMap;

/// Groups chosen so far.
#[derive(Debug)]
pub(super) struct Chosen<K, T> {
    most: usize,
    groups: BTreeMap<K, Vec<T>>,
    held: usize,
    /// The first group pushed out, and how many of its series were met.
    beyond: Option<(K, usize)>,
}

/// What a page holds: its groups, and the first group after them with the
/// series met of it; or that group alone, whose series pass what a page
/// holds, when it is the page's first.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Choice<K, T> {
    Groups(BTreeMap<K, Vec<T>>, Option<K>),
    TooMany(K, usize),
}

impl<K: Ord, T> Chosen<K, T> {
    pub(super) fn new(most: usize) -> Chosen<K, T> {
        Chosen { most, groups: BTreeMap::new(), held: 0, beyond: None }
    }

    /// Keeps a series in its group while the groups before it leave room,
    /// pushing out the last groups while the page holds more than its most.
    pub(super) fn add(&mut self, group: K, series: T) {
        if let Some((first_out, met)) = &mut self.beyond {
            match group.cmp(first_out) {
                Ordering::Less => {}
                Ordering::Equal => {
                    *met += 1;
                    return;
                }
                Ordering::Greater => return,
            }
        }
        self.groups.entry(group).or_default().push(series);
        self.held += 1;
        while self.held > self.most {
            let Some((last, members)) = self.groups.pop_last() else { break };
            self.held -= members.len();
            // every group still chosen comes before the one that left
            self.beyond = Some((last, members.len()));
        }
    }

    /// What the page holds once every series was met. A first group that
    /// was pushed out left the page with none: only its own series could
    /// push it, so they alone pass what a page holds.
    pub(super) fn choice(self) -> Choice<K, T> {
        match self.beyond {
            Some((group, met)) if self.groups.is_empty() => Choice::TooMany(group, met),
            beyond => Choice::Groups(self.groups, beyond.map(|(group, _)| group)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The groups chosen of series met in this order, each a group's letter
    /// and its number.
    fn chosen(most: usize, met: &[(char, u32)]) -> Choice<char, u32> {
        let mut chosen = Chosen::new(most);
        for &(group, series) in met {
            chosen.add(group, series);
        }
        chosen.choice()
    }

    fn groups(groups: &[(char, &[u32])], beyond: Option<char>) -> Choice<char, u32> {
        Choice::Groups(groups.iter().map(|(group, members)| (*group, members.to_vec())).collect(), beyond)
    }

    #[test]
    fn the_first_groups_are_chosen_whole_in_whatever_order_their_series_come() {
        let met = [('a', 1), ('c', 1), ('b', 1), ('c', 2), ('b', 2), ('a', 2)];
        assert_eq!(chosen(4, &met), groups(&[('a', &[1, 2]), ('b', &[1, 2])], Some('c')));
        assert_eq!(chosen(6, &met), groups(&[('a', &[1, 2]), ('b', &[1, 2]), ('c', &[1, 2])], None));
        assert_eq!(chosen(3, &met), groups(&[('a', &[1, 2])], Some('b')), "a group that once fitted goes out");
        assert_eq!(chosen(4, &[]), groups(&[], None));
    }

    #[test]
    fn a_first_group_past_the_most_is_too_many_and_counts_every_series_of_it() {
        let met = [('b', 1), ('b', 2), ('c', 1), ('b', 3), ('b', 4)];
        assert_eq!(chosen(2, &met), Choice::TooMany('b', 4));
        let later = [('b', 1), ('b', 2), ('b', 3), ('a', 1)];
        assert_eq!(chosen(2, &later), groups(&[('a', &[1])], Some('b')), "a group before it is the page's");
    }
}
