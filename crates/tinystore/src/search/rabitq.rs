//! RaBitQ's code of a vector in a cluster: a bit a dimension of where its
//! direction lies from the cluster's centroid, turned by a rotation that
//! spreads it evenly over every dimension, from which a question's cosine
//! with it is estimated in a few dozen popcounts.
//!
//! With u the vector's direction, c its centroid, ō the unit offset
//! (u − c) / |u − c| and P the rotation:
//!
//! ```text
//! cos(q, u) = q·c + |u − c| × q·ō,   q·ō = Pq·Pō ≈ (Pq·x̄) / (x̄·Pō),   x̄ = sign(Pō) / √D
//! code:     |u − c| f32 | x̄·Pō f32 | sign(Pō), a bit a dimension
//! ```
//!
//! The estimate only orders a cluster's members; every score a search
//! answers is computed again on the vector as given. The rotation is fixed
//! by its seed for good, since every code kept was turned by it.

/// A question's rotated direction in sixteen levels a dimension, four
/// planes of bits, so that its dot product with a code's signs is popcounts.
#[derive(Debug)]
pub(crate) struct Turned {
    planes: [Vec<u64>; 4],
    low: f32,
    step: f32,
    /// The sum of the levels' values over every dimension.
    sum: f32,
    root: f32,
}

/// What a code keeps before its bits.
const HEADER: usize = 8;

/// The dimensions a rotation turns: the next power of two, at least 64, the
/// vector padded with zeros.
pub(crate) fn turned_dimensions(dimensions: usize) -> usize {
    dimensions.next_power_of_two().max(64)
}

/// The bytes a code of a vector of `dimensions` takes.
pub(crate) fn code_size(dimensions: usize) -> usize {
    HEADER + turned_dimensions(dimensions) / 8
}

/// Three rounds of random signs, each followed by a Walsh–Hadamard transform
/// scaled to keep lengths: an orthogonal rotation in D log D.
fn rotate(values: &[f32], size: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; size];
    out[..values.len()].copy_from_slice(values);
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let scale = 1.0 / (size as f32).sqrt();
    for _ in 0..3 {
        for value in &mut out {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            if state & 1 == 1 {
                *value = -*value;
            }
        }
        let mut half = 1;
        while half < size {
            for start in (0..size).step_by(half * 2) {
                for at in start..start + half {
                    let (a, b) = (out[at], out[at + half]);
                    out[at] = a + b;
                    out[at + half] = a - b;
                }
            }
            half *= 2;
        }
        out.iter_mut().for_each(|value| *value *= scale);
    }
    out
}

/// The code of `direction`, of length one, in the cluster centred on
/// `centroid`, of length one too.
pub(crate) fn code(direction: &[f32], centroid: &[f32]) -> Vec<u8> {
    let size = turned_dimensions(direction.len());
    let offset: Vec<f32> = direction.iter().zip(centroid).map(|(u, c)| u - c).collect();
    let length = offset.iter().map(|x| f64::from(*x) * f64::from(*x)).sum::<f64>().sqrt();
    let mut code = Vec::with_capacity(code_size(direction.len()));
    if length == 0.0 {
        code.extend(0.0f32.to_le_bytes());
        code.extend(1.0f32.to_le_bytes());
        code.resize(code_size(direction.len()), 0);
        return code;
    }
    let unit: Vec<f32> = offset.iter().map(|x| (f64::from(*x) / length) as f32).collect();
    let turned = rotate(&unit, size);
    let factor = turned.iter().map(|x| f64::from(x.abs())).sum::<f64>() / (size as f64).sqrt();
    code.extend((length as f32).to_le_bytes());
    code.extend((factor as f32).to_le_bytes());
    for chunk in turned.as_chunks::<8>().0 {
        code.push(chunk.iter().enumerate().fold(0u8, |byte, (bit, x)| byte | (u8::from(*x > 0.0) << bit)));
    }
    code
}

impl Turned {
    /// A question's direction, of length one, rotated and put in levels.
    pub(crate) fn new(direction: &[f32]) -> Turned {
        let size = turned_dimensions(direction.len());
        let turned = rotate(direction, size);
        let low = turned.iter().copied().fold(f32::INFINITY, f32::min);
        let high = turned.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        let step = if high > low { (high - low) / 15.0 } else { 1.0 };
        let mut planes: [Vec<u64>; 4] = std::array::from_fn(|_| vec![0u64; size / 64]);
        let mut levels = 0u32;
        for (at, value) in turned.iter().enumerate() {
            let level = ((value - low) / step).round().clamp(0.0, 15.0) as u32;
            levels += level;
            for (shift, plane) in planes.iter_mut().enumerate() {
                plane[at / 64] |= u64::from((level >> shift) & 1) << (at % 64);
            }
        }
        let sum = low * size as f32 + step * levels as f32;
        Turned { planes, low, step, sum, root: (size as f32).sqrt() }
    }

    /// The question's estimated cosine with a member whose centroid it meets
    /// at `toward`.
    pub(crate) fn estimate(&self, toward: f32, code: &[u8]) -> f32 {
        let float = |at: usize| f32::from_le_bytes([code[at], code[at + 1], code[at + 2], code[at + 3]]);
        let (length, factor) = (float(0), float(4));
        if length == 0.0 {
            return toward;
        }
        let (words, _) = code[HEADER..].as_chunks::<8>();
        let (mut ones, mut weighted) = (0u32, 0u32);
        for (at, word) in words.iter().enumerate() {
            let bits = u64::from_le_bytes(*word);
            ones += bits.count_ones();
            for (shift, plane) in self.planes.iter().enumerate() {
                weighted += (bits & plane[at]).count_ones() << shift;
            }
        }
        let set = self.low * ones as f32 + self.step * weighted as f32;
        toward + length * ((2.0 * set - self.sum) / self.root) / factor
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::fixture::Random;

    fn unit(random: &mut Random, dimensions: usize) -> Vec<f32> {
        let values: Vec<f32> = (0..dimensions).map(|_| random.below(2001) as f32 / 1000.0 - 1.0).collect();
        let length = values.iter().map(|x| x * x).sum::<f32>().sqrt();
        values.iter().map(|x| x / length).collect()
    }

    #[test]
    fn a_rotation_keeps_lengths_and_dot_products() {
        let mut random = Random::new(1);
        for dimensions in [3, 64, 100, 1024] {
            let (a, b) = (unit(&mut random, dimensions), unit(&mut random, dimensions));
            let size = turned_dimensions(dimensions);
            let (ta, tb) = (rotate(&a, size), rotate(&b, size));
            let dot = |x: &[f32], y: &[f32]| x.iter().zip(y).map(|(p, q)| p * q).sum::<f32>();
            assert!((dot(&ta, &ta) - 1.0).abs() < 1e-4, "{dimensions}");
            assert!((dot(&ta, &tb) - dot(&a, &b)).abs() < 1e-4, "{dimensions}");
        }
    }

    #[test]
    fn an_estimate_lies_near_the_cosine() {
        let mut random = Random::new(2);
        let dimensions = 256;
        let centroid = unit(&mut random, dimensions);
        let mut error = 0.0;
        for _ in 0..200 {
            let noise = unit(&mut random, dimensions);
            let mixed: Vec<f32> = centroid.iter().zip(&noise).map(|(c, n)| c + 0.6 * n).collect();
            let length = mixed.iter().map(|x| x * x).sum::<f32>().sqrt();
            let member: Vec<f32> = mixed.iter().map(|x| x / length).collect();
            let question = unit(&mut random, dimensions);
            let toward: f32 = question.iter().zip(&centroid).map(|(q, c)| q * c).sum();
            let cosine: f32 = question.iter().zip(&member).map(|(q, m)| q * m).sum();
            let estimate = Turned::new(&question).estimate(toward, &code(&member, &centroid));
            error += f64::from((estimate - cosine).abs());
        }
        assert!(error / 200.0 < 0.03, "a mean error of {}", error / 200.0);
    }

    #[test]
    fn a_member_at_its_centroid_is_estimated_by_the_centroid() {
        let mut random = Random::new(3);
        let centroid = unit(&mut random, 16);
        let code = code(&centroid, &centroid);
        assert_eq!(code.len(), code_size(16));
        assert_eq!(Turned::new(&unit(&mut random, 16)).estimate(0.25, &code), 0.25);
    }
}
