use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use super::*;
use crate::search::fixture::{Random, fixture, open};
use crate::search::read::checked;
use crate::search::{Index, eq, maintain};

const DIMENSIONS: usize = 32;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Item {
    title: String,
    half: bool,
}

fn uniform(random: &mut Random) -> f32 {
    random.below(2001) as f32 / 1000.0 - 1.0
}

/// Directions about which vectors gather, as an embedding's topics do.
fn centers(random: &mut Random, count: usize) -> Vec<Vec<f32>> {
    (0..count).map(|_| (0..DIMENSIONS).map(|_| uniform(random)).collect()).collect()
}

/// A vector near one of `centers`.
fn near(random: &mut Random, centers: &[Vec<f32>]) -> Vec<f32> {
    let center = &centers[random.below(centers.len() as u64) as usize];
    center.iter().map(|x| x + 0.35 * uniform(random)).collect()
}

fn cosine(a: &[f32], b: &[f32]) -> f64 {
    let length = |v: &[f32]| v.iter().map(|x| f64::from(*x) * f64::from(*x)).sum::<f64>().sqrt();
    let dot: f64 = a.iter().zip(b).map(|(x, y)| f64::from(*x) * f64::from(*y)).sum();
    dot / (length(a) * length(b))
}

/// An index with recall, and the vectors it holds by id.
struct Recalled {
    index: Index<Item>,
    vectors: Vec<(String, Vec<f32>, bool)>,
}

fn recalled(store: &crate::Store, recall: Option<f64>) -> Index<Item> {
    let builder = store.index::<Item>("items").field("title", 1.0).filters(["half"]).vectors(DIMENSIONS);
    match recall {
        Some(recall) => builder.recall(recall).open().unwrap(),
        None => builder.open().unwrap(),
    }
}

fn filled(store: &crate::Store, random: &mut Random, centers: &[Vec<f32>], count: usize) -> Recalled {
    let index = recalled(store, Some(0.9));
    let mut vectors = Vec::new();
    for n in 0..count {
        let (id, vector, half) = (format!("v{n}"), near(random, centers), n % 2 == 0);
        index.id(&id).vector(&vector).set(&Item { title: "item".to_owned(), half }).unwrap();
        vectors.push((id, vector, half));
    }
    Recalled { index, vectors }
}

impl Recalled {
    /// The exact best `k` of the vectors held that `keep` passes.
    fn exact(&self, question: &[f32], k: usize, keep: impl Fn(bool) -> bool) -> Vec<String> {
        let mut scored: Vec<(f64, usize)> = (0..self.vectors.len())
            .filter(|at| keep(self.vectors[*at].2))
            .map(|at| (cosine(question, &self.vectors[at].1), at))
            .collect();
        scored.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
        scored.into_iter().take(k).map(|(_, at)| self.vectors[at].0.clone()).collect()
    }

    /// The share of the exact ten found over questions near the centers,
    /// every hit's score checked to be its exact cosine.
    fn recall(&self, random: &mut Random, centers: &[Vec<f32>], filtered: bool) -> f64 {
        let mut found = 0.0;
        for _ in 0..50 {
            let question = near(random, centers);
            let call = self.index.search_vector(&question).limit(10);
            let call = if filtered { call.filter(eq("half", &true)) } else { call };
            let hits = checked(|| call.run()).unwrap().hits;
            let truth: HashSet<String> = self.exact(&question, 10, |half| !filtered || half).into_iter().collect();
            for hit in &hits {
                let held = self.vectors.iter().find(|(id, _, _)| *id == hit.id).unwrap();
                assert_eq!(hit.score, cosine(&question, &held.1), "a score is the exact cosine");
                assert!(!filtered || hit.value.half, "every hit passes the filter");
            }
            found += hits.iter().filter(|hit| truth.contains(&hit.id)).count() as f64 / truth.len() as f64;
        }
        found / 50.0
    }

    fn state(&self) -> State {
        self.index.search.file_read(|connection| state(connection, self.index.open.id)).unwrap()
    }
}

#[test]
fn k_means_gathers_the_rows_drawn_about_each_center() {
    let mut random = Random::new(1);
    let centers = centers(&mut random, 6);
    let unit = |v: Vec<f32>| normalized(&v.iter().map(|x| f64::from(*x)).collect::<Vec<_>>()).unwrap();
    let rows: Vec<(usize, Vec<f32>)> = (0..600)
        .map(|n| (n % 6, unit(centers[n % 6].iter().map(|x| x + 0.1 * uniform(&mut random)).collect())))
        .collect();
    let sample: Vec<Vec<f32>> = rows.iter().map(|(_, row)| row.clone()).collect();
    let centroids = train(&sample, 6);
    for center in 0..6 {
        let gathered: HashSet<usize> =
            rows.iter().filter(|(of, _)| *of == center).map(|(_, row)| best_of(&centroids, row).0).collect();
        assert_eq!(gathered.len(), 1, "the rows about center {center} share one cluster");
    }
}

#[test]
fn a_search_with_recall_is_exact_until_its_clusters_serve_and_then_finds_its_share() {
    let fixture = fixture();
    let mut random = Random::new(2);
    let centers = centers(&mut random, 40);
    let held = filled(&fixture.store, &mut random, &centers, 3000);
    assert_eq!(held.state().phase, Phase::None);
    assert_eq!(held.recall(&mut random, &centers, false), 1.0, "exact until the clusters serve");
    maintain(&fixture.store).unwrap();
    let state = held.state();
    assert_eq!(state.phase, Phase::Serving);
    assert!(state.probes > 0 && state.probes < 55, "{} probes", state.probes);
    let recall = held.recall(&mut random, &centers, false);
    assert!(recall >= 0.85, "a recall of {recall}");
    let filtered = held.recall(&mut random, &centers, true);
    assert!(filtered >= 0.8, "a filtered recall of {filtered}");
}

#[test]
fn a_vector_set_once_the_clusters_serve_is_found_from_the_head_and_once_assigned() {
    let fixture = fixture();
    let mut random = Random::new(3);
    let centers = centers(&mut random, 20);
    let held = filled(&fixture.store, &mut random, &centers, 1000);
    maintain(&fixture.store).unwrap();
    let odd: Vec<f32> = (0..DIMENSIONS).map(|at| if at == 3 { 1.0 } else { 0.0 }).collect();
    held.index.id("odd").vector(&odd).set(&Item { title: "item".to_owned(), half: true }).unwrap();
    let top = |index: &Index<Item>| checked(|| index.search_vector(&odd).limit(1).run()).unwrap().hits[0].id.clone();
    assert_eq!(top(&held.index), "odd", "from the head");
    let head = |index: &Index<Item>| index.search.file_read(|connection| members(connection, index.open.id, HEAD));
    assert_eq!(head(&held.index).unwrap(), 1);
    maintain(&fixture.store).unwrap();
    assert_eq!(head(&held.index).unwrap(), 0);
    assert_eq!(top(&held.index), "odd", "from its cluster");
    held.index.delete("odd").unwrap();
    assert_ne!(top(&held.index), "odd", "a deleted vector never answers");
}

#[test]
fn a_cluster_grown_past_twice_its_size_splits_in_two() {
    let fixture = fixture();
    let mut random = Random::new(4);
    let centers = centers(&mut random, 10);
    let held = filled(&fixture.store, &mut random, &centers, 500);
    maintain(&fixture.store).unwrap();
    let before =
        held.index.search.file_read(|connection| Clusters::read(connection, held.index.open.id, 0, DIMENSIONS));
    let crowd = self::centers(&mut random, 1);
    for n in 0..400 {
        let vector = near(&mut random, &crowd);
        held.index.id(format!("crowd{n}")).vector(&vector).set(&Item { title: "item".to_owned(), half: true }).unwrap();
    }
    maintain(&fixture.store).unwrap();
    let after = held.index.search.file_read(|connection| Clusters::read(connection, held.index.open.id, 0, DIMENSIONS));
    assert!(after.unwrap().len() > before.unwrap().len(), "the crowded cluster split");
    let question = near(&mut random, &crowd);
    let hits = checked(|| held.index.search_vector(&question).limit(10).run()).unwrap().hits;
    assert!(hits.iter().all(|hit| hit.id.starts_with("crowd")), "{hits:?}");
}

#[test]
fn recall_removed_is_exact_at_once_and_drops_the_clusters() {
    let fixture = fixture();
    let mut random = Random::new(5);
    let centers = centers(&mut random, 20);
    let mut held = filled(&fixture.store, &mut random, &centers, 1000);
    maintain(&fixture.store).unwrap();
    assert_eq!(held.state().phase, Phase::Serving);
    held.index = recalled(&fixture.store, None);
    assert_eq!(held.recall(&mut random, &centers, false), 1.0, "exact at once");
    maintain(&fixture.store).unwrap();
    assert_eq!(held.state().phase, Phase::None);
    let left: i64 = held
        .index
        .search
        .file_read(|connection| {
            connection
                .query_row("select count(*) from _tinystore_search_members", [], |row| row.get(0))
                .map_err(|error| sql_error("count", error))
        })
        .unwrap();
    assert_eq!(left, 0);
}

#[test]
fn an_index_reopened_mid_build_goes_on_and_serves() {
    let fixture = fixture();
    let mut random = Random::new(6);
    let centers = centers(&mut random, 20);
    let held = filled(&fixture.store, &mut random, &centers, 3000);
    let state = held.state();
    let centroids =
        held.index.search.file_read(|connection| trained(connection, held.index.open.id, &state, DIMENSIONS)).unwrap();
    held.index.search.file_write(|tx| begin(tx, held.index.open.id, state, &centroids).map(drop)).unwrap();
    let crate::search::fixture::Fixture { dir, store } = fixture;
    drop(held);
    store.close().unwrap();
    let store = open(dir.path());
    let index = recalled(&store, Some(0.9));
    let question = near(&mut random, &centers);
    assert!(!checked(|| index.search_vector(&question).run()).unwrap().hits.is_empty(), "exact while building");
    maintain(&store).unwrap();
    let state = index.search.file_read(|connection| super::state(connection, index.open.id)).unwrap();
    assert_eq!(state.phase, Phase::Serving);
}

/// How long a build and a search from clusters take: 100,000 vectors of 256
/// dimensions about 500 centers, recall 0.95, against the exact best found by
/// scoring every vector in memory. Run by hand:
/// `cargo test -p tinystore --release --lib a_search_from_clusters -- --ignored --nocapture`.
#[test]
#[ignore = "a timing, run by hand"]
fn a_search_from_clusters_over_a_hundred_thousand_takes() {
    let fixture = fixture();
    let index = fixture.store.index::<Item>("timing").field("title", 1.0).vectors(256).recall(0.95).open().unwrap();
    let mut random = Random::new(7);
    let centers: Vec<Vec<f32>> = (0..500).map(|_| (0..256).map(|_| uniform(&mut random)).collect()).collect();
    let near = |random: &mut Random| -> Vec<f32> {
        let center = &centers[random.below(500) as usize];
        center.iter().map(|x| x + 0.5 * uniform(random)).collect()
    };
    let item = Item { title: "item".to_owned(), half: true };
    let mut held: Vec<(String, Vec<f32>)> = Vec::new();
    for batch in 0..100 {
        let vectors: Vec<(String, Vec<f32>)> = (0..1000).map(|n| (format!("{batch}-{n}"), near(&mut random))).collect();
        index.set_many(vectors.iter().map(|(id, vector)| (id, &item, vector))).unwrap();
        held.extend(vectors);
    }
    let started = std::time::Instant::now();
    maintain(&fixture.store).unwrap();
    let state = index.search.file_read(|connection| super::state(connection, index.open.id)).unwrap();
    let built = started.elapsed().as_secs_f64();
    eprintln!("built in {built:.1} s: {} probes, {} scored again", state.probes, state.rescored);
    let (mut took, mut found) = (Vec::new(), 0.0);
    for _ in 0..40 {
        let question = near(&mut random);
        let started = std::time::Instant::now();
        let hits = index.search_vector(&question).run().unwrap().hits;
        took.push(started.elapsed().as_secs_f64() * 1000.0);
        let mut scored: Vec<(f64, &String)> = held.iter().map(|(id, v)| (cosine(&question, v), id)).collect();
        scored.sort_by(|a, b| b.0.total_cmp(&a.0));
        let truth: HashSet<&String> = scored.into_iter().take(10).map(|(_, id)| id).collect();
        found += hits.iter().filter(|hit| truth.contains(&hit.id)).count() as f64 / 10.0;
    }
    took.sort_by(f64::total_cmp);
    eprintln!("from clusters {:.2} ms at the median, {:.2} at most; recall {:.3}", took[20], took[39], found / 40.0);
}
