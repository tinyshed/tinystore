use rusqlite::Connection;

use super::*;
use crate::search::fixture::Random;

/// A number in [−1, 1).
fn uniform(random: &mut Random) -> f64 {
    (random.next() >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
}

/// A vector of one of the shapes that strain a code: spread evenly, most of
/// it in one dimension, mostly zeros, all equal, far below one or far above.
fn vector(random: &mut Random, dimensions: usize) -> Vec<f32> {
    let shape = random.below(6);
    let mut values: Vec<f32> = (0..dimensions)
        .map(|_| match shape {
            2 if random.below(8) != 0 => 0.0,
            3 => 1.0,
            _ => (uniform(random) + uniform(random) + uniform(random)) as f32,
        })
        .collect();
    let at = random.below(dimensions as u64) as usize;
    match shape {
        1 => values[at] *= 100.0,
        4 => values.iter_mut().for_each(|value| *value *= 1e-30),
        5 => values.iter_mut().for_each(|value| *value *= 1e30),
        _ => {}
    }
    if values.iter().all(|value| *value == 0.0) {
        values[at] = 1.0;
    }
    values
}

#[test]
fn a_codes_range_holds_the_cosine_of_every_vector() {
    let mut random = Random::new(1);
    for round in 0..3000 {
        let dimensions = match round % 4 {
            0 => 1 + random.below(4) as usize,
            1 => 1 + random.below(64) as usize,
            2 => 1 + random.below(1024) as usize,
            _ => MOST_DIMENSIONS,
        };
        let question = Question::new(&vector(&mut random, dimensions)).unwrap();
        let kept = Vector::new(&vector(&mut random, dimensions), dimensions).unwrap();
        let (lower, upper) = question.bounds(&kept.code).unwrap();
        let cosine = question.cosine(&kept.bytes, kept.length);
        assert!(lower <= cosine && cosine <= upper, "round {round}: {lower} ≤ {cosine} ≤ {upper}");
    }
}

#[test]
fn a_dot_product_of_levels_is_exact_at_its_extremes() {
    let levels = vec![QUESTION_LEVELS as i16; MOST_DIMENSIONS];
    let code = vec![-128i8 as u8; MOST_DIMENSIONS];
    assert_eq!(i64::from(dot(&levels, &code)), -(MOST_DIMENSIONS as i64) * 4095 * 128);
    assert_eq!(dot(&[3, -2, 7], &[2, 255, 1]), 6 + 2 + 7);
}

#[test]
fn a_vector_of_another_length_not_finite_or_of_zeros_is_refused() {
    assert!(Vector::new(&[1.0, 2.0], 3).is_err());
    assert!(Vector::new(&[1.0, f32::NAN, 2.0], 3).is_err());
    assert!(Vector::new(&[1.0, f32::INFINITY, 2.0], 3).is_err());
    assert!(Vector::new(&[0.0, -0.0, 0.0], 3).is_err());
    assert!(Question::new(&[0.0; 4]).is_err());
    assert_eq!(Question::new(&vec![1.0; MOST_DIMENSIONS + 1]).unwrap_err().kind(), crate::ErrorKind::Limit);
}

/// A database of search's tables alone.
fn tables() -> Connection {
    let connection = Connection::open_in_memory().unwrap();
    connection.execute_batch(include_str!("migrations/0001_schema.sql")).unwrap();
    connection
}

fn put(connection: &Connection, index: i64, doc: u32, values: &[f32]) {
    let kept = Vector::new(values, values.len()).unwrap();
    connection.execute(ADD_VECTOR, (key(index, doc), kept.length, &kept.bytes)).unwrap();
    connection.execute(ADD_CODE, (key(index, doc), &kept.code)).unwrap();
}

/// Vectors drawn from a few, each kept again as it was, at twice its length,
/// or a step of one float off in one dimension: cosines equal and nearly
/// equal, which only the vectors as given order.
fn near(random: &mut Random, pool: &[Vec<f32>]) -> Vec<f32> {
    let mut values = pool[random.below(pool.len() as u64) as usize].clone();
    match random.below(4) {
        0 => values.iter_mut().for_each(|value| *value *= 2.0),
        1 => {
            let at = random.below(values.len() as u64) as usize;
            values[at] = if random.below(2) == 0 { values[at].next_up() } else { values[at].next_down() };
        }
        _ => {}
    }
    if values.iter().all(|value| *value == 0.0) {
        values[0] = f32::MIN_POSITIVE;
    }
    values
}

#[test]
fn the_best_by_codes_are_the_best_by_every_vector_bit_for_bit() {
    for seed in 0..60 {
        let mut random = Random::new(seed);
        let dimensions = [1, 3, 16, 64, 300][random.below(5) as usize];
        let connection = tables();
        let pool: Vec<Vec<f32>> = (0..1 + random.below(30)).map(|_| vector(&mut random, dimensions)).collect();
        let count = random.below(1500) as u32;
        for doc in 1..=count {
            let values = if random.below(2) == 0 { near(&mut random, &pool) } else { vector(&mut random, dimensions) };
            put(&connection, 7, doc, &values);
            put(&connection, 8, doc, &vector(&mut random, dimensions));
        }
        for _ in 0..5 {
            let question =
                if random.below(2) == 0 { near(&mut random, &pool) } else { vector(&mut random, dimensions) };
            let question = Question::new(&question).unwrap();
            let k = 1 + random.below(60) as usize;
            let (found, _) = best_counted(&connection, 7, &question, k).unwrap();
            let every = every(&connection, 7, &question, k).unwrap();
            let bits =
                |hits: &[(f64, u32)]| hits.iter().map(|(score, doc)| (score.to_bits(), *doc)).collect::<Vec<_>>();
            assert_eq!(bits(&found), bits(&every), "seed {seed}, k {k}");
        }
    }
}

#[test]
fn codes_rule_out_most_vectors() {
    let mut random = Random::new(5);
    let connection = tables();
    let normal = |random: &mut Random| (uniform(random) + uniform(random) + uniform(random)) as f32;
    for doc in 1..=5000 {
        let values: Vec<f32> = (0..256).map(|_| normal(&mut random)).collect();
        put(&connection, 1, doc, &values);
    }
    let mut rescored = 0;
    for _ in 0..20 {
        let question: Vec<f32> = (0..256).map(|_| normal(&mut random)).collect();
        rescored += best_counted(&connection, 1, &Question::new(&question).unwrap(), 10).unwrap().1;
    }
    eprintln!("{} vectors of 5,000 scored again a search", rescored / 20);
    assert!(rescored / 20 < 100, "{} vectors of 5,000 scored again a search", rescored / 20);
}

#[test]
fn a_code_that_no_longer_reads_is_named() {
    let connection = tables();
    put(&connection, 1, 1, &[1.0, 2.0, 3.0]);
    connection.execute("update _tinystore_search_codes set code = x'0102'", []).unwrap();
    let question = Question::new(&[1.0, 0.0, 0.0]).unwrap();
    let error = best(&connection, 1, &question, 10).unwrap_err();
    assert_eq!(error.kind(), crate::ErrorKind::Corrupt);
    assert!(error.to_string().contains("document 1"), "{error}");
}
