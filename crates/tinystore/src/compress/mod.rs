//! What records and metrics both keep their bytes with: integers of variable
//! width and a reader that bounds every length before it takes it, bits
//! packed and read a word at a time, rice codes, zstd frames, and the CRC-32
//! every stored row ends with. Neither engine imports the other: both stand
//! on this, and search reads its postings' integers with the same reader.

#[cfg(any(feature = "records", feature = "metrics"))]
pub(crate) mod bits;
pub(crate) mod checksum;
#[cfg(any(feature = "records", feature = "metrics"))]
pub(crate) mod frame;
#[cfg(any(feature = "records", feature = "metrics", feature = "search"))]
pub(crate) mod varint;
