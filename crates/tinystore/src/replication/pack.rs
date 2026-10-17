//! A link's bytes after its hello and welcome, through one zstd stream each
//! way. A commit's pages are mostly the pages of the commits just before, which
//! the stream's window still holds, so an entry costs about what changed
//! rather than its pages.
//!
//! Each send is flushed, so the other end reads every message whole as it
//! arrives. The window is 4 MiB a way, a link's memory being bounded for a
//! node that serves twenty others.

use std::io;

use zstd::stream::raw::{Decoder, Encoder, InBuffer, Operation, OutBuffer};
use zstd::zstd_safe::{CParameter, DParameter};

const LEVEL: i32 = 3;
const WINDOW_LOG: u32 = 22;
/// What one step of the stream writes at most.
const STEP: usize = 128 << 10;

/// The sending end of a link's stream.
pub(crate) struct Packer(Encoder<'static>);

impl Packer {
    pub(crate) fn new() -> io::Result<Packer> {
        let mut encoder = Encoder::new(LEVEL)?;
        encoder.set_parameter(CParameter::WindowLog(WINDOW_LOG))?;
        Ok(Packer(encoder))
    }

    /// `bytes` packed and flushed, so that the other end reads them whole now.
    pub(crate) fn pack(&mut self, bytes: &[u8]) -> io::Result<Vec<u8>> {
        let mut packed = Vec::with_capacity(bytes.len() / 2 + 64);
        let mut step = vec![0u8; STEP];
        let mut input = InBuffer::around(bytes);
        while input.pos() < bytes.len() {
            let mut output = OutBuffer::around(&mut step[..]);
            self.0.run(&mut input, &mut output)?;
            let written = output.pos();
            packed.extend_from_slice(&step[..written]);
        }
        loop {
            let mut output = OutBuffer::around(&mut step[..]);
            let left = self.0.flush(&mut output)?;
            let written = output.pos();
            packed.extend_from_slice(&step[..written]);
            if left == 0 {
                return Ok(packed);
            }
        }
    }
}

/// The receiving end of a link's stream.
pub(crate) struct Unpacker(Decoder<'static>);

impl Unpacker {
    pub(crate) fn new() -> io::Result<Unpacker> {
        let mut decoder = Decoder::new()?;
        decoder.set_parameter(DParameter::WindowLogMax(WINDOW_LOG))?;
        Ok(Unpacker(decoder))
    }

    /// Unpacks what arrived, appending what it holds to `into`.
    pub(crate) fn unpack(&mut self, bytes: &[u8], into: &mut Vec<u8>) -> io::Result<()> {
        let mut step = vec![0u8; STEP];
        let mut input = InBuffer::around(bytes);
        loop {
            let mut output = OutBuffer::around(&mut step[..]);
            self.0.run(&mut input, &mut output)?;
            let written = output.pos();
            into.extend_from_slice(&step[..written]);
            if input.pos() == bytes.len() && written < STEP {
                return Ok(());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_is_packed_unpacks_as_it_was_and_a_page_sent_again_costs_little() {
        let (mut packer, mut unpacker) = (Packer::new().unwrap(), Unpacker::new().unwrap());
        let page: Vec<u8> = (0..4096u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8).collect();
        let first = packer.pack(&page).unwrap();
        let mut changed = page.clone();
        changed[100] ^= 1;
        let again = packer.pack(&changed).unwrap();
        assert!(again.len() < first.len() / 8, "{} bytes again against {}", again.len(), first.len());
        let mut unpacked = Vec::new();
        unpacker.unpack(&first[..10], &mut unpacked).unwrap();
        unpacker.unpack(&first[10..], &mut unpacked).unwrap();
        unpacker.unpack(&again, &mut unpacked).unwrap();
        assert!(unpacked[..4096] == page[..] && unpacked[4096..] == changed[..]);
    }
}
