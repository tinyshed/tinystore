use super::*;

fn sum(values: &[f64]) -> Exact {
    let mut exact = Exact::ZERO;
    for value in values {
        assert!(exact.add_float(*value), "{value} is finite");
    }
    exact
}

/// Floats of every kind: tiny, huge, subnormal, both signs, from a fixed
/// sequence.
fn floats(count: usize) -> Vec<f64> {
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    (0..count)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let value = f64::from_bits(state);
            if value.is_finite() { value } else { f64::from_bits(state & !(1 << 62)) }
        })
        .collect()
}

// contract: MET-04
#[test]
fn a_sum_of_two_rounds_as_their_float_sum_does() {
    let values = floats(4000);
    for pair in values.chunks(2) {
        let (a, b) = (pair[0], pair[1]);
        assert_eq!(sum(&[a, b]).round().to_bits(), (a + b).to_bits(), "{a:e} + {b:e}");
        let mut taken = Exact::of(a).unwrap();
        assert!(taken.sub_float(b));
        assert_eq!(taken.round().to_bits(), (a - b).to_bits(), "{a:e} - {b:e}");
    }
}

// contract: MET-04
#[test]
fn a_quotient_rounds_as_a_float_division_does() {
    let values = floats(2000);
    for (index, value) in values.iter().enumerate() {
        let divisor = (index as u64 * 7919) % 100_000 + 1;
        let expected = value / divisor as f64;
        assert_eq!(Exact::of(*value).unwrap().divided(divisor).to_bits(), expected.to_bits(), "{value:e} / {divisor}");
    }
    assert_eq!(sum(&[1.0, 1.0, 2.0]).divided(3), 4.0 / 3.0);
    assert_eq!(sum(&[3.0]).scaled(1000, 7), 3000.0 / 7.0);
}

#[test]
fn a_sum_is_rounded_once_whatever_order_it_was_added_in() {
    assert_eq!(sum(&[0.1, 0.2]).round(), 0.30000000000000004);
    assert_eq!(sum(&[1e308, 1e308, -1e308]).round(), 1e308);
    assert_eq!(sum(&[f64::MAX, f64::MAX]).round(), f64::INFINITY);
    assert_eq!(sum(&[-f64::MAX, -f64::MAX]).round(), f64::NEG_INFINITY);
    assert_eq!(sum(&[1e16, 1.0, -1e16]).round(), 1.0);
    let tiny = f64::from_bits(1);
    assert_eq!(sum(&[tiny, tiny, tiny]).round().to_bits(), 3);
    // ties go to the even float
    assert_eq!(sum(&[9_007_199_254_740_992.0, 1.0]).round(), 9_007_199_254_740_992.0);
    assert_eq!(sum(&[9_007_199_254_740_992.0, 3.0]).round(), 9_007_199_254_740_996.0);
    let values = floats(240);
    let mut backwards: Vec<f64> = values.clone();
    backwards.reverse();
    assert_eq!(sum(&values), sum(&backwards));
}

#[test]
fn zero_is_positive_and_a_cancelled_sum_is_zero() {
    assert!(sum(&[-0.0]).is_zero());
    assert_eq!(sum(&[-0.0]).round().to_bits(), 0.0_f64.to_bits());
    assert!(sum(&[5.5, -5.5]).is_zero());
    assert!(Exact::of(f64::NAN).is_none() && Exact::of(f64::INFINITY).is_none());
}

#[test]
fn an_exact_sum_is_written_one_way_and_read_back() {
    let mut cases = vec![Exact::ZERO, sum(&[1.0]), sum(&[-1.0]), sum(&[0.1, 0.7]), sum(&[f64::MAX; 240])];
    cases.extend(floats(64).iter().map(|value| Exact::of(*value).unwrap()));
    for exact in cases {
        let mut out = Vec::new();
        exact.put(&mut out);
        let mut reader = Reader::new(&out);
        assert_eq!(Exact::read(&mut reader, BLOCK_BITS), exact);
        reader.finish().unwrap();
    }
    let mut zero = Vec::new();
    Exact::ZERO.put(&mut zero);
    assert_eq!(zero, [0]);
}

#[test]
fn another_spelling_of_the_same_sum_or_one_past_its_bound_is_refused() {
    let refused = |bytes: &[u8]| {
        let mut reader = Reader::new(bytes);
        Exact::read(&mut reader, BLOCK_BITS);
        reader.finish().is_err()
    };
    // its magnitude's bytes behind their count and a token of the zero bits taken out and the sign
    let spelled = |magnitude: &[u8], zeros: u64, negative: bool| {
        let mut out = Vec::new();
        put_count(&mut out, magnitude.len() as u64);
        put_count(&mut out, (zeros << 1) | u64::from(negative));
        out.extend_from_slice(magnitude);
        out
    };
    assert!(!refused(&spelled(&[1], 0, false)), "one, as written");
    assert!(refused(&spelled(&[0, 1], 0, false)), "a leading zero byte");
    assert!(refused(&spelled(&[2], 0, false)), "a trailing zero bit left in");
    assert!(refused(&spelled(&[1], 2106, true)), "more zero bits than a block's sum holds");
    assert!(refused(&spelled(&[3], 2105, false)), "two bits shifted past the bound");
    assert!(!refused(&spelled(&[1], 2105, false)), "one bit at the bound's top");
}

#[test]
fn sums_compare_as_the_numbers_they_are() {
    let mut order = vec![sum(&[-1e300]), sum(&[-1.0]), Exact::ZERO, sum(&[1e-300]), sum(&[2.0]), sum(&[1e300])];
    let sorted = order.clone();
    order.reverse();
    order.sort();
    assert_eq!(order, sorted);
}
