//! A directory's file held open by a read: each piece read looks at the
//! file's size and time once, after its bytes, so that a piece that holds a
//! byte of another version fails rather than reach the reader (BLOB-13).

use std::fs::File;
use std::io;

use super::nanos_of;
use crate::blobs::disk::read_at;
use crate::{Error, ErrorKind};

#[derive(Debug)]
pub(crate) struct Outside {
    file: File,
    /// The version `get` opened: the file's size and its time in nanoseconds.
    size: u64,
    modified: i64,
    /// Whether a read holds to no version and goes up to the size now.
    growing: bool,
}

/// A piece of a growing file: its bytes, and the file's size and time as it
/// was read.
pub(crate) struct Piece {
    pub(crate) read: usize,
    pub(crate) size: u64,
    pub(crate) modified: i64,
}

impl Outside {
    pub(super) fn new(file: File, growing: bool) -> io::Result<Outside> {
        let (size, modified) = version(&file)?;
        Ok(Outside { file, size, modified, growing })
    }

    pub(crate) fn size(&self) -> u64 {
        self.size
    }

    pub(crate) fn modified(&self) -> i64 {
        self.modified
    }

    pub(crate) fn is_growing(&self) -> bool {
        self.growing
    }

    /// Reads at `offset` up to the size `get` opened, or in a growing file up
    /// to its size now, then looks at the file: one that moved since `get`
    /// fails `conflict` unless it is growing.
    pub(crate) fn read_at(&self, bytes: &mut [u8], offset: u64) -> io::Result<Piece> {
        let wanted = if self.growing {
            bytes.len()
        } else {
            let left = self.size.saturating_sub(offset);
            usize::try_from(left).map_or(bytes.len(), |left| left.min(bytes.len()))
        };
        let read = if wanted == 0 { 0 } else { read_at(&self.file, &mut bytes[..wanted], offset)? };
        let (size, modified) = version(&self.file)?;
        if !self.growing && (size, modified) != (self.size, self.modified) {
            let moved = Error::new(ErrorKind::Conflict, "the file changed since its get: get it again");
            return Err(io::Error::other(moved));
        }
        if !self.growing && read == 0 && wanted > 0 {
            return Err(io::Error::other(Error::new(ErrorKind::Conflict, "the file ends before its size")));
        }
        Ok(Piece { read, size, modified })
    }
}

/// A file's size and modification time, from its open handle: `fstat`.
pub(super) fn version(file: &File) -> io::Result<(u64, i64)> {
    let metadata = file.metadata()?;
    Ok((metadata.len(), nanos_of(metadata.modified()?)))
}
