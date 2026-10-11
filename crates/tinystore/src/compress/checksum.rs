//! The CRC-32 of zlib and Ethernet, which every stored object of records and
//! metrics ends with, so that bytes that changed under it read as `corrupt`
//! rather than as other data.

/// The checksum of `data`, as zlib's `crc32` gives it.
#[cfg_attr(all(not(test), not(feature = "records")), expect(dead_code, reason = "only records uses it"))]
pub(crate) fn crc32(data: &[u8]) -> u32 {
    Crc32::new().update(data).finish()
}

/// A checksum taken over several pieces, as of their bytes one after
/// another: what binds an object to what holds it, a key and then its bytes.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Crc32(u32);

impl Crc32 {
    pub(crate) fn new() -> Self {
        Crc32(!0)
    }

    /// Sixteen bytes at a time: table `n` holds what a byte adds to the
    /// remainder with `n` bytes after it, so that the sixteen of a step are
    /// looked up apart and summed, and a block's checksum costs a few times
    /// less than its bytes one at a time.
    #[must_use]
    pub(crate) fn update(self, data: &[u8]) -> Self {
        let (steps, rest) = data.as_chunks::<16>();
        let mut crc = self.0;
        for step in steps {
            let mut bytes = *step;
            crc.to_le_bytes().iter().enumerate().for_each(|(place, byte)| bytes[place] ^= byte);
            crc = bytes.iter().enumerate().fold(0, |sum, (place, byte)| sum ^ TABLES[15 - place][usize::from(*byte)]);
        }
        Crc32(rest.iter().fold(crc, |crc, &byte| TABLES[0][((crc ^ u32::from(byte)) & 0xff) as usize] ^ (crc >> 8)))
    }

    pub(crate) fn finish(self) -> u32 {
        !self.0
    }
}

const TABLES: [[u32; 256]; 16] = {
    let mut tables = [[0_u32; 256]; 16];
    let mut byte = 0;
    while byte < 256 {
        let mut crc = byte as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 == 1 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
            bit += 1;
        }
        tables[0][byte] = crc;
        byte += 1;
    }
    let mut table = 1;
    while table < 16 {
        let mut byte = 0;
        while byte < 256 {
            let before = tables[table - 1][byte];
            tables[table][byte] = (before >> 8) ^ tables[0][(before & 0xff) as usize];
            byte += 1;
        }
        table += 1;
    }
    tables
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_checksum_is_zlibs() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
        assert_eq!(crc32(b""), 0);
        // sixteen bytes at a time give what a byte at a time gives, across every way a length ends
        let bitwise = |data: &[u8]| {
            let step =
                |crc: u32| (0..8).fold(crc, |crc, _| if crc & 1 == 1 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 });
            !data.iter().fold(!0_u32, |crc, byte| step(crc ^ u32::from(*byte)))
        };
        let data: Vec<u8> = (0..300_u32).map(|n| (n.wrapping_mul(2_654_435_761) >> 13) as u8).collect();
        for length in 0..data.len() {
            assert_eq!(crc32(&data[..length]), bitwise(&data[..length]), "{length}");
        }
    }

    #[test]
    fn pieces_sum_as_their_bytes_one_after_another() {
        let data: Vec<u8> = (0..100_u8).collect();
        for cut in 0..data.len() {
            let (key, rest) = data.split_at(cut);
            assert_eq!(Crc32::new().update(key).update(rest).finish(), crc32(&data), "{cut}");
        }
    }
}
