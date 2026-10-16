//! A file's log: its commits as entries, in segment files of
//! `log/<file>/`, each named by the sequence of its first entry.
//!
//! An entry is appended while the file's writer is held, with no sync of its
//! own: the WAL holds the commit durably until the capture's checkpoint, and
//! the checkpoint syncs the log first. A segment's tail torn by a crash is cut
//! off at open, and the capture hands what it lost again from the WAL.

use std::collections::VecDeque;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use super::entry::{self, Entry, Head, Kind, MAGIC};
use crate::sqlite::{Commit, Observe, Position};
use crate::{Error, ErrorKind, Result};

/// How large a log grows.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Bounds {
    /// A segment past these bytes is closed, and the next entry starts one.
    pub(crate) segment: u64,
    /// The bytes of a file's segments kept; the oldest go past them, and a
    /// copy that needed them starts again from a snapshot.
    pub(crate) keep: u64,
}

impl Default for Bounds {
    fn default() -> Self {
        Bounds { segment: 16 << 20, keep: 256 << 20 }
    }
}

pub(crate) struct Log {
    dir: PathBuf,
    bounds: Bounds,
    tail: Mutex<Tail>,
}

struct Tail {
    /// Every segment, oldest first; appends go to the last.
    segments: VecDeque<Segment>,
    appending: Option<fs::File>,
    next: u64,
    epoch: u64,
    check: [u8; 16],
    last: Option<Position>,
    unsynced: bool,
    /// A segment was made since the directory's last sync.
    named: bool,
}

struct Segment {
    first: u64,
    bytes: u64,
}

impl Log {
    /// Opens the log in `dir`, its last segment's torn tail cut off.
    pub(crate) fn open(dir: PathBuf, bounds: Bounds) -> Result<Log> {
        fs::create_dir_all(&dir).map_err(|error| Error::io(format!("log {}", dir.display()), error))?;
        let mut tail = Tail {
            segments: VecDeque::new(),
            appending: None,
            next: 1,
            epoch: 1,
            check: [0; 16],
            last: None,
            unsynced: false,
            named: false,
        };
        for first in segments_in(&dir)? {
            let path = segment_path(&dir, first);
            let bytes = fs::metadata(&path).map_err(|error| Error::io(path.display().to_string(), error))?.len();
            tail.segments.push_back(Segment { first, bytes });
        }
        let log = Log { dir, bounds, tail: Mutex::new(tail) };
        log.find_the_end().map_err(|error| error.within(format!("log {}", log.dir.display())))?;
        Ok(log)
    }

    /// Every entry the log holds, oldest first.
    #[cfg_attr(not(test), expect(dead_code, reason = "the links read the log, phase 7's fourth slice"))]
    pub(crate) fn read(dir: &Path, mut each: impl FnMut(Entry<'_>) -> Result<()>) -> Result<()> {
        for first in segments_in(dir)? {
            let bytes = read_segment(&segment_path(dir, first))?;
            let mut at = MAGIC.len();
            while let Some((entry, used)) = entry::decode(&bytes[at..])? {
                each(entry)?;
                at += used;
            }
        }
        Ok(())
    }

    /// Reads the last segment that holds an entry to the end of its last
    /// whole one, cuts what follows, and removes the segments after it,
    /// which a crash left before their first entry.
    fn find_the_end(&self) -> Result<()> {
        let mut tail = self.lock();
        while let Some(segment) = tail.segments.back() {
            let path = segment_path(&self.dir, segment.first);
            let bytes = read_segment(&path)?;
            let mut at = MAGIC.len();
            let mut last: Option<(u64, u64, [u8; 16], Position)> = None;
            while let Some((entry, used)) = entry::decode(&bytes[at..]).unwrap_or(None) {
                let follows = last
                    .is_none_or(|(sequence, _, check, _)| entry.sequence == sequence + 1 && entry.previous == check);
                if !follows {
                    break;
                }
                last = Some((entry.sequence, entry.epoch, entry.check, entry.position));
                at += used;
            }
            let Some((sequence, epoch, check, position)) = last else {
                fs::remove_file(&path).map_err(|error| Error::io(path.display().to_string(), error))?;
                tail.segments.pop_back();
                continue;
            };
            if at < bytes.len() {
                let file = OpenOptions::new().write(true).open(&path).map_err(|error| io_error(&path, error))?;
                file.set_len(at as u64).and_then(|()| file.sync_data()).map_err(|error| io_error(&path, error))?;
            }
            tail.segments.back_mut().expect("the segment just read").bytes = at as u64;
            (tail.next, tail.epoch, tail.check, tail.last) = (sequence + 1, epoch, check, Some(position));
            tail.appending = Some(OpenOptions::new().append(true).open(&path).map_err(|error| io_error(&path, error))?);
            return Ok(());
        }
        Ok(())
    }

    fn append(&self, kind: Kind, position: Position, page_size: u32, size: u32, pages: &[(u32, &[u8])]) -> Result<()> {
        let mut tail = self.lock();
        let head =
            Head { kind, sequence: tail.next, epoch: tail.epoch, previous: tail.check, position, page_size, size };
        let (bytes, check) = entry::encode(&head, pages);
        let full = tail.segments.back().is_none_or(|segment| segment.bytes >= self.bounds.segment);
        if full {
            self.start_segment(&mut tail)?;
        }
        let path = segment_path(&self.dir, tail.segments.back().expect("a segment to append to").first);
        let file = tail.appending.as_mut().expect("the last segment is open");
        file.write_all(&bytes).map_err(|error| io_error(&path, error))?;
        tail.segments.back_mut().expect("the segment just written").bytes += bytes.len() as u64;
        (tail.next, tail.check, tail.last, tail.unsynced) = (head.sequence + 1, check, Some(position), true);
        Ok(())
    }

    /// Closes the segment appended to, durably, starts the next at the next
    /// entry's sequence, and removes the oldest past the bytes kept.
    fn start_segment(&self, tail: &mut Tail) -> Result<()> {
        if let Some(file) = tail.appending.take() {
            file.sync_data().map_err(|error| Error::io(format!("log {}", self.dir.display()), error))?;
        }
        let path = segment_path(&self.dir, tail.next);
        let mut file =
            OpenOptions::new().append(true).create_new(true).open(&path).map_err(|error| io_error(&path, error))?;
        file.write_all(MAGIC).map_err(|error| io_error(&path, error))?;
        tail.segments.push_back(Segment { first: tail.next, bytes: MAGIC.len() as u64 });
        tail.appending = Some(file);
        tail.named = true;
        while tail.segments.len() > 1
            && tail.segments.iter().map(|segment| segment.bytes).sum::<u64>() > self.bounds.keep
        {
            let oldest = tail.segments.pop_front().expect("more than one segment");
            let path = segment_path(&self.dir, oldest.first);
            fs::remove_file(&path).map_err(|error| io_error(&path, error))?;
        }
        Ok(())
    }

    fn lock(&self) -> MutexGuard<'_, Tail> {
        self.tail.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Observe for Log {
    fn last(&self) -> Option<Position> {
        self.lock().last
    }

    fn begin(&self, position: Position) -> Result<()> {
        self.append(Kind::Begin, position, 0, 0, &[])
    }

    fn committed(&self, commit: &Commit<'_>) -> Result<()> {
        self.append(Kind::Commit, commit.position, commit.page_size, commit.size, &commit.pages)
    }

    fn gap(&self, position: Position) -> Result<()> {
        tracing::warn!(
            target: "tinystore",
            log = %self.dir.display(),
            "the file was written beside the store's writer: a copy of it starts again from a snapshot"
        );
        self.append(Kind::Gap, position, 0, 0, &[])
    }

    fn sync(&self) -> Result<()> {
        let mut tail = self.lock();
        if tail.unsynced {
            let file = tail.appending.as_ref().expect("an entry was appended");
            file.sync_data().map_err(|error| Error::io(format!("log {}", self.dir.display()), error))?;
            tail.unsynced = false;
        }
        if tail.named {
            crate::durable::sync_directory(&self.dir)
                .map_err(|error| Error::io(format!("log {}", self.dir.display()), error))?;
            tail.named = false;
        }
        Ok(())
    }
}

/// The first sequences of the segments in `dir`, in order.
fn segments_in(dir: &Path) -> Result<Vec<u64>> {
    let mut firsts = Vec::new();
    let listed = match fs::read_dir(dir) {
        Ok(listed) => listed,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(firsts),
        Err(error) => return Err(io_error(dir, error)),
    };
    for found in listed {
        let found = found.map_err(|error| io_error(dir, error))?;
        let name = found.file_name();
        let first = name.to_str().and_then(|name| name.strip_suffix(".log")).and_then(|first| first.parse().ok());
        if let Some(first) = first {
            firsts.push(first);
        }
    }
    firsts.sort_unstable();
    Ok(firsts)
}

fn segment_path(dir: &Path, first: u64) -> PathBuf {
    dir.join(format!("{first:020}.log"))
}

fn read_segment(path: &Path) -> Result<Vec<u8>> {
    let bytes = fs::read(path).map_err(|error| io_error(path, error))?;
    if bytes.len() < MAGIC.len() {
        return Ok(MAGIC.to_vec());
    }
    if bytes[..MAGIC.len()] != MAGIC[..] {
        return Err(Error::new(ErrorKind::Corrupt, format!("{}: a segment of another format", path.display())));
    }
    Ok(bytes)
}

fn io_error(path: &Path, error: io::Error) -> Error {
    Error::io(path.display().to_string(), error)
}
