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

    // steps of every order of size, which a rice code escapes for
    let orders = [1, 200, 40_000, 9_000_000, 2_000_000_000];
    let wild: Vec<i64> = (0..240).scan(0, |at, index| Some(std::mem::replace(at, *at + orders[index % 5]))).collect();
    let times = Times::of(&points(&wild));
    assert_eq!(times.steps[0], PLAIN);
    assert_eq!(read_back(&times), wild);

    // a ping a second, its time taken when the answer came: 4 bits a step
    let answered: Vec<i64> = (0..240).map(|index| index * 1000 + (index * 7919) % 9).collect();
    let times = Times::of(&points(&answered));
    assert_eq!(times.steps[0], DEVIATIONS);
    assert!(times.steps.len() < 130, "{} bytes", times.steps.len());
    assert_eq!(read_back(&times), answered);

    let single = Times::of(&points(&[7]));
    assert_eq!(read_back(&single), [7]);
}

#[test]
fn deviations_bring_back_steps_of_any_size() {
    let steps = [1000, 1003, 997, 1_000_000_000_000, 1001, u64::MAX, 1, 999];
    let encoded = deviations(&steps, 1);
    let mut read = Vec::new();
    walk_steps(&encoded, steps.len() + 1, &mut |_, step| {
        read.push(step);
        Ok(())
    })
    .unwrap();
    assert_eq!(read, steps);
    let mut longer = encoded.clone();
    longer.push(0);
    assert!(walk_steps(&longer, steps.len() + 1, &mut |_, _| Ok(())).is_err(), "a byte past the last step");
}

#[test]
fn a_clock_of_version_1_reads_as_it_was_written() {
    let written = [
        1, 2, 156, 232, 193, 254, 174, 104, 186, 24, 4, 8, 0, 1, 139, 8, 197, 8, 234, 7, 128, 240, 199, 254, 174, 104,
        176, 234, 1, 3, 0, 66, 228, 49, 118,
    ];
    let blocks = [
        Times::of(&points(&[1_793_013_070_350, 1_793_013_071_385, 1_793_013_072_478, 1_793_013_073_480])),
        Times::of(&points(&[1_793_013_120_000, 1_793_013_135_000, 1_793_013_150_000])),
    ];
    assert_eq!(decode(&written).unwrap(), blocks);
    assert!(encode(&blocks).len() < written.len(), "version 2 spells the second start shorter");
}

#[test]
fn a_clock_reads_back_its_blocks_and_refuses_what_changed() {
    let answered: Vec<i64> = (0..60).map(|index| 400_000 + index * 1000 + (index * 31) % 7).collect();
    let blocks = vec![
        Times::of(&points(&(0..240).map(|index| index * 1000).collect::<Vec<_>>())),
        Times::of(&points(&[300_000, 300_500, 302_000])),
        Times::of(&points(&answered)),
        Times::of(&points(&[i64::MAX - 10])),
    ];
    assert_eq!(blocks[2].steps[0], DEVIATIONS);
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
