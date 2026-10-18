//! A file's first copy on another node: its pages as they lie, read a chunk
//! at a time between two points of its log, then the entries between them.
//!
//! ```text
//! source:  point P0 ── chunks, each read at some commit ≥ P0 ── point P1
//! copy:    pages written as they come, then entries P0+1 … P1 over them
//! ```
//!
//! Each chunk is read at a commit between P0 and P1, and each entry after P0
//! carries every page its commit wrote; so once the entries up to P1 are
//! written over the chunks, every page is P1's. Until then the file is no
//! file at all, so it is put together under a name of its own, by plain
//! writes, and takes the copy's name only once whole: until then the copy
//! held before, if any, stays as it was for its readers.

use std::fs::{self, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use super::copy::{self, Position, io_error};
use super::entry::{Entry, Kind, Point};
use super::log::Log;
use crate::sqlite::{Chunk, Pages};
use crate::{Error, ErrorKind, Result};

/// The pages a chunk reads in one transaction: a mebibyte of 4 KiB pages.
pub(crate) const CHUNK: u32 = 256;

/// Where a snapshot's file is put together beside the copy's name.
const TAKING: &str = "taking";

/// A copy being put together from a snapshot.
pub(crate) struct Taking {
    /// The copy's log directory, where its log and position are kept.
    dir: PathBuf,
    /// The copy's file, the name the snapshot takes once whole.
    target: PathBuf,
    temp: PathBuf,
    file: fs::File,
    page_size: u32,
    /// The entry the pages written so far stand at once whole.
    point: Point,
    /// The point the source's file reached by the snapshot's end: the file is
    /// whole once the entries up to it are written.
    ready_at: Option<u64>,
}

impl Taking {
    /// Starts the snapshot of the file `target`, whose log is in `dir`, from
    /// pages of `page_size` read at `at` or later.
    pub(crate) fn begin(dir: &Path, target: &Path, page_size: u32, at: Point) -> Result<Taking> {
        copy::mark(dir)?;
        let temp = dir.join(TAKING);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temp)
            .map_err(|error| io_error(&temp, error))?;
        Ok(Taking { dir: dir.to_owned(), target: target.to_owned(), temp, file, page_size, point: at, ready_at: None })
    }

    /// Writes pages of the snapshot.
    pub(crate) fn pages<P: AsRef<[u8]>>(&mut self, pages: &[(u32, P)]) -> Result<()> {
        for (number, page) in pages {
            self.write_page(*number, page.as_ref())?;
        }
        Ok(())
    }

    /// The source read its last chunk with its log at `ready_at`.
    pub(crate) fn end(&mut self, ready_at: u64) {
        self.ready_at = Some(ready_at);
    }

    /// Writes an entry after the snapshot's first point over the pages; one
    /// before it is nothing. The copy's log keeps what follows the snapshot.
    pub(crate) fn apply(&mut self, entry: &Entry<'_>) -> Result<()> {
        if entry.sequence <= self.point.sequence {
            return Ok(());
        }
        if !entry.follows(&self.point) {
            return Err(Error::new(
                ErrorKind::Conflict,
                format!("entry {} does not follow the snapshot's {}", entry.sequence, self.point.sequence),
            ));
        }
        match entry.kind {
            Kind::Commit => {
                if entry.page_size != self.page_size {
                    return Err(Error::corrupt(format!(
                        "an entry's pages are {} bytes and the snapshot's {}",
                        entry.page_size, self.page_size
                    )));
                }
                self.pages(&entry.pages)?;
                let size = u64::from(entry.size) * u64::from(self.page_size);
                self.file.set_len(size).map_err(|error| io_error(&self.temp, error))?;
            }
            Kind::Begin | Kind::Gap => {
                return Err(Error::new(
                    ErrorKind::Conflict,
                    format!("its home's log starts again at entry {}: the snapshot starts again", entry.sequence),
                ));
            }
        }
        self.point = entry.point();
        Ok(())
    }

    /// Whether every page is the source's at one commit.
    pub(crate) fn whole(&self) -> bool {
        self.ready_at.is_some_and(|ready_at| self.point.sequence >= ready_at)
    }

    /// Gives the whole file the copy's name, durably, in place of the copy
    /// held before, then keeps its position: a crash before leaves a copy that
    /// starts again from a snapshot, never a file that is not whole. The copy
    /// held before must be closed: on Windows a file open is not replaced.
    pub(crate) fn finish(self) -> Result<()> {
        self.file.sync_all().map_err(|error| io_error(&self.temp, error))?;
        drop(self.file);
        let position = self.dir.join(Position::FILE);
        match fs::remove_file(&position) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_error(&position, error)),
        }
        remove_file_and_wal(&self.target)?;
        Log::clear(&self.dir)?;
        if let Some(parent) = self.target.parent() {
            fs::create_dir_all(parent).map_err(|error| io_error(parent, error))?;
        }
        fs::rename(&self.temp, &self.target).map_err(|error| io_error(&self.target, error))?;
        if let Some(parent) = self.target.parent() {
            crate::durable::sync_directory(parent).map_err(|error| io_error(parent, error))?;
        }
        let mut position = Position::read(self.dir.join(Position::FILE))?;
        position.keep_durably(self.point)
    }

    fn write_page(&mut self, number: u32, page: &[u8]) -> Result<()> {
        if number == 0 || page.len() != self.page_size as usize {
            return Err(Error::corrupt(format!(
                "a snapshot's page {number} is not a page of {} bytes",
                self.page_size
            )));
        }
        let at = u64::from(number - 1) * u64::from(self.page_size);
        self.file
            .seek(SeekFrom::Start(at))
            .and_then(|_| self.file.write_all(page))
            .map_err(|error| io_error(&self.temp, error))
    }
}

/// Removes a copy's file and what SQLite keeps beside it, for a snapshot
/// that takes its place.
fn remove_file_and_wal(target: &Path) -> Result<()> {
    for suffix in ["", "-wal", "-shm"] {
        let mut path = target.as_os_str().to_owned();
        path.push(suffix);
        let path = PathBuf::from(path);
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_error(&path, error)),
        }
    }
    Ok(())
}

/// What a snapshot is read from: a file's path, and how to read where its
/// log stands with no commit under way.
pub(crate) trait Source {
    fn path(&self) -> &Path;
    fn stable(&self) -> Result<Point>;
}

/// What a snapshot hands its taker, in order.
pub(crate) enum Part {
    Begin { page_size: u32, at: Point },
    Pages(Vec<(u32, Vec<u8>)>),
    End { ready_at: u64 },
}

/// Reads the snapshot of `source`, handing each part to `send`, and gives
/// the point whose next entry the taker needs after it.
pub(crate) fn read(source: &dyn Source, mut send: impl FnMut(Part) -> Result<()>) -> Result<Point> {
    let what = || format!("a snapshot of {}", source.path().display());
    let at = source.stable().map_err(|error| error.within(what()))?;
    let pages = Pages::open(source.path()).map_err(|error| error.within(what()))?;
    let mut first = 1;
    let Chunk { page_size, size, pages: read } = pages.read(first, CHUNK).map_err(|error| error.within(what()))?;
    send(Part::Begin { page_size, at })?;
    let mut more = first.saturating_add(CHUNK) <= size;
    first += CHUNK;
    send(Part::Pages(read))?;
    while more {
        let chunk = pages.read(first, CHUNK).map_err(|error| error.within(what()))?;
        if chunk.page_size != page_size {
            return Err(Error::corrupt("the file's pages changed size while a snapshot read it").within(what()));
        }
        more = first.saturating_add(CHUNK) <= chunk.size;
        first += CHUNK;
        send(Part::Pages(chunk.pages))?;
    }
    drop(pages);
    let ready_at = source.stable().map_err(|error| error.within(what()))?.sequence;
    send(Part::End { ready_at })?;
    Ok(at)
}
