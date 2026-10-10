//! The scrub: every stored byte read again over thirty days, a slice a minute,
//! so that bytes the disk changed are found before a read needs them. A slice
//! reads a 43,200th of the bytes files name and at least 1 MiB.

use std::io;

use rusqlite::{OptionalExtension, params};
use sha2::{Digest, Sha256};

use super::disk::read_at;
use super::engine::{Blobs, lock};
use crate::Result;
use crate::sqlite::sql_error;

/// Slices in a pass: thirty days of a slice a minute.
const SLICES: i64 = 30 * 24 * 60;

/// What a slice reads at once, and holds of the store's memory.
const BUFFER: usize = 1 << 20;

const SELECT_PLACE: &str = "select content, pace from scrub";
const UPDATE_PLACE: &str = "update scrub set content = ?1, pace = ?2";
const NAMED_BYTES: &str = "select coalesce(sum(size), 0) from contents where names > 0";
const NEXT: &str = "select id, size, sha256, inline from contents
    where id >= ?1 and names > 0 and damaged = 0 order by id limit 1";
const BODY_OF: &str = "select bytes from bodies where id = ?1";
const MARK_DAMAGED: &str = "update contents set damaged = 1 where id = ?1 and names > 0";
const PATHS_OF: &str = "select s.name, f.path from files as f join sets as s on s.id = f.set_id
    where f.content = ?1 limit 16";

/// A content the scrub is part way through: the hash of its bytes before
/// `offset`, kept in memory between slices.
pub(crate) struct Scrubbing {
    content: i64,
    offset: u64,
    hash: Sha256,
}

struct Next {
    id: i64,
    size: u64,
    sha256: Vec<u8>,
    inline: bool,
}

impl Blobs {
    /// Reads a slice of the contents in the order of their ids from where the
    /// last slice stopped; says how many bytes it read and how many contents
    /// it found damaged. A slice for which the store's memory is taken waits
    /// for the next minute.
    pub(crate) fn scrub(&self) -> Result<(u64, usize)> {
        let Ok(_reserved) = self.memory.try_reserve(BUFFER as u64, "the scrub's buffer") else {
            return Ok((0, 0));
        };
        let (mut content, mut pace) = self.scrub_place()?;
        let mut state = lock(&self.scrubbing).take();
        let (mut read, mut damaged) = (0_u64, 0);
        let mut buffer = vec![0_u8; BUFFER];
        while read < u64::try_from(pace).unwrap_or(0) {
            let Some(next) = self.next_to_scrub(content)? else {
                (content, pace, state) = (0, 0, None);
                break;
            };
            let mut scrubbing = state.take().filter(|scrubbing| scrubbing.content == next.id).unwrap_or(Scrubbing {
                content: next.id,
                offset: 0,
                hash: Sha256::new(),
            });
            content = next.id;
            let left = u64::try_from(pace).unwrap_or(0) - read;
            match self.read_on(&next, &mut scrubbing, &mut buffer, left) {
                Ok(n) => read += n,
                Err(error)
                    if error.kind() == io::ErrorKind::NotFound || error.kind() == io::ErrorKind::UnexpectedEof =>
                {
                    damaged += usize::from(self.judge(&next, "missing or shorter than its size")?);
                    content = next.id + 1;
                    continue;
                }
                Err(error) => return Err(crate::Error::io("blobs: scrub a file", error)),
            }
            if scrubbing.offset < next.size {
                state = Some(scrubbing);
                break;
            }
            if scrubbing.hash.finalize().as_slice() != next.sha256.as_slice() {
                damaged += usize::from(self.judge(&next, "changed")?);
            }
            content = next.id + 1;
        }
        *lock(&self.scrubbing) = state;
        self.file.write(0, move |tx| {
            tx.execute(UPDATE_PLACE, params![content, pace])
                .map(drop)
                .map_err(|error| sql_error("blobs: the scrub's place", error))
        })?;
        Ok((read, damaged))
    }

    /// Where the scrub stopped and the bytes a slice reads in this pass; a
    /// pass begins when the last one ended, its pace from what files name now.
    fn scrub_place(&self) -> Result<(i64, i64)> {
        let (content, pace): (i64, i64) = self.file.read(|connection| {
            connection
                .query_row(SELECT_PLACE, [], |row| Ok((row.get(0)?, row.get(1)?)))
                .map_err(|error| sql_error("blobs: the scrub's place", error))
        })?;
        if pace > 0 {
            return Ok((content, pace));
        }
        let total: i64 = self.file.read(|connection| {
            connection
                .query_row(NAMED_BYTES, [], |row| row.get(0))
                .map_err(|error| sql_error("blobs: the bytes files name", error))
        })?;
        Ok((0, (total / SLICES).max(BUFFER as i64)))
    }

    fn next_to_scrub(&self, from: i64) -> Result<Option<Next>> {
        self.file.read(|connection| {
            connection
                .prepare_cached(NEXT)
                .and_then(|mut statement| {
                    statement
                        .query_row([from], |row| {
                            Ok(Next {
                                id: row.get(0)?,
                                size: u64::try_from(row.get::<_, i64>(1)?).unwrap_or(0),
                                sha256: row.get(2)?,
                                inline: row.get(3)?,
                            })
                        })
                        .optional()
                })
                .map_err(|error| sql_error("blobs: the next content to scrub", error))
        })
    }

    /// Reads on through one content, at most `left` bytes, into its hash;
    /// says how many it read.
    fn read_on(&self, next: &Next, scrubbing: &mut Scrubbing, buffer: &mut [u8], left: u64) -> io::Result<u64> {
        if next.inline {
            let body: Option<Vec<u8>> = self
                .file
                .read(|connection| {
                    connection
                        .query_row(BODY_OF, [next.id], |row| row.get(0))
                        .optional()
                        .map_err(|error| sql_error("blobs: an inline body", error))
                })
                .map_err(io::Error::other)?;
            let body = body.filter(|body| body.len() as u64 == next.size).ok_or(io::ErrorKind::UnexpectedEof)?;
            scrubbing.hash.update(&body);
            scrubbing.offset = next.size;
            return Ok(next.size);
        }
        let file = std::fs::File::open(self.disk.object_path(next.id))?;
        let mut read = 0;
        while scrubbing.offset < next.size && read < left {
            let wanted =
                usize::try_from((next.size - scrubbing.offset).min(buffer.len() as u64)).unwrap_or(buffer.len());
            let n = read_at(&file, &mut buffer[..wanted], scrubbing.offset)?;
            if n == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            scrubbing.hash.update(&buffer[..n]);
            scrubbing.offset += n as u64;
            read += n as u64;
        }
        Ok(read)
    }

    /// Marks a content damaged, so that its next read is `corrupt`, and logs
    /// it once with the paths that name it; a content that lost its last name
    /// while it was read is left alone. A backup is what repairs it. Says
    /// whether it marked one.
    fn judge(&self, next: &Next, why: &'static str) -> Result<bool> {
        let id = next.id;
        let marked = self.file.write(0, move |tx| {
            tx.execute(MARK_DAMAGED, [id]).map_err(|error| sql_error("blobs: mark a content damaged", error))
        })? == 1;
        if marked {
            let paths: Vec<String> = self.file.read(|connection| {
                let mut statement = connection
                    .prepare_cached(PATHS_OF)
                    .map_err(|error| sql_error("blobs: the paths of a content", error))?;
                let rows = statement
                    .query_map([id], |row| Ok(format!("{}/{}", row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
                    .map_err(|error| sql_error("blobs: the paths of a content", error))?;
                rows.collect::<rusqlite::Result<Vec<String>>>()
                    .map_err(|error| sql_error("blobs: the paths of a content", error))
            })?;
            tracing::error!(target: "tinystore", content = id, ?paths, "blobs: a file's bytes are {why}");
        }
        Ok(marked)
    }
}
