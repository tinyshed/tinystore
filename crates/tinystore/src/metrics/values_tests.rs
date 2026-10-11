use super::*;

fn zstd() -> Zstd {
    Zstd::windowed(3, 13)
}

fn points(values: &[f64]) -> Vec<Point> {
    values.iter().zip(0..).map(|(&value, index)| Point { at: 1_000 + index * 15_000, value }).collect()
}

/// Encodes a block's values, decodes them back onto its times, checks every
/// bit, and gives the way they were kept and the hint after.
fn round_trip(values: &[f64], hint: i64) -> (Option<u8>, i64) {
    let written = points(values);
    let times = Times::of(&written);
    let (body, hint) = encode(&mut zstd(), &written, &times, hint).unwrap();
    let mut run = vec![Point::default(); written.len()];
    times.fill(&mut run).unwrap();
    decode(&mut zstd(), values[0], &times, body.as_deref().unwrap_or_default(), &mut run).unwrap();
    for (wrote, read) in written.iter().zip(&run) {
        assert_eq!((wrote.at, wrote.value.to_bits()), (read.at, read.value.to_bits()));
    }
    (body.map(|body| body[0]), hint)
}

#[test]
fn every_way_keeps_the_bits_and_the_shortest_wins() {
    assert_eq!(round_trip(&[42.0; 240], UNTRIED), (None, UNTRIED), "a constant block keeps no body");
    let steps: Vec<f64> = (0..240).map(|index| (index / 60) as f64).collect();
    assert_eq!(round_trip(&steps, UNTRIED).0, Some(CHANGES));
    let raw: Vec<f64> =
        (0..240).map(|index| f64::from_bits((index as u64 + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15))).collect();
    assert_eq!(round_trip(&raw, UNTRIED).0, Some(ORDINARY));
    let noisy: Vec<f64> = (0..240).map(|index| (index as f64 * 0.137).sin() * 12.0 + 0.1 * 3.0).collect();
    round_trip(&noisy, UNTRIED);
    let cents: Vec<f64> = (0..240).map(|index| ((index * 7919) % 1000) as f64 / 100.0 + 0.1 + 0.2).collect();
    let (way, hint) = round_trip(&cents, UNTRIED);
    assert_eq!(way, Some(GRID), "on a grid of hundredths or finer, with a residual each");
    assert!(hint >= 2);
    assert_eq!(round_trip(&cents, hint).1, hint, "the hint tries its scale alone");
    assert_eq!(round_trip(&cents, NO_GRID).1, NO_GRID, "a series no grid won for tries none");
}

#[test]
fn the_extremes_keep_their_bits_in_every_way() {
    let extremes =
        [-0.0, 0.0, f64::NAN, f64::from_bits(0xfff4_0000_dead_beef), f64::INFINITY, f64::MIN, f64::from_bits(1)];
    let values: Vec<f64> = (0..240).map(|index| extremes[index % extremes.len()]).collect();
    round_trip(&values, UNTRIED);
    let mostly: Vec<f64> = (0..240).map(|index| if index == 100 { f64::NAN } else { 7.0 }).collect();
    assert_eq!(round_trip(&mostly, UNTRIED).0, Some(CHANGES));
}

#[test]
fn a_body_read_as_another_blocks_is_refused() {
    let written = points(&(0..240).map(|index| (index % 7) as f64).collect::<Vec<_>>());
    let times = Times::of(&written);
    let (body, _) = encode(&mut zstd(), &written, &times, UNTRIED).unwrap();
    let body = body.unwrap();
    let mut run = vec![Point::default(); 240];
    let moved = Times { start: times.start + 1, end: times.end + 1, ..times.clone() };
    assert!(decode(&mut zstd(), 0.0, &moved, &body, &mut run).is_err(), "another extent");
    assert!(decode(&mut zstd(), 1.0, &times, &body, &mut run).is_err(), "another first value");
    for place in 0..body.len() {
        let mut broken = body.clone();
        broken[place] ^= 0x02;
        assert!(decode(&mut zstd(), 0.0, &times, &broken, &mut run).is_err(), "byte {place}");
    }
}
