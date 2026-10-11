//! Bloom filters, by which a read passes a block without fetching it: a
//! block's trace ids, and the values of its fields that look like ids. A
//! filter that says no is right; one that says maybe is wrong about one
//! value in a hundred.

/// The bits a filter spends on each distinct value, and the hashes it takes:
/// together about one false maybe in a hundred.
const BITS: usize = 10;
const HASHES: u64 = 7;
/// The values a field needs in a block before its filter pays.
const ID_VALUES: usize = 16;
/// The longest value a field that looks like ids holds.
const ID_WIDTH: usize = 64;

/// A filter of `values`.
pub(super) fn bloom_of<'v>(values: impl Iterator<Item = &'v [u8]>) -> Vec<u8> {
    let distinct: std::collections::HashSet<&[u8]> = values.collect();
    let mut filter = vec![0_u8; (distinct.len() * BITS).div_ceil(8).max(8)];
    let size = filter.len() as u64 * 8;
    for value in distinct {
        for bit in bits(value, size) {
            filter[(bit / 8) as usize] |= 1 << (bit % 8);
        }
    }
    filter
}

/// Whether a filter may hold `value`; one of no bytes holds nothing.
pub(super) fn may_hold(filter: &[u8], value: &[u8]) -> bool {
    let size = filter.len() as u64 * 8;
    size > 0 && bits(value, size).all(|bit| filter[(bit / 8) as usize] & (1 << (bit % 8)) != 0)
}

/// The bits of a value in a filter of `size` bits: FNV-1a and a mix of it,
/// the two hashes double hashing needs.
fn bits(value: &[u8], size: u64) -> impl Iterator<Item = u64> {
    let first = value
        .iter()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3));
    let second = ((first ^ first >> 31).wrapping_mul(0x9e37_79b9_7f4a_7c15)) | 1;
    (0..HASHES).map(move |n| first.wrapping_add(n.wrapping_mul(second)) % size)
}

/// Whether a field's values in a block are what a read names one of: short
/// JSON strings nearly all distinct, as request ids, hashes and uuids are.
/// Distinct numbers are times and measures, which a read does not ask for
/// one of.
pub(super) fn id_like(values: &[&str]) -> bool {
    let quoted =
        |value: &&str| value.len() >= 2 && value.len() <= ID_WIDTH && value.starts_with('"') && value.ends_with('"');
    if values.len() < ID_VALUES || !values.iter().all(quoted) {
        return false;
    }
    let distinct: std::collections::HashSet<&&str> = values.iter().collect();
    distinct.len() * 10 >= values.len() * 9
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_filter_holds_every_value_it_was_made_of_and_few_others() {
        let values: Vec<String> = (0..1000).map(|n| format!("\"request-{n}\"")).collect();
        let filter = bloom_of(values.iter().map(|value| value.as_bytes()));
        assert_eq!(filter.len(), 1250);
        assert!(values.iter().all(|value| may_hold(&filter, value.as_bytes())));
        let others = (1000..11_000).filter(|n| may_hold(&filter, format!("\"request-{n}\"").as_bytes())).count();
        // about one in a hundred
        assert!(others < 200, "{others}");
        assert!(!may_hold(&[], b"x"));
        assert!(may_hold(&bloom_of([&b"x"[..]].into_iter()), b"x"));
    }

    #[test]
    fn only_short_strings_nearly_all_distinct_look_like_ids() {
        let ids: Vec<String> = (0..16).map(|n| format!("\"9f86d0{n:02}\"")).collect();
        assert!(id_like(&ids.iter().map(String::as_str).collect::<Vec<_>>()));
        assert!(!id_like(&ids[..15].iter().map(String::as_str).collect::<Vec<_>>()));
        let numbers: Vec<String> = (0..16).map(|n| n.to_string()).collect();
        assert!(!id_like(&numbers.iter().map(String::as_str).collect::<Vec<_>>()));
        let routes: Vec<&str> = (0..16).map(|n| ["\"/notes\"", "\"/buy\""][n % 2]).collect();
        assert!(!id_like(&routes));
        let long = format!("\"{}\"", "x".repeat(70));
        let mut wide: Vec<&str> = ids.iter().map(String::as_str).collect();
        wide[3] = &long;
        assert!(!id_like(&wide));
    }
}
