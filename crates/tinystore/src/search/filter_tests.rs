use std::collections::HashSet;

use serde_json::json;

use super::*;
use crate::search::fixture::Random;

fn terms(condition: &Condition) -> Result<Vec<String>> {
    match condition.asked(&mut Counted::default())? {
        Asked::Terms { terms, .. } => Ok(terms),
        asked => panic!("{asked:?}"),
    }
}

#[test]
fn a_value_is_a_term_by_its_type() {
    assert_eq!(terms(&eq("c", "shoes")).unwrap(), ["t:shoes"]);
    assert_eq!(terms(&eq("c", &42)).unwrap(), ["i:42"]);
    assert_eq!(terms(&eq("c", &42.0)).unwrap(), ["i:42"], "a whole number written as a float");
    assert_eq!(terms(&eq("c", &u64::MAX)).unwrap(), [format!("i:{}", u64::MAX)]);
    assert_eq!(terms(&eq("c", &true)).unwrap(), ["b:true"]);
    assert_eq!(terms(&missing("c")).unwrap(), ["n:"]);
    assert_eq!(terms(&eq("c", &4.5)).unwrap(), [precise(0, sortable(4.5))]);
    assert_eq!(terms(&is_in("c", &["b", "a"])).unwrap(), ["t:b", "t:a"]);
    assert!(terms(&is_in::<&str>("c", &[])).unwrap().is_empty());
    let kind = |condition: Condition| terms(&condition).unwrap_err().kind();
    assert_eq!(kind(eq("c", &json!({ "a": 1 }))), crate::ErrorKind::Invalid);
    assert_eq!(kind(eq("c", &["a", "b"])), crate::ErrorKind::Invalid, "eq takes one value");
    assert_eq!(kind(is_in("c", &[[1]])), crate::ErrorKind::Invalid);
    assert_eq!(kind(eq("c", &"x".repeat(257))), crate::ErrorKind::Limit);
    assert_eq!(kind(range("c", "a".."b")), crate::ErrorKind::Invalid);
    assert_eq!(kind(eq("c", &None::<i64>)), crate::ErrorKind::Invalid, "an absent id never asks for none");
    assert_eq!(kind(is_in("c", &[Some(1), None])), crate::ErrorKind::Invalid);
}

#[test]
fn a_document_holds_each_value_of_its_list_and_none_where_it_has_none() {
    let document = json!({ "tags": ["b", null, "a", "b"], "owner": { "id": 7 }, "empty": [] });
    assert_eq!(held(&document, "tags").unwrap(), ["t:a", "t:b"]);
    let owner = held(&document, "owner.id").unwrap();
    assert!(owner.contains(&"i:7".to_owned()) && owner.len() == 1 + LEVELS as usize, "{owner:?}");
    assert_eq!(held(&document, "empty").unwrap(), ["n:"]);
    assert_eq!(held(&document, "missing").unwrap(), ["n:"]);
}

#[test]
fn places_sort_as_their_numbers_do() {
    let numbers =
        [f64::NEG_INFINITY, -1e300, -2.5, -1.0, -f64::MIN_POSITIVE, 0.0, 1e-300, 1.0, 2.5, 1e300, f64::INFINITY];
    for pair in numbers.windows(2) {
        assert!(sortable(pair[0]) < sortable(pair[1]), "{} < {}", pair[0], pair[1]);
    }
    assert_eq!(sortable(-0.0), sortable(0.0));
}

/// Whether a number's places hold one of a range's blocks.
fn within(blocks: &HashSet<(u32, u64)>, number: u64) -> bool {
    (0..LEVELS).any(|level| blocks.contains(&(level, number >> (STEP * level))))
}

#[test]
fn a_ranges_blocks_hold_every_number_within_it_and_none_without() {
    let mut random = Random::new(3);
    let place = |random: &mut Random| match random.below(3) {
        0 => random.next(),
        1 => (1 << 63) + random.below(1 << 20),
        _ => random.below(1 << 30) << random.below(34),
    };
    for _ in 0..2000 {
        let (a, b) = (place(&mut random), place(&mut random));
        let (low, high) = (a.min(b), a.max(b));
        let found = blocks(low, high);
        assert!(found.len() <= 2 * 255 * (LEVELS as usize - 1) + 256, "{} blocks", found.len());
        let set: HashSet<(u32, u64)> = found.iter().copied().collect();
        assert_eq!(set.len(), found.len(), "no block twice");
        let near = [low, high, low.wrapping_sub(1), high.wrapping_add(1), low / 2 + high / 2, place(&mut random)];
        for number in near {
            assert_eq!(within(&set, number), (low..=high).contains(&number), "{number} in {low}..={high}");
        }
    }
    assert_eq!(blocks(0, u64::MAX).len(), 256, "everything is the coarsest blocks");
    assert_eq!(blocks(7, 7), [(0, 7)]);
}

#[test]
fn a_union_and_an_intersection_keep_their_documents_in_order() {
    assert_eq!(union(&[1, 3, 5], &[2, 3, 9]), [1, 2, 3, 5, 9]);
    assert_eq!(union(&[], &[4]), [4]);
    assert_eq!(intersection(&[1, 3, 5, 9], &[2, 3, 9]), [3, 9]);
    assert!(intersection(&[1], &[]).is_empty());
}

#[test]
fn a_filter_counts_the_conditions_inside_or_and_and() {
    let many = (0..9).map(|n| eq("c", &n));
    assert_eq!(asked(&[or(many)]).unwrap_err().kind(), crate::ErrorKind::Limit);
    let values: Vec<i64> = (0..101).collect();
    assert_eq!(asked(&[is_in("c", &values)]).unwrap_err().kind(), crate::ErrorKind::Limit);
    assert!(asked(&[and([eq("a", &1), range("b", 1..5)]), or([])]).is_ok());
}
