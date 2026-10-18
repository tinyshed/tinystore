use super::*;

fn points(times: &[i64], values: &[f64]) -> Vec<Point> {
    times.iter().zip(values).map(|(&at, &value)| Point { at, value }).collect()
}

fn even(count: usize, mut value: impl FnMut(usize) -> f64) -> Vec<Point> {
    (0..count).map(|index| Point { at: 1_700_000_000_000 + index as i64 * 15_000, value: value(index) }).collect()
}

/// Encodes and decodes `run`, checks every time and every bit came back, and
/// gives the flags it was kept with.
fn round_trip(run: &[Point]) -> u8 {
    let (head, body) = encode(&mut zstd(), run).unwrap();
    let mut out = vec![Point { at: 7, value: 7.0 }];
    decode(&mut zstd(), &head, &body, &mut out).unwrap();
    assert_eq!(out.len(), run.len() + 1, "decode appends");
    for (wrote, read) in run.iter().zip(&out[1..]) {
        assert_eq!((wrote.at, wrote.value.to_bits()), (read.at, read.value.to_bits()));
    }
    body[1]
}

fn value_mode(flags: u8) -> u8 {
    (flags >> 2) & 7
}

#[test]
fn every_value_representation_keeps_the_bits() {
    let cases: [(&str, Vec<Point>, u8); 6] = [
        ("constant", even(240, |_| 42.5), VALUES_CONSTANT),
        ("integers", even(240, |index| (index as f64) * 3.0 - 100.0), VALUES_INTEGERS),
        ("decimals", even(240, |index| (index % 17) as f64 / 10.0), VALUES_DECIMALS),
        ("xor", even(240, |index| f64::from_bits(0x4059_0000_0000_0000 + ((index as u64 * 7919) % 1024))), VALUES_XOR),
        ("raw", even(16, |index| f64::from_bits((index as u64 + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15))), VALUES_RAW),
        ("one", even(1, |_| f64::from_bits(0x7ff4_0000_dead_beef)), VALUES_CONSTANT),
    ];
    for (name, run, mode) in cases {
        let flags = round_trip(&run);
        assert_eq!(value_mode(flags), mode, "{name}");
    }
}

#[test]
fn the_extremes_survive_too() {
    let extremes = [
        -0.0,
        0.0,
        f64::NAN,
        f64::from_bits(0x7ff8_0000_0000_0001),
        f64::from_bits(0xfff4_0000_0000_beef),
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::MIN_POSITIVE,
        f64::from_bits(1),
        -f64::from_bits(1),
        f64::MAX,
        f64::MIN,
        9_007_199_254_740_993.0,
        -9_223_372_036_854_775_808.0,
    ];
    let values: Vec<f64> = (0..240).map(|index| extremes[index % extremes.len()]).collect();
    let times: Vec<i64> = (0..240).map(|index| i64::MIN + index as i64).collect();
    round_trip(&points(&times, &values));
    let late: Vec<i64> = (0..240).map(|index| i64::MAX - 239 + index as i64).collect();
    round_trip(&points(&late, &values));
    round_trip(&points(&[i64::MIN, i64::MAX], &[1.0, 2.0]));
}

#[test]
fn times_are_kept_evenly_as_deltas_or_as_their_changes_whichever_is_shortest() {
    let mut steady = even(240, |index| index as f64);
    assert_eq!(round_trip(&steady) & 3, TIMES_EVEN);
    steady[100].at += 1;
    assert_eq!(round_trip(&steady) & 3, TIMES_CHANGES);
    // steps of 1 and 100 by turns: each one byte, each change two
    let jumpy: Vec<i64> = (0..240).map(|index| index / 2 * 101 + index % 2).collect();
    let values = vec![1.0; 240];
    assert_eq!(round_trip(&points(&jumpy, &values)) & 3, TIMES_DELTAS);
}

#[test]
fn a_decimal_travels_as_the_integer_it_was_written_as() {
    // a price that walks by cents, which no period lets zstd fold
    let mut cents = 1999_i64;
    let run: Vec<Point> = even(240, |index| {
        cents += (index as i64 * 7919 % 13) - 6;
        cents as f64 / 100.0
    });
    assert_eq!(value_mode(round_trip(&run)), VALUES_DECIMALS);
    assert_eq!(exact_scale(19.99), Some(2));
    assert_eq!(exact_scale(1e-9), Some(9));
}

#[test]
fn a_value_no_scale_reproduces_is_refused_rather_than_rounded() {
    assert_eq!(exact_scale(0.1 + 0.2), None);
    assert_eq!(exact_scale(-0.0), None);
    assert_eq!(exact_scale(f64::NAN), None);
    assert_eq!(exact_scale(1e-10), None);
    // one such value keeps the whole run off the decimal grid, and its bits whole
    let run = even(240, |index| if index == 7 { 0.1 + 0.2 } else { index as f64 / 10.0 });
    assert_ne!(value_mode(round_trip(&run)), VALUES_DECIMALS);
}

#[test]
fn a_corrupt_body_is_refused() {
    let run = even(240, |index| (index as f64).sin());
    let (head, body) = encode(&mut zstd(), &run).unwrap();
    for place in 0..body.len() {
        let mut broken = body.clone();
        broken[place] ^= 0x10;
        let error = decode(&mut zstd(), &head, &broken, &mut Vec::new()).unwrap_err();
        assert_eq!(error.kind(), crate::ErrorKind::Corrupt, "byte {place}: {error}");
    }
    for cut in 0..body.len() {
        assert!(decode(&mut zstd(), &head, &body[..cut], &mut Vec::new()).is_err(), "cut at {cut}");
    }
}

#[test]
fn a_head_that_does_not_fit_its_body_is_refused() {
    let run = even(100, |index| index as f64);
    let (head, body) = encode(&mut zstd(), &run).unwrap();
    let moved = [
        Head { start: head.start + 1, ..head },
        Head { end: head.end + 15_000, ..head },
        Head { count: head.count - 1, ..head },
        Head { first: 1.0, ..head },
        Head { count: 0, ..head },
        Head { count: 241, ..head },
        Head { end: head.start - 1, ..head },
    ];
    for other in moved {
        let mut out = Vec::new();
        assert!(decode(&mut zstd(), &other, &body, &mut out).is_err(), "{other:?}");
        assert!(out.is_empty(), "a refused body appends nothing");
    }
}

#[test]
fn unordered_or_oversized_input_is_refused() {
    assert!(encode(&mut zstd(), &[]).is_err());
    assert!(encode(&mut zstd(), &even(241, |_| 1.0)).is_err());
    assert!(encode(&mut zstd(), &points(&[5, 5], &[1.0, 2.0])).is_err());
    assert!(encode(&mut zstd(), &points(&[6, 5], &[1.0, 2.0])).is_err());
}

#[test]
fn values_alone_read_back_into_the_times_their_caller_gives() {
    let values: Vec<f64> = (0..240).map(|index| (index % 9) as f64 * 0.5).collect();
    let stream = encode_values_alone(&mut zstd(), &values).unwrap();
    let mut run: Vec<Point> = (0..240).map(|index| Point { at: index * 1000 + 3, value: 0.0 }).collect();
    decode_values_alone(&mut zstd(), values[0], &stream, &mut run).unwrap();
    for (index, point) in run.iter().enumerate() {
        assert_eq!((point.at, point.value.to_bits()), (index as i64 * 1000 + 3, values[index].to_bits()));
    }
}

#[test]
fn a_word_of_ones_past_its_run_or_padding_that_is_not_zero_is_refused() {
    let mut out = vec![Point::default(); 3];
    let head = Head { start: 0, end: 2, count: 3, first: 5.0 };
    // a run of 120 ones where two values are left
    let mut words = vec![DELTAS_IN_WORDS];
    words.extend_from_slice(&(1_u64 << 60).to_le_bytes());
    assert!(decode_integers(&head, false, &words, &mut out).is_err());
    // a word of 60 one-bit fields holding a set bit past the two values left
    let mut padded = vec![DELTAS_IN_WORDS];
    padded.extend_from_slice(&((2_u64 << 60) | 0b100).to_le_bytes());
    assert!(decode_integers(&head, false, &padded, &mut out).is_err());
    let mut fine = vec![DELTAS_IN_WORDS];
    fine.extend_from_slice(&((2_u64 << 60) | 0b11).to_le_bytes());
    decode_integers(&head, false, &fine, &mut out).unwrap();
    assert_eq!(out[1..].iter().map(|point| point.value).collect::<Vec<_>>(), [4.0, 3.0]);
}
