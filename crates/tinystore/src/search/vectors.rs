//! Vectors: kept as the program gave them, and found again by their cosine
//! with a question's.
//!
//! An exact search reads every vector's code rather than the vector: a byte
//! a dimension of its direction, from which it knows a range the cosine
//! lies in. Only a vector whose range reaches the k-th best lower end is
//! scored again, on the vector as given, and every score a search answers
//! is that one, so the answer is the one scoring every vector gives, bit for
//! bit.
//!
//! ```text
//! code:      scale f32 | error f32 | a level a dimension, i8
//! direction: v / |v| = scale × levels + r, where |r| ≤ error
//! ```
//!
//! A row each, in two tables: the codes a search reads apart from the
//! vectors it scores again, so that a scan reads a quarter of the bytes.

use std::cmp::{Ordering, Reverse};
use std::collections::{BinaryHeap, HashMap};

use rusqlite::{Connection, Row};

use super::write::execute;
use crate::sqlite::{Tx, sql_error};
use crate::{Error, Result};

const ADD_VECTOR: &str = "insert into _tinystore_search_vectors (key, length, vector) values (?1, ?2, ?3)";
const ADD_CODE: &str = "insert into _tinystore_search_codes (key, code) values (?1, ?2)";
const DROP_VECTOR: &str = "delete from _tinystore_search_vectors where key = ?1";
const DROP_CODE: &str = "delete from _tinystore_search_codes where key = ?1";
const SCAN: &str = "select key, code from _tinystore_search_codes where key >= ?1 and key < ?2 order by key";
const READ_VECTOR: &str = "select length, vector from _tinystore_search_vectors where key = ?1";

/// The most dimensions a vector has.
pub(crate) const MOST_DIMENSIONS: usize = 4096;
/// A code's scale and error, before its levels.
const HEADER: usize = 8;
/// A question's levels reach ±4,095, so that a dot product of 4,096 of them
/// with a code's, ±128 at most, stays within an i32: 4096 × 4095 × 128 < 2³¹.
const QUESTION_LEVELS: f64 = 4095.0;
/// What a bound leaves for rounding: a cosine and a bound are computed in
/// f64, whose errors over 4,096 dimensions stay below 10⁻¹².
const ROUNDING: f64 = 1e-9;
/// Candidates a scan holds before it drops those the k-th best lower end
/// has passed since.
const CANDIDATES: usize = 4096;

/// A vector ready to keep: its floats as given, little-endian, its length
/// and its code.
#[derive(Debug)]
pub(crate) struct Vector {
    bytes: Vec<u8>,
    length: f64,
    code: Vec<u8>,
}

impl Vector {
    /// `values` checked against the index's `dimensions`, measured and coded.
    pub(crate) fn new(values: &[f32], dimensions: usize) -> Result<Vector> {
        let length = checked(values, dimensions)?;
        let bytes = values.iter().flat_map(|value| value.to_le_bytes()).collect();
        Ok(Vector { bytes, length, code: code(values, length) })
    }

    /// What it adds to a commit.
    pub(crate) fn size(&self) -> usize {
        self.bytes.len() + self.code.len()
    }
}

/// A question's vector: as given for the scores, and its direction as
/// levels for the bounds.
#[derive(Debug)]
pub(crate) struct Question {
    values: Vec<f32>,
    length: f64,
    levels: Vec<i16>,
    /// What a level stands for.
    step: f64,
    /// The length of what the levels leave out of the direction.
    rest: f64,
}

impl Question {
    pub(crate) fn new(values: &[f32]) -> Result<Question> {
        if values.len() > MOST_DIMENSIONS {
            return Err(Error::limit(format!("a vector of {} dimensions, past {MOST_DIMENSIONS}", values.len())));
        }
        let length = checked(values, values.len())?;
        let direction: Vec<f64> = values.iter().map(|value| f64::from(*value) / length).collect();
        let step = largest(&direction) / QUESTION_LEVELS;
        let mut rest = 0.0;
        let mut levels = Vec::with_capacity(direction.len());
        for x in &direction {
            let level = (x / step).round().clamp(-QUESTION_LEVELS, QUESTION_LEVELS);
            rest += (x - step * level) * (x - step * level);
            levels.push(level as i16);
        }
        Ok(Question { values: values.to_vec(), length, levels, step, rest: rest.sqrt() })
    }

    pub(crate) fn dimensions(&self) -> usize {
        self.values.len()
    }

    /// The range a vector's cosine with the question lies in, from its code.
    ///
    /// With q̂ the question's direction, t its step, b its levels and ρ what
    /// they leave out, and u, s, c and r the vector's:
    ///
    /// ```text
    /// q̂·u = s t (b·c) + s (ρ·c) + q̂·r
    /// |s (ρ·c)| ≤ |ρ| |s c| = |ρ| |u − r| ≤ |ρ| (1 + error),  |q̂·r| ≤ error
    /// ```
    ///
    /// and b·c is exact in an i32.
    fn bounds(&self, code: &[u8]) -> Result<(f64, f64)> {
        let header = code.first_chunk::<HEADER>().filter(|_| code.len() == HEADER + self.levels.len());
        let Some(header) = header else {
            return Err(Error::corrupt(format!("a code of {} bytes, not {}", code.len(), HEADER + self.levels.len())));
        };
        let float = |at: usize| f32::from_le_bytes([header[at], header[at + 1], header[at + 2], header[at + 3]]);
        let (scale, error) = (float(0), float(4));
        if !(scale.is_finite() && scale > 0.0 && error.is_finite() && error >= 0.0) {
            return Err(Error::corrupt(format!("a code of scale {scale} and error {error}")));
        }
        let (scale, error) = (f64::from(scale), f64::from(error));
        let estimate = scale * self.step * f64::from(dot(&self.levels, &code[HEADER..]));
        let reach = self.rest * (1.0 + error) + error + ROUNDING;
        Ok((estimate - reach, estimate + reach))
    }

    /// Its cosine with a vector kept as `bytes` of `length`: their dot
    /// product summed in order in f64, where each product of two f32 is
    /// exact, over the product of their lengths. Every score a search by a
    /// vector answers is computed here.
    fn cosine(&self, bytes: &[u8], length: f64) -> f64 {
        let (floats, _) = bytes.as_chunks::<4>();
        let products = self.values.iter().zip(floats).map(|(q, v)| f64::from(*q) * f64::from(f32::from_le_bytes(*v)));
        products.sum::<f64>() / (self.length * length)
    }
}

/// Keeps a document's vector.
pub(crate) fn keep(tx: &Tx<'_>, index: i64, doc: u32, vector: &Vector) -> Result<()> {
    let key = key(index, doc);
    execute(tx, ADD_VECTOR, (key, vector.length, &vector.bytes), "keep a vector")?;
    execute(tx, ADD_CODE, (key, &vector.code), "keep a vector's code")
}

/// Forgets a document's vector, if it had one.
pub(crate) fn forget(tx: &Tx<'_>, index: i64, doc: u32) -> Result<()> {
    let key = key(index, doc);
    execute(tx, DROP_VECTOR, [key], "drop a vector")?;
    execute(tx, DROP_CODE, [key], "drop a vector's code")
}

/// The best `k` of an index's vectors by their cosine with `question`, the
/// document set first first of equal ones: every code read for its range,
/// then those whose range reaches the k-th best lower end scored again,
/// the highest upper end first, until the next cannot reach the k-th best.
pub(crate) fn best(connection: &Connection, index: i64, question: &Question, k: usize) -> Result<Vec<(f64, u32)>> {
    best_counted(connection, index, question, k).map(|(best, _)| best)
}

/// [`best`], and how many vectors it scored again.
fn best_counted(
    connection: &Connection,
    index: i64,
    question: &Question,
    k: usize,
) -> Result<(Vec<(f64, u32)>, usize)> {
    let mut candidates = candidates(connection, index, question, k)?;
    candidates.sort_unstable_by(ranked);
    let mut best: Vec<(f64, u32)> = Vec::with_capacity(k + 1);
    let mut rescored = 0;
    for (upper, doc) in candidates {
        if best.len() == k && best.last().is_some_and(|(kth, _)| upper < *kth) {
            break;
        }
        let Some(score) = scored(connection, index, question, doc)? else {
            return Err(Error::corrupt(format!("document {doc}'s code has no vector")));
        };
        rescored += 1;
        let at = best.partition_point(|held| ranked(held, &(score, doc)) == Ordering::Less);
        best.insert(at, (score, doc));
        best.truncate(k);
    }
    Ok((best, rescored))
}

/// Each vector whose range reaches the k-th best lower end once every code
/// is read, with its upper end.
fn candidates(connection: &Connection, index: i64, question: &Question, k: usize) -> Result<Vec<(f64, u32)>> {
    let failed = |error| sql_error("read vectors' codes", error);
    let mut select = connection.prepare_cached(SCAN).map_err(failed)?;
    let mut rows = select.query((key(index, 0), key(index + 1, 0))).map_err(failed)?;
    let mut lowers: BinaryHeap<Reverse<Lower>> = BinaryHeap::with_capacity(k + 1);
    let mut threshold = f64::NEG_INFINITY;
    let mut candidates = Vec::new();
    while let Some(row) = rows.next().map_err(failed)? {
        let (doc, code) = code_of(row)?;
        let (lower, upper) = question.bounds(code).map_err(|error| error.within(format!("document {doc}")))?;
        if upper < threshold {
            continue;
        }
        candidates.push((upper, doc));
        lowers.push(Reverse(Lower(lower)));
        if lowers.len() > k {
            lowers.pop();
        }
        if lowers.len() == k {
            threshold = lowers.peek().map_or(threshold, |Reverse(Lower(kth))| *kth);
        }
        if candidates.len() >= CANDIDATES + k {
            candidates.retain(|(upper, _)| *upper >= threshold);
        }
    }
    candidates.retain(|(upper, _)| *upper >= threshold);
    Ok(candidates)
}

/// The cosine with `question` of each of `docs` that has a vector.
pub(crate) fn cosines(
    connection: &Connection,
    index: i64,
    question: &Question,
    docs: &[u32],
) -> Result<HashMap<u32, f64>> {
    let mut cosines = HashMap::with_capacity(docs.len());
    for doc in docs {
        if let Some(score) = scored(connection, index, question, *doc)? {
            cosines.insert(*doc, score);
        }
    }
    Ok(cosines)
}

/// The cosine with `question` of document `doc`'s vector, none when it has none.
fn scored(connection: &Connection, index: i64, question: &Question, doc: u32) -> Result<Option<f64>> {
    let failed = |error| sql_error(format!("read document {doc}'s vector"), error);
    let mut select = connection.prepare_cached(READ_VECTOR).map_err(failed)?;
    let mut rows = select.query([key(index, doc)]).map_err(failed)?;
    let Some(row) = rows.next().map_err(failed)? else { return Ok(None) };
    let length: f64 = row.get(0).map_err(failed)?;
    let bytes = row.get_ref(1).and_then(|value| Ok(value.as_blob()?)).map_err(failed)?;
    if bytes.len() != 4 * question.dimensions() || !(length.is_finite() && length > 0.0) {
        let said = format!("document {doc}'s vector of {} bytes and length {length}", bytes.len());
        return Err(Error::corrupt(said));
    }
    Ok(Some(question.cosine(bytes, length)))
}

/// A row of codes: its document and its code.
fn code_of<'r>(row: &'r Row<'_>) -> Result<(u32, &'r [u8])> {
    let failed = |error| sql_error("read a vector's code", error);
    let key: i64 = row.get(0).map_err(failed)?;
    let code = row.get_ref(1).and_then(|value| Ok(value.as_blob()?)).map_err(failed)?;
    Ok((doc_of(key), code))
}

/// A vector's row: the index's id × 2³² + the document's number, so that an
/// index's vectors lie together, in the order their documents were set.
fn key(index: i64, doc: u32) -> i64 {
    index << 32 | i64::from(doc)
}

fn doc_of(key: i64) -> u32 {
    (key & 0xffff_ffff) as u32
}

/// The length of `values`, every length a cosine divides by, and refuses a
/// vector of another length than `dimensions`, one that is not finite and
/// one of zeros, which has no direction.
fn checked(values: &[f32], dimensions: usize) -> Result<f64> {
    if values.len() != dimensions {
        return Err(Error::invalid(format!(
            "a vector of {} dimensions where the index holds {dimensions}",
            values.len()
        )));
    }
    if let Some((at, value)) = values.iter().enumerate().find(|(_, value)| !value.is_finite()) {
        return Err(Error::invalid(format!("a vector holding {value} at {at}")));
    }
    let length = values.iter().map(|value| f64::from(*value) * f64::from(*value)).sum::<f64>().sqrt();
    if length == 0.0 {
        return Err(Error::invalid("a vector of zeros, which has no direction to compare"));
    }
    Ok(length)
}

/// A vector's code: its direction's levels from −127 to 127 by the scale
/// that brings its largest to 127, and the length of what they leave out,
/// rounded up so that it stays a bound.
fn code(values: &[f32], length: f64) -> Vec<u8> {
    let direction: Vec<f64> = values.iter().map(|value| f64::from(*value) / length).collect();
    let scale = (largest(&direction) / 127.0) as f32;
    let step = f64::from(scale);
    let mut code = Vec::with_capacity(HEADER + values.len());
    code.extend(scale.to_le_bytes());
    code.extend([0; 4]);
    let mut left = 0.0;
    for x in &direction {
        let level = (x / step).round().clamp(-127.0, 127.0);
        code.push(level as i8 as u8);
        left += (x - step * level) * (x - step * level);
    }
    let error = left.sqrt();
    let rounded = error as f32;
    let error = if f64::from(rounded) < error { rounded.next_up() } else { rounded };
    code[4..HEADER].copy_from_slice(&error.to_le_bytes());
    code
}

/// The largest magnitude of a direction, at least 1 / √dimensions.
fn largest(direction: &[f64]) -> f64 {
    direction.iter().fold(0.0, |most: f64, x| most.max(x.abs()))
}

/// The dot product of a question's levels and a code's, exact: sixteen
/// lanes, which the compiler turns into the processor's multiply-adds.
fn dot(levels: &[i16], code: &[u8]) -> i32 {
    let (whole, rest) = levels.as_chunks::<16>();
    let (code_whole, code_rest) = code.as_chunks::<16>();
    let mut lanes = [0i32; 16];
    for (levels, code) in whole.iter().zip(code_whole) {
        for ((lane, level), byte) in lanes.iter_mut().zip(levels).zip(code) {
            *lane += i32::from(*level) * i32::from(*byte as i8);
        }
    }
    let tail: i32 = rest.iter().zip(code_rest).map(|(level, byte)| i32::from(*level) * i32::from(*byte as i8)).sum();
    lanes.iter().sum::<i32>() + tail
}

/// Higher scores first, and of equal ones the lower number: the document
/// set first.
pub(crate) fn ranked(a: &(f64, u32), b: &(f64, u32)) -> Ordering {
    b.0.partial_cmp(&a.0).unwrap_or(Ordering::Equal).then(a.1.cmp(&b.1))
}

/// A lower end, ordered as numbers are.
#[derive(PartialEq)]
struct Lower(f64);

impl Eq for Lower {}

impl PartialOrd for Lower {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Lower {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.total_cmp(&other.0)
    }
}

/// The best `k` by scoring every vector the index holds, as [`best`] should
/// answer them.
#[cfg(test)]
pub(crate) fn every(connection: &Connection, index: i64, question: &Question, k: usize) -> Result<Vec<(f64, u32)>> {
    let failed = |error| sql_error("read vectors", error);
    let sql = "select key, length, vector from _tinystore_search_vectors where key >= ?1 and key < ?2";
    let mut select = connection.prepare_cached(sql).map_err(failed)?;
    let mut rows = select.query((key(index, 0), key(index + 1, 0))).map_err(failed)?;
    let mut scored = Vec::new();
    while let Some(row) = rows.next().map_err(failed)? {
        let (key, length): (i64, f64) = (row.get(0).map_err(failed)?, row.get(1).map_err(failed)?);
        let bytes = row.get_ref(2).and_then(|value| Ok(value.as_blob()?)).map_err(failed)?;
        scored.push((question.cosine(bytes, length), doc_of(key)));
    }
    scored.sort_by(ranked);
    scored.truncate(k);
    Ok(scored)
}

#[cfg(test)]
#[path = "vectors_tests.rs"]
mod tests;
