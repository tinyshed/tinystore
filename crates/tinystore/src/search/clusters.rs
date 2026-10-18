//! Clusters of an index's vectors, for an index opened with `recall`: a
//! search reads the members of the clusters whose centroids lie nearest its
//! question, ordered by RaBitQ's estimate, and scores again on their vectors
//! the best of them, rather than every code.
//!
//! ```text
//! none ── maintenance trains ──► building ── every vector assigned ──► serving
//!   ▲                                                                   │
//!   └──────────── dropping, a step a commit ◄── recall removed ─────────┘
//! ```
//!
//! - **Training** reads a sample of the index's codes, at most 32 a cluster,
//!   and runs spherical k-means over it from k-means++'s seeds, √n clusters
//!   for n vectors, so that a cluster holds about as many members as there
//!   are clusters.
//! - **Members** are rows ordered by their cluster, so that a cluster is
//!   one range of rows and needs no packing; a set puts a vector in the
//!   head, cluster −1, and maintenance moves the head into the cluster
//!   nearest each, 1,024 a commit, as it moves every vector while building.
//! - **A split** halves a cluster past twice √n by spherical 2-means in a
//!   commit of its own, as search-vchurn kept clusters under drift, so that
//!   clusters grow in number with the index.
//! - **Probes**, how many clusters a search reads, are chosen from `recall`
//!   on questions sampled from the index's own vectors, each left out of
//!   its own answer, and again once the vectors double.
//!
//! Searches stay exact until the clusters serve, and every score a search
//! answers is its exact cosine.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashSet};
use std::sync::Arc;

use rusqlite::{Connection, OptionalExtension};

use super::rabitq::{Turned, code};
use super::vectors::{self, Among, Question, Scan};
use super::write::execute;
use crate::sqlite::{Tx, sql_error};
use crate::{Error, Result};

const READ_STATE: &str = "select vectors, clusters, built_through, probes, rescored, calibrated_vectors,
    calibrated_recall, clusters_version from _tinystore_search_indexes where id = ?1";
const WRITE_STATE: &str = "update _tinystore_search_indexes set clusters = ?2, built_through = ?3, probes = ?4,
    rescored = ?5, calibrated_vectors = ?6, calibrated_recall = ?7, clusters_version = ?8 where id = ?1";
const READ_CENTROIDS: &str =
    "select cluster, centroid from _tinystore_search_clusters where index_id = ?1 order by cluster";
const WRITE_CENTROID: &str =
    "insert or replace into _tinystore_search_clusters (index_id, cluster, centroid) values (?1, ?2, ?3)";
const ADD_MEMBER: &str = "insert or replace into _tinystore_search_members (index_id, cluster, doc, code)
    select ?1, ?2, ?3, ?4 where exists (select 1 from _tinystore_search_vectors where key = ?5)";
const READ_MEMBERS: &str = "select doc, code from _tinystore_search_members where index_id = ?1 and cluster = ?2";
const READ_HEAD: &str =
    "select doc from _tinystore_search_members where index_id = ?1 and cluster = -1 order by doc limit ?2";
const COUNT_MEMBERS: &str = "select count(*) from _tinystore_search_members where index_id = ?1 and cluster = ?2";
const READ_VECTORS: &str = "select key, length, vector from _tinystore_search_vectors
    where key > ?1 and key < ?2 order by key limit ?3";
const READ_CODES: &str = "select key, code from _tinystore_search_codes where key >= ?1 and key < ?2";
const DROP_MEMBERS: &str = "delete from _tinystore_search_members where index_id = ?1 and cluster in
    (select cluster from _tinystore_search_members where index_id = ?1 limit 1)";
const DROP_CLUSTERS: &str = "delete from _tinystore_search_clusters where index_id = ?1";

/// The head's cluster: vectors set since maintenance last assigned them.
pub(crate) const HEAD: i64 = -1;
/// Vectors an index holds before clusters are trained for it.
pub(crate) const BUILD_AT: i64 = if cfg!(test) { 200 } else { 2048 };
/// Head members that make assigning them due.
pub(crate) const HEAD_AT: i64 = if cfg!(test) { 24 } else { 256 };
/// Vectors a step of assigning moves, a commit each.
pub(crate) const STEP: i64 = 1024;
/// How many times its size, √n, a cluster grows before it is split.
const SPLIT: u64 = 2;
/// Questions a calibration samples.
const QUESTIONS: usize = if cfg!(test) { 16 } else { 32 };
/// k-means' rounds over the sample, and over a cluster split in two.
const ROUNDS: usize = 8;

/// Where an index's clusters stand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    None,
    Building,
    Serving,
    Dropping,
}

/// What an index's row keeps of its clusters.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct State {
    pub(crate) vectors: i64,
    pub(crate) phase: Phase,
    /// While building, every vector numbered at or below it is assigned.
    pub(crate) built_through: i64,
    /// How many clusters a search reads, 0 for none, an exact search.
    pub(crate) probes: i64,
    /// How many of the best by RaBitQ's estimate a search scores again, at least.
    pub(crate) rescored: i64,
    pub(crate) calibrated_vectors: i64,
    pub(crate) calibrated_recall: f64,
    pub(crate) version: i64,
}

/// An index's centroids as a search keeps them, by their version.
#[derive(Debug)]
pub(crate) struct Clusters {
    pub(crate) version: i64,
    ids: Vec<i64>,
    centroids: Vec<f32>,
    dimensions: usize,
}

pub(crate) fn state(connection: &Connection, index: i64) -> Result<State> {
    let failed = |error| sql_error("read an index's clusters", error);
    let mut select = connection.prepare_cached(READ_STATE).map_err(failed)?;
    select
        .query_row([index], |row| {
            let phase = match row.get::<_, i64>(1)? {
                1 => Phase::Building,
                2 => Phase::Serving,
                3 => Phase::Dropping,
                _ => Phase::None,
            };
            Ok(State {
                vectors: row.get(0)?,
                phase,
                built_through: row.get(2)?,
                probes: row.get(3)?,
                rescored: row.get(4)?,
                calibrated_vectors: row.get(5)?,
                calibrated_recall: row.get(6)?,
                version: row.get(7)?,
            })
        })
        .map_err(failed)
}

fn write_state(tx: &Tx<'_>, index: i64, state: &State) -> Result<()> {
    let phase = match state.phase {
        Phase::None => 0,
        Phase::Building => 1,
        Phase::Serving => 2,
        Phase::Dropping => 3,
    };
    let row = (index, phase, state.built_through, state.probes, state.rescored, state.calibrated_vectors);
    let row = (row.0, row.1, row.2, row.3, row.4, row.5, state.calibrated_recall, state.version);
    execute(tx, WRITE_STATE, row, "write an index's clusters")
}

impl Clusters {
    /// The centroids an index's clusters have in this snapshot.
    pub(crate) fn read(connection: &Connection, index: i64, version: i64, dimensions: usize) -> Result<Clusters> {
        let failed = |error| sql_error("read an index's centroids", error);
        let mut select = connection.prepare_cached(READ_CENTROIDS).map_err(failed)?;
        let mut rows = select.query([index]).map_err(failed)?;
        let (mut ids, mut centroids) = (Vec::new(), Vec::new());
        while let Some(row) = rows.next().map_err(failed)? {
            ids.push(row.get::<_, i64>(0).map_err(failed)?);
            let bytes = row.get_ref(1).and_then(|value| Ok(value.as_blob()?)).map_err(failed)?;
            if bytes.len() != 4 * dimensions {
                return Err(Error::corrupt(format!("a centroid of {} bytes, not {}", bytes.len(), 4 * dimensions)));
            }
            centroids.extend(bytes.as_chunks::<4>().0.iter().map(|float| f32::from_le_bytes(*float)));
        }
        Ok(Clusters { version, ids, centroids, dimensions })
    }

    fn centroid(&self, at: usize) -> &[f32] {
        &self.centroids[at * self.dimensions..][..self.dimensions]
    }

    pub(crate) fn len(&self) -> usize {
        self.ids.len()
    }

    /// Each cluster with its centroid's dot product with `direction`, the
    /// nearest first.
    fn nearest(&self, direction: &[f32]) -> Vec<(f32, usize)> {
        let mut order: Vec<(f32, usize)> = (0..self.len()).map(|at| (dot(self.centroid(at), direction), at)).collect();
        order.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
        order
    }

    /// The cluster nearest `direction`, by its place among them.
    fn nearest_one(&self, direction: &[f32]) -> usize {
        (0..self.len())
            .max_by(|a, b| dot(self.centroid(*a), direction).total_cmp(&dot(self.centroid(*b), direction)))
            .unwrap_or(0)
    }
}

/// Sixteen lanes of a dot product of two vectors of floats.
fn dot(a: &[f32], b: &[f32]) -> f32 {
    let (whole, rest) = a.as_chunks::<16>();
    let (other, other_rest) = b.as_chunks::<16>();
    let mut lanes = [0.0f32; 16];
    for (a, b) in whole.iter().zip(other) {
        for ((lane, a), b) in lanes.iter_mut().zip(a).zip(b) {
            *lane += a * b;
        }
    }
    lanes.iter().sum::<f32>() + rest.iter().zip(other_rest).map(|(a, b)| a * b).sum::<f32>()
}

/// A vector's direction, as f32 of length one.
fn direction(bytes: &[u8], length: f64) -> Vec<f32> {
    bytes.as_chunks::<4>().0.iter().map(|float| (f64::from(f32::from_le_bytes(*float)) / length) as f32).collect()
}

fn normalized(sum: &[f64]) -> Option<Vec<f32>> {
    let length = sum.iter().map(|x| x * x).sum::<f64>().sqrt();
    (length > 0.0).then(|| sum.iter().map(|x| (x / length) as f32).collect())
}

/// Spherical k-means over `sample`: centroids of length one, each the
/// direction of its members' sum, seeded by k-means++; a cluster left empty
/// takes the row its centroid fits worst.
fn train(sample: &[Vec<f32>], clusters: usize) -> Vec<Vec<f32>> {
    let mut centroids = seeds(sample, clusters);
    for _ in 0..ROUNDS {
        let assigned: Vec<(usize, f32)> = sample.iter().map(|row| best_of(&centroids, row)).collect();
        let mut sums = vec![vec![0.0f64; sample[0].len()]; clusters];
        for (row, (cluster, _)) in sample.iter().zip(&assigned) {
            sums[*cluster].iter_mut().zip(row).for_each(|(sum, x)| *sum += f64::from(*x));
        }
        let mut worst: Vec<usize> = (0..sample.len()).collect();
        worst.sort_by(|a, b| assigned[*a].1.total_cmp(&assigned[*b].1));
        let mut reseeded = worst.into_iter();
        for (centroid, sum) in centroids.iter_mut().zip(&sums) {
            match normalized(sum) {
                Some(next) => *centroid = next,
                None => *centroid = sample[reseeded.next().unwrap_or(0)].clone(),
            }
        }
    }
    centroids
}

/// k-means++'s seeds: each next row drawn with a chance by how far it lies
/// from the nearest seed so far, 1 − its cosine, from a generator seeded
/// alike every time, so that a build trains the same clusters again.
fn seeds(sample: &[Vec<f32>], clusters: usize) -> Vec<Vec<f32>> {
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let mut chosen = vec![sample[0].clone()];
    let mut far: Vec<f64> = sample.iter().map(|row| f64::from(1.0 - dot(row, &chosen[0])).max(0.0)).collect();
    while chosen.len() < clusters {
        let total: f64 = far.iter().sum();
        if total <= 0.0 {
            break;
        }
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let mut target = (state >> 11) as f64 / (1u64 << 53) as f64 * total;
        let at = far.iter().position(|d| {
            target -= d;
            target <= 0.0
        });
        let at = at.unwrap_or(sample.len() - 1);
        chosen.push(sample[at].clone());
        for (d, row) in far.iter_mut().zip(sample) {
            *d = d.min(f64::from(1.0 - dot(row, &sample[at])).max(0.0));
        }
    }
    while chosen.len() < clusters {
        chosen.push(sample[chosen.len() * sample.len() / clusters].clone());
    }
    chosen
}

fn best_of(centroids: &[Vec<f32>], row: &[f32]) -> (usize, f32) {
    let scored = centroids.iter().enumerate().map(|(at, centroid)| (at, dot(centroid, row)));
    scored.max_by(|a, b| a.1.total_cmp(&b.1).then(b.0.cmp(&a.0))).unwrap_or((0, 0.0))
}

/// How many clusters `vectors` vectors are trained into: about √n, so that
/// a cluster holds as many members as there are clusters.
fn clusters_for(vectors: i64) -> usize {
    ((vectors.max(1) as f64).sqrt().round() as usize).clamp(1, 4096)
}

/// Trains an index's clusters from a sample of its codes, at most 32 a
/// cluster, read in one snapshot.
pub(crate) fn trained(connection: &Connection, index: i64, state: &State, dimensions: usize) -> Result<Vec<Vec<f32>>> {
    let clusters = clusters_for(state.vectors);
    let wanted = (32 * clusters).max(clusters) as i64;
    let stride = (state.vectors / wanted).max(1);
    let failed = |error| sql_error("read a sample of vectors", error);
    let mut select = connection.prepare_cached(READ_CODES).map_err(failed)?;
    let mut rows = select.query((index << 32, (index + 1) << 32)).map_err(failed)?;
    let (mut sample, mut at) = (Vec::new(), 0i64);
    while let Some(row) = rows.next().map_err(failed)? {
        at += 1;
        if at % stride != 0 {
            continue;
        }
        let code = row.get_ref(1).and_then(|value| Ok(value.as_blob()?)).map_err(failed)?;
        if let Some(direction) = decoded(code, dimensions) {
            sample.push(direction);
        }
    }
    if sample.is_empty() {
        return Ok(Vec::new());
    }
    Ok(train(&sample, clusters.min(sample.len())))
}

/// A direction from a vector's byte code: its levels by its scale.
fn decoded(code: &[u8], dimensions: usize) -> Option<Vec<f32>> {
    let header = code.first_chunk::<8>()?;
    let scale = f32::from_le_bytes([header[0], header[1], header[2], header[3]]);
    let levels = code.get(8..8 + dimensions)?;
    let sum: Vec<f64> = levels.iter().map(|level| f64::from(*level as i8) * f64::from(scale)).collect();
    normalized(&sum)
}

/// Writes trained centroids and begins assigning every vector to them.
pub(crate) fn begin(tx: &Tx<'_>, index: i64, mut state: State, centroids: &[Vec<f32>]) -> Result<State> {
    execute(tx, DROP_CLUSTERS, [index], "drop an index's centroids")?;
    for (cluster, centroid) in (0i64..).zip(centroids) {
        let bytes: Vec<u8> = centroid.iter().flat_map(|x| x.to_le_bytes()).collect();
        execute(tx, WRITE_CENTROID, (index, cluster, bytes), "write a centroid")?;
    }
    state.phase = Phase::Building;
    state.built_through = 0;
    state.version += 1;
    write_state(tx, index, &state)?;
    Ok(state)
}

/// A step of assigning, worked out in a snapshot so that the writer is held
/// only to write it: each vector's cluster and code, and where a build
/// stands after it.
#[derive(Debug)]
pub(crate) struct Assigned {
    state: State,
    members: Vec<(u32, i64, Vec<u8>)>,
}

/// A step of assigning: while building, the next vectors by their number;
/// while serving, the head's, each to the cluster whose centroid is nearest.
pub(crate) fn assigned(connection: &Connection, index: i64, clusters: &Clusters, mut state: State) -> Result<Assigned> {
    let vectors = match state.phase {
        Phase::Building => read_vectors(connection, index, state.built_through, STEP)?,
        _ => head_vectors(connection, index)?,
    };
    let mut members = Vec::with_capacity(vectors.len());
    for (doc, direction) in &vectors {
        let at = clusters.nearest_one(direction);
        members.push((*doc, clusters.ids[at], code(direction, clusters.centroid(at))));
    }
    if state.phase == Phase::Building {
        match vectors.last() {
            Some((doc, _)) if vectors.len() as i64 == STEP => state.built_through = i64::from(*doc),
            _ => state.phase = Phase::Serving,
        }
    }
    Ok(Assigned { state, members })
}

/// Writes a step of assigning, unless the clusters changed since it was
/// worked out; a member whose vector a write has since dropped is left out.
/// Says the clusters it added members to, none when it wrote nothing.
pub(crate) fn assign(tx: &Tx<'_>, index: i64, step: &Assigned) -> Result<Option<Vec<i64>>> {
    let now = state(tx, index)?;
    if now.version != step.state.version || now.phase != Phase::Building && step.state.phase == Phase::Building {
        return Ok(None);
    }
    let mut touched = Vec::with_capacity(step.members.len());
    for (doc, cluster, code) in &step.members {
        let key = index << 32 | i64::from(*doc);
        let assigned = tx.prepare_cached(ADD_MEMBER).and_then(|mut add| add.execute((index, cluster, doc, code, key)));
        if assigned.map_err(|error| sql_error("assign a vector", error))? > 0 {
            touched.push(*cluster);
        }
    }
    if now.phase == Phase::Building {
        write_state(tx, index, &State { vectors: now.vectors, ..step.state })?;
    }
    touched.sort_unstable();
    touched.dedup();
    Ok(Some(touched))
}

/// Up to `most` vectors numbered after `after`, each a direction.
fn read_vectors(connection: &Connection, index: i64, after: i64, most: i64) -> Result<Vec<(u32, Vec<f32>)>> {
    let failed = |error| sql_error("read vectors", error);
    let mut select = connection.prepare_cached(READ_VECTORS).map_err(failed)?;
    let mut rows = select.query((index << 32 | after, (index + 1) << 32, most)).map_err(failed)?;
    let mut read = Vec::new();
    while let Some(row) = rows.next().map_err(failed)? {
        let (key, length): (i64, f64) = (row.get(0).map_err(failed)?, row.get(1).map_err(failed)?);
        let bytes = row.get_ref(2).and_then(|value| Ok(value.as_blob()?)).map_err(failed)?;
        read.push(((key & 0xffff_ffff) as u32, direction(bytes, length)));
    }
    Ok(read)
}

/// The head's first members, each a direction.
fn head_vectors(connection: &Connection, index: i64) -> Result<Vec<(u32, Vec<f32>)>> {
    let failed = |error| sql_error("read the head of vectors", error);
    let mut select = connection.prepare_cached(READ_HEAD).map_err(failed)?;
    let docs = select.query_map((index, STEP), |row| row.get::<_, u32>(0));
    let docs: Vec<u32> = docs.and_then(Iterator::collect).map_err(failed)?;
    let mut read = Vec::new();
    for doc in docs {
        let one = read_vectors(connection, index, i64::from(doc) - 1, 1)?;
        read.extend(one.into_iter().filter(|(found, _)| *found == doc));
    }
    Ok(read)
}

/// How many members a cluster holds, the head −1 among them.
pub(crate) fn members(connection: &Connection, index: i64, cluster: i64) -> Result<i64> {
    let failed = |error| sql_error("count a cluster's members", error);
    let mut select = connection.prepare_cached(COUNT_MEMBERS).map_err(failed)?;
    select.query_row((index, cluster), |row| row.get(0)).map_err(failed)
}

/// Whether a cluster holds past twice √n members of n vectors, as many as
/// clusters were trained for them, so that clusters split as the index grows
/// and a cluster stays about as large as their count.
pub(crate) fn too_large(connection: &Connection, index: i64, cluster: i64) -> Result<bool> {
    let held = members(connection, index, cluster)?;
    let size = (state(connection, index)?.vectors.max(1) as f64).sqrt().max(32.0);
    Ok(held as f64 > SPLIT as f64 * size)
}

/// Splits a cluster in two by spherical 2-means over its vectors: it keeps
/// its number for one half and gives the other a new one, every member's
/// code made again around its new centroid.
pub(crate) fn split(tx: &Tx<'_>, index: i64, clusters: &Clusters, cluster: i64) -> Result<()> {
    let docs = cluster_docs(tx, index, cluster)?;
    let mut rows = Vec::with_capacity(docs.len());
    for doc in docs {
        rows.extend(read_vectors(tx, index, i64::from(doc) - 1, 1)?.into_iter().filter(|(found, _)| *found == doc));
    }
    if rows.len() < 2 {
        return Ok(());
    }
    let sample: Vec<Vec<f32>> = rows.iter().map(|(_, direction)| direction.clone()).collect();
    let halves = train(&sample, 2);
    let added = clusters.ids.iter().max().map_or(0, |most| most + 1);
    for (at, centroid) in [(cluster, &halves[0]), (added, &halves[1])] {
        let bytes: Vec<u8> = centroid.iter().flat_map(|x| x.to_le_bytes()).collect();
        execute(tx, WRITE_CENTROID, (index, at, bytes), "write a centroid")?;
    }
    for (doc, direction) in &rows {
        let (half, _) = best_of(&halves, direction);
        let id = if half == 0 { cluster } else { added };
        let key = index << 32 | i64::from(*doc);
        execute(tx, ADD_MEMBER, (index, id, doc, code(direction, &halves[half]), key), "move a member")?;
    }
    let mut state = state(tx, index)?;
    state.version += 1;
    write_state(tx, index, &state)
}

fn cluster_docs(connection: &Connection, index: i64, cluster: i64) -> Result<Vec<u32>> {
    let failed = |error| sql_error("read a cluster's members", error);
    let mut select = connection.prepare_cached(READ_MEMBERS).map_err(failed)?;
    let docs = select.query_map((index, cluster), |row| row.get::<_, u32>(0));
    docs.and_then(Iterator::collect).map_err(failed)
}

/// A step of dropping an index's clusters: one cluster's members a commit,
/// then the centroids and the state. Says whether it is done.
pub(crate) fn drop_step(tx: &Tx<'_>, index: i64) -> Result<bool> {
    let dropped = tx.prepare_cached(DROP_MEMBERS).and_then(|mut delete| delete.execute([index]));
    if dropped.map_err(|error| sql_error("drop a cluster's members", error))? > 0 {
        return Ok(false);
    }
    execute(tx, DROP_CLUSTERS, [index], "drop an index's centroids")?;
    let mut state = state(tx, index)?;
    (state.phase, state.built_through, state.probes, state.version) = (Phase::None, 0, 0, state.version + 1);
    // Calibrated again with the clusters built again, whatever recall asks.
    (state.calibrated_vectors, state.calibrated_recall) = (0, 0.0);
    write_state(tx, index, &state)?;
    Ok(true)
}

/// A search of an index's clusters.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Probe<'a> {
    pub(crate) index: i64,
    pub(crate) clusters: &'a Clusters,
    pub(crate) question: &'a Question,
    pub(crate) k: usize,
    pub(crate) probes: usize,
    /// How many of the best by RaBitQ's estimate it scores again, at least.
    pub(crate) rescored: usize,
    /// The documents a filter passes, crossed out of the members read; the
    /// search reads further clusters until enough of them pass.
    pub(crate) passing: Option<&'a [u32]>,
}

/// The best `k` among the members of the clusters nearest the question and
/// the head, by their exact cosines: the `max(rescored, 4k)` best by
/// RaBitQ's estimate scored again on their vectors, and every member of the
/// head.
pub(crate) fn best(connection: &Connection, probe: &Probe<'_>) -> Result<Vec<(f64, u32)>> {
    let keep = (4 * probe.k).max(probe.rescored);
    let direction = probe.question.direction();
    let turned = Turned::new(&direction);
    let passes = |doc: u32| probe.passing.is_none_or(|docs| docs.binary_search(&doc).is_ok());
    let mut kept: BinaryHeap<Reverse<(Estimate, u32)>> = BinaryHeap::with_capacity(keep + 1);
    let mut passed = 0;
    for (read, (toward, at)) in probe.clusters.nearest(&direction).into_iter().enumerate() {
        if read >= probe.probes && (probe.passing.is_none() || passed >= keep) {
            break;
        }
        let failed = |error| sql_error("read a cluster's members", error);
        let mut select = connection.prepare_cached(READ_MEMBERS).map_err(failed)?;
        let mut rows = select.query((probe.index, probe.clusters.ids[at])).map_err(failed)?;
        while let Some(row) = rows.next().map_err(failed)? {
            let doc: u32 = row.get(0).map_err(failed)?;
            if !passes(doc) {
                continue;
            }
            passed += 1;
            let code = row.get_ref(1).and_then(|value| Ok(value.as_blob()?)).map_err(failed)?;
            if !turned.fits(code) {
                return Err(Error::corrupt(format!("cluster member {doc}: a code of {} bytes", code.len())));
            }
            kept.push(Reverse((Estimate(turned.estimate(toward, code)), doc)));
            if kept.len() > keep {
                kept.pop();
            }
        }
    }
    let mut docs: Vec<u32> = kept.into_iter().map(|Reverse((_, doc))| doc).collect();
    docs.extend(cluster_docs(connection, probe.index, HEAD)?.into_iter().filter(|doc| passes(*doc)));
    rescored(connection, probe, docs)
}

/// The best `k` of `docs` by their exact cosines.
fn rescored(connection: &Connection, probe: &Probe<'_>, mut docs: Vec<u32>) -> Result<Vec<(f64, u32)>> {
    docs.sort_unstable();
    docs.dedup();
    let cosines = vectors::cosines(connection, probe.index, probe.question, &docs)?;
    let mut scored: Vec<(f64, u32)> = cosines.into_iter().map(|(doc, cosine)| (cosine, doc)).collect();
    scored.sort_by(vectors::ranked);
    scored.truncate(probe.k);
    Ok(scored)
}

/// An estimate, ordered as numbers are.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Estimate(f32);

impl Eq for Estimate {}

impl PartialOrd for Estimate {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Estimate {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&other.0)
    }
}

/// The fewest members scored again, then the fewest clusters read, with
/// which a search finds `recall` of the exact ten on average over questions
/// drawn from the index's own vectors, each left out of its own answer; no
/// clusters, an exact search, when even every cluster and 2,048 members
/// scored again do not. Vectors much alike in a crowd need more scored
/// again, since a bit a dimension cannot tell their cosines apart.
pub(crate) fn calibrate(
    connection: &Connection,
    index: i64,
    clusters: &Clusters,
    recall: f64,
) -> Result<(usize, usize)> {
    let questions = sample_questions(connection, index)?;
    if questions.is_empty() {
        return Ok((0, 0));
    }
    let mut truths: Vec<HashSet<u32>> = Vec::with_capacity(questions.len());
    for (doc, question) in &questions {
        let scan = Scan { index, question, k: 11, among: Among::Every };
        let found = vectors::best(connection, &scan)?.into_iter().map(|(_, found)| found);
        truths.push(found.filter(|found| found != doc).take(10).collect());
    }
    let reached = |probes: usize, rescored: usize| -> Result<f64> {
        let mut found = 0.0;
        for ((doc, question), truth) in questions.iter().zip(&truths) {
            let probe = Probe { index, clusters, question, k: 11, probes, rescored, passing: None };
            let hits = best(connection, &probe)?.into_iter().filter(|(_, d)| d != doc).take(10);
            found += hits.filter(|(_, d)| truth.contains(d)).count() as f64 / truth.len().max(1) as f64;
        }
        Ok(found / questions.len() as f64)
    };
    let mut rescored = 64;
    while reached(clusters.len(), rescored)? < recall {
        if rescored >= 2048 {
            return Ok((0, 0));
        }
        rescored *= 2;
    }
    let (mut low, mut high) = (1, clusters.len());
    while low < high {
        let middle = (low + high) / 2;
        if reached(middle, rescored)? >= recall { high = middle } else { low = middle + 1 }
    }
    Ok((low, rescored))
}

/// Questions drawn from the index's own vectors, spread over their numbers.
fn sample_questions(connection: &Connection, index: i64) -> Result<Vec<(u32, Question)>> {
    let vectors = state(connection, index)?.vectors;
    let stride = (vectors / QUESTIONS as i64).max(1);
    let mut questions = Vec::new();
    let mut after = 0i64;
    while questions.len() < QUESTIONS {
        let Some((doc, values)) = raw_vector(connection, index, after)? else { break };
        questions.push((doc, Question::new(&values)?));
        after = i64::from(doc) + stride - 1;
    }
    Ok(questions)
}

/// The first vector numbered after `after`, as given.
fn raw_vector(connection: &Connection, index: i64, after: i64) -> Result<Option<(u32, Vec<f32>)>> {
    let failed = |error| sql_error("read a vector", error);
    let mut select = connection.prepare_cached(READ_VECTORS).map_err(failed)?;
    let found = select
        .query_row((index << 32 | after, (index + 1) << 32, 1), |row| {
            let key: i64 = row.get(0)?;
            let bytes: Vec<u8> = row.get(2)?;
            Ok(((key & 0xffff_ffff) as u32, bytes))
        })
        .optional()
        .map_err(failed)?;
    Ok(found.map(|(doc, bytes)| (doc, bytes.as_chunks::<4>().0.iter().map(|f| f32::from_le_bytes(*f)).collect())))
}

/// Records how many clusters a search reads and members it scores again,
/// and for what.
pub(crate) fn calibrated(
    tx: &Tx<'_>,
    index: i64,
    (probes, rescored): (usize, usize),
    (vectors, recall): (i64, f64),
) -> Result<()> {
    let mut state = state(tx, index)?;
    (state.probes, state.rescored) = (probes as i64, rescored as i64);
    (state.calibrated_vectors, state.calibrated_recall) = (vectors, recall);
    write_state(tx, index, &state)
}

/// The kept centroids of an index, read again when its clusters changed.
pub(crate) fn kept(
    held: &std::sync::Mutex<Option<Arc<Clusters>>>,
    connection: &Connection,
    (index, version, dimensions): (i64, i64, usize),
) -> Result<Arc<Clusters>> {
    let mut held = held.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(clusters) = held.as_ref().filter(|clusters| clusters.version == version) {
        return Ok(Arc::clone(clusters));
    }
    let read = Arc::new(Clusters::read(connection, index, version, dimensions)?);
    *held = Some(Arc::clone(&read));
    Ok(read)
}

#[cfg(test)]
#[path = "clusters_tests.rs"]
mod tests;
