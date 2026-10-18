//! zstd frames, through contexts made at their first use and kept for the
//! next, so that a call does not pay for making them every time.

use crate::{Error, Result};

/// The contexts of one level: a compressor of that level, and a
/// decompressor, which reads a frame of any level.
pub(crate) struct Zstd {
    level: i32,
    /// The window, as a power of two, of a compressor whose objects never
    /// pass it: a frame then needs no larger one to read, and a decompressor
    /// refuses a frame that asks for more.
    window_log: Option<u32>,
    compressor: Option<zstd::bulk::Compressor<'static>>,
    decompressor: Option<zstd::bulk::Decompressor<'static>>,
}

impl Zstd {
    #[cfg_attr(not(feature = "records"), expect(dead_code, reason = "only records uses it"))]
    pub(crate) const fn new(level: i32) -> Zstd {
        Zstd { level, window_log: None, compressor: None, decompressor: None }
    }

    /// For objects of at most 2^`window_log` bytes.
    #[cfg_attr(all(not(test), not(feature = "metrics")), expect(dead_code, reason = "only metrics uses it"))]
    pub(crate) const fn windowed(level: i32, window_log: u32) -> Zstd {
        Zstd { level, window_log: Some(window_log), compressor: None, decompressor: None }
    }

    /// `plain` as one frame.
    pub(crate) fn squeeze(&mut self, plain: &[u8]) -> Result<Vec<u8>> {
        let failed = |error| Error::internal("zstd could not write a frame").with_source(error);
        let compressor = match &mut self.compressor {
            Some(compressor) => compressor,
            slot @ None => {
                let mut made = zstd::bulk::Compressor::new(self.level).map_err(failed)?;
                if let Some(window_log) = self.window_log {
                    made.set_parameter(zstd::zstd_safe::CParameter::WindowLog(window_log)).map_err(failed)?;
                }
                slot.insert(made)
            }
        };
        compressor.compress(plain).map_err(failed)
    }

    /// Expands a frame into `most` bytes at most: a frame that says more is
    /// refused before anything is allocated for it.
    pub(crate) fn expand(&mut self, frame: &[u8], most: usize) -> Result<Vec<u8>> {
        let decompressor = match &mut self.decompressor {
            Some(decompressor) => decompressor,
            slot @ None => {
                let failed = |error| Error::internal("zstd could not start").with_source(error);
                let mut made = zstd::bulk::Decompressor::new().map_err(failed)?;
                if let Some(window_log) = self.window_log {
                    made.set_parameter(zstd::zstd_safe::DParameter::WindowLogMax(window_log)).map_err(failed)?;
                }
                slot.insert(made)
            }
        };
        decompressor.decompress(frame, most).map_err(|_| Error::corrupt("a row's frame"))
    }
}

impl std::fmt::Debug for Zstd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Zstd({})", self.level)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_reads_back_within_its_bound_and_not_past_it() {
        let plain: Vec<u8> = (0..4000_u32).map(|n| (n % 7) as u8).collect();
        let mut zstd = Zstd::windowed(3, 13);
        let frame = zstd.squeeze(&plain).unwrap();
        assert!(frame.len() < plain.len());
        assert_eq!(zstd.expand(&frame, plain.len()).unwrap(), plain);
        assert_eq!(zstd.expand(&frame, plain.len() - 1).unwrap_err().to_string(), "a row's frame: corrupt");
        assert_eq!(zstd.expand(&frame[..frame.len() - 1], 8192).unwrap_err().to_string(), "a row's frame: corrupt");
    }
}
