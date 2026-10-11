use super::*;

fn points(times: &[i64]) -> Vec<Point> {
    times.iter().map(|&at| Point { at, value: 0.0 }).collect()
}

fn read_back(times: &Times) -> Vec<i64> {
    let mut run = vec![Point::default(); times.count];
    times.fill(&mut run).unwrap();
    run.iter().map(|point| point.at).collect()
}

#[test]
fn a_block_evenly_spaced_keeps_no_steps_and_others_the_shortest_spelling() {
    let even: Vec<i64> = (0..240).map(|index| 1_000 + index * 15_000).collect();
    let times = Times::of(&points(&even));
    assert!(times.steps.is_empty());
    assert_eq!(read_back(&times), even);

    let one_late: Vec<i64> = (0..240).map(|index| index * 15_000 + i64::from(index == 100) * 30).collect();
    let times = Times::of(&points(&one_late));
    assert_eq!(times.steps[0], EXCEPTIONS, "a steady step with two exceptions");
    assert_eq!(read_back(&times), one_late);

    let halves: Vec<i64> =
        (0..240).map(|index| if index < 120 { index * 10 } else { 1200 + (index - 120) * 30 }).collect();
    let times = Times::of(&points(&halves));
    assert_eq!(times.steps[0], RUNS, "two runs of steps");
    assert_eq!(read_back(&times), halves);

    let wild: Vec<i64> = (0..240).map(|index| index * index * 3 + index).collect();
    let times = Times::of(&points(&wild));
    assert_eq!(times.steps[0], PLAIN);
    assert_eq!(read_back(&times), wild);

    let single = Times::of(&points(&[7]));
    assert_eq!(read_back(&single), [7]);
}

#[test]
fn a_clock_reads_back_its_blocks_and_refuses_what_changed() {
    let blocks = vec![
        Times::of(&points(&(0..240).map(|index| index * 1000).collect::<Vec<_>>())),
        Times::of(&points(&[300_000, 300_500, 302_000])),
        Times::of(&points(&[i64::MAX - 10])),
    ];
    let body = encode(&blocks);
    assert_eq!(decode(&body).unwrap(), blocks);
    for place in 0..body.len() {
        let mut broken = body.clone();
        broken[place] ^= 0x04;
        assert!(decode(&broken).is_err(), "byte {place}");
    }
    let overlapping = encode(&[blocks[1].clone(), blocks[0].clone()]);
    assert!(decode(&overlapping).is_err());
}

#[test]
fn steps_that_do_not_lead_to_the_end_are_refused() {
    let mut times = Times::of(&points(&[0, 10, 30]));
    times.end = 40;
    assert!(times.walk(|_, _| {}).is_err());
    let uneven = Times { start: 0, end: 10, count: 4, steps: Vec::new() };
    assert!(uneven.walk(|_, _| {}).is_err(), "10 is not three equal steps");
    let past = Times { start: i64::MAX - 5, end: i64::MAX - 5, count: 2, steps: vec![PLAIN, 1, 10] };
    assert!(past.walk(|_, _| {}).is_err(), "a step past the end of time");
    let zero_unit = Times { start: 0, end: 10, count: 3, steps: vec![PLAIN, 0, 5, 5] };
    assert!(zero_unit.walk(|_, _| {}).is_err());
}
