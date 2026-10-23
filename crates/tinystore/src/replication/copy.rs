//! A copy of a file another node writes: its home's entries applied in
//! order, each a whole commit, the position applied kept beside them, and
//! each entry kept in the copy's own log for the nodes that copy it.
//!
//! A position kept early would claim pages the file lacks, so it is written
//! only after its commit, and so may be late: a process killed between the
//! two, or a write of it a power loss took, leaves the file holding commits
//! its position does not name. The position keeps where the file's WAL ended
//! after the commit it names, and the WAL holds every commit past that until a
//! checkpoint, which runs only once the position is synced. A copy that opens
//! counts the commits its WAL holds past its position: those its own log
//! names, kept before the position, it takes as applied, and the others it
//! names as its home sends them again, writing none of their pages twice. A
//! promotion that cannot name them begins its log again
//! ([`super::promote`]).

use std::collections::VecDeque;
use std::fs::{self, OpenOptions};
use std::io::{self, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use sha2::{Digest, Sha256};

use super::entry::{Entry, Kind, Point};
use super::log::Log;
use crate::sqlite::{Applier, Durability, Observe, Position as WalEnd};
use crate::store::WeakStore;
use crate::{Error, ErrorKind, Result, Store};

/// The file in a copy's log directory that marks its file a copy.
pub(crate) const MARK: &str = "copy";

pub(crate) struct Copy {
    name: String,
    path: PathBuf,
    store: WeakStore,
    applier: Applier,
    position: Mutex<Position>,
    log: Log,
    /// Where each commit the file holds past the position ends in its WAL,
    /// as a crash left them: the next entries, whose pages it holds already.
    unnamed: Mutex<VecDeque<WalEnd>>,
}

/// The entry applied last, where the file's WAL ended after its commit, and
/// the file that keeps them.
pub(crate) struct Position {
    point: Point,
    wal: WalEnd,
    kept: fs::File,
    path: PathBuf,
}

impl Copy {
    /// Makes the store's file `name` a copy of its home's from its first
    /// entry, or opens the copy it is already. A file the store wrote itself
    /// is its own and becomes a copy only from a snapshot of its home.
    #[cfg(test)]
    pub(crate) fn start(store: &Store, name: &str) -> Result<Copy> {
        let what = || format!("copy {name}");
        let dir = store.log_of(name).map_err(|error| error.within(what()))?;
        if !dir.join(MARK).exists() {
            if store.dir().join(name).exists() {
                return Err(Error::invalid(
                    "the store holds its own file of that name: a copy takes its place only from a snapshot of its home",
                )
                .within(what()));
            }
            mark(&dir).map_err(|error| error.within(what()))?;
        }
        Self::open(store, name)
    }

    /// Opens the copy of `name` the store holds, going on from its position.
    pub(crate) fn open(store: &Store, name: &str) -> Result<Copy> {
        let what = || format!("copy {name}");
        let dir = store.log_of(name).map_err(|error| error.within(what()))?;
        let path = store.dir().join(name);
        let mut position = Position::read(dir.join(Position::FILE)).map_err(|error| error.within(what()))?;
        let unnamed = position.settle(&path, &dir).map_err(|error| error.within(what()))?;
        let log = Log::open(dir, store.node_bounds(), store.node_notify()).map_err(|e| e.within(what()))?;
        Ok(Copy {
            name: name.to_owned(),
            applier: Applier::new(&path, Durability::Full),
            path,
            store: store.downgrade(),
            position: Mutex::new(position),
            log,
            unnamed: Mutex::new(unnamed),
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Runs `read` with the entry applied last, no entry applied meanwhile.
    pub(crate) fn holding<T>(&self, read: impl FnOnce(Point) -> Result<T>) -> Result<T> {
        let position = self.lock();
        read(position.point)
    }

    /// The sequence of the entry applied last: the next it takes is the one after.
    pub(crate) fn applied(&self) -> u64 {
        self.lock().point.sequence
    }

    /// The entry applied last. Taken while no entry is being applied, so the
    /// file is that entry's commit.
    pub(crate) fn point(&self) -> Point {
        self.lock().point
    }

    /// Applies the home's next entry, or nothing for one applied already. An
    /// entry that does not follow the last applied, by its sequence and by the
    /// check it names, is refused: entries are missing, or another home wrote it.
    /// `bytes` are the entry as its home wrote it, which the copy's log keeps.
    pub(crate) fn apply(&self, entry: &Entry<'_>, bytes: Option<&[u8]>) -> Result<()> {
        let mut position = self.lock();
        if !position.takes(entry).map_err(|error| error.within(format!("copy {}", self.name)))? {
            return Ok(());
        }
        let wal = match entry.kind {
            Kind::Commit => self.commit(entry, &position)?,
            Kind::Begin if entry.sequence == 1 && entry.size <= 1 => position.wal,
            Kind::Begin | Kind::Gap => {
                return Err(Error::new(
                    ErrorKind::Conflict,
                    format!(
                        "copy {}: its home's log starts again at entry {}: the copy starts again from a snapshot",
                        self.name, entry.sequence
                    ),
                ));
            }
        };
        // the log keeps the entry before the position names it, so that a log ahead names what the file holds
        if let Some(bytes) = bytes {
            self.log.keep(entry, bytes).map_err(|error| error.within(format!("copy {}", self.name)))?;
        }
        position.keep(entry.point(), wal).map_err(|error| error.within(format!("copy {}", self.name)))?;
        if self.applier.is_long() {
            self.sync(&mut position).map_err(|error| error.within(format!("copy {}", self.name)))?;
            self.applier.checkpoint()?;
        }
        drop(position);
        if let Some(store) = self.store.upgrade() {
            store.applied(&self.name);
        }
        Ok(())
    }

    /// Commits `entry`'s pages, or names the next commit the file holds
    /// already, a crash having left the position behind it; where the WAL
    /// ends after it. Those commits are the old home's, of the position's
    /// epoch: an entry of a later epoch while some remain is another home's,
    /// and the copy starts again from a snapshot.
    fn commit(&self, entry: &Entry<'_>, position: &Position) -> Result<WalEnd> {
        let mut unnamed = self.unnamed.lock().unwrap_or_else(PoisonError::into_inner);
        if unnamed.is_empty() {
            return self.applier.apply(entry.page_size, entry.size, &entry.pages);
        }
        if entry.epoch != position.point.epoch {
            return Err(Position::conflict(format!(
                "copy {}: its file holds {} commits of epoch {} past its position, and entry {} is of epoch {}: it \
                 starts again from a snapshot",
                self.name,
                unnamed.len(),
                position.point.epoch,
                entry.sequence,
                entry.epoch
            )));
        }
        Ok(unnamed.pop_front().expect("one is left"))
    }

    /// Makes the log and then the position durable: a position synced first
    /// would, after a power loss, name entries the log lost.
    fn sync(&self, position: &mut Position) -> Result<()> {
        self.log.sync()?;
        position.sync()
    }

    fn lock(&self) -> MutexGuard<'_, Position> {
        self.position.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
#[allow(dead_code, reason = "the copy tests call them, and they build only with every engine")]
impl Copy {
    /// Commits an entry's pages and keeps nothing of it, as a process killed
    /// right after the commit leaves a copy.
    pub(crate) fn commit_and_die(&self, entry: &Entry<'_>) -> Result<()> {
        self.applier.apply(entry.page_size, entry.size, &entry.pages).map(drop)
    }

    pub(crate) fn unnamed(&self) -> u64 {
        self.unnamed.lock().unwrap_or_else(PoisonError::into_inner).len() as u64
    }

    pub(crate) fn log_unsynced(&self) -> bool {
        self.log.unsynced()
    }
}

impl Drop for Copy {
    fn drop(&mut self) {
        // the position is what says how far the file went once its
        // connections close: synced before them
        let _ = self.sync(&mut self.lock());
        self.applier.close();
    }
}

/// Whether the store's file whose log is in `log` is a copy, which its
/// engines open to read.
pub(crate) fn is_copy(log: &Path) -> bool {
    log.join(MARK).exists()
}

/// Whether the copy whose log is in `log` holds a whole file at a position:
/// its file is there, and the position it applied.
pub(crate) fn is_ready(log: &Path, file: &Path) -> bool {
    is_copy(log) && file.exists() && fs::metadata(log.join(Position::FILE)).is_ok_and(|kept| kept.len() > 0)
}

/// Marks the file whose log is in `dir` a copy, before anything of its
/// home's is applied.
pub(crate) fn mark(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir).map_err(|error| io_error(dir, error))?;
    fs::write(dir.join(MARK), b"").map_err(|error| io_error(dir, error))?;
    crate::durable::sync_directory(dir).map_err(|error| io_error(dir, error))
}

impl Position {
    /// Where the copy keeps the entry it applied last: its sequence, epoch
    /// and check, where the file's WAL ended after it, as its checkpoints, two
    /// salts and frame, and the first four bytes of their SHA-256.
    pub(crate) const FILE: &str = "applied";
    const NAMED: usize = 8 + 8 + 16 + 16;
    const BYTES: usize = Self::NAMED + 4;

    /// The entry the position names.
    pub(crate) fn point(&self) -> Point {
        self.point
    }

    /// Reads the position kept in `path`, or the copy's start when there is
    /// none yet.
    pub(crate) fn read(path: PathBuf) -> Result<Position> {
        let kept = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&path);
        let kept = kept.map_err(|error| io_error(&path, error))?;
        let (point, wal) = Self::read_kept(&path)?.unwrap_or_default();
        Ok(Position { point, wal, kept, path })
    }

    /// Finds the commits the file at `file` holds past the position, as a
    /// crash leaves them: those the copy's log in `log` names become the
    /// position, and the others are answered, each with where it ends in the
    /// WAL, for the entries to come.
    pub(crate) fn settle(&mut self, file: &Path, log: &Path) -> Result<VecDeque<WalEnd>> {
        let mut past: VecDeque<WalEnd> = crate::sqlite::commits_past(file, self.wal)?.into();
        if let Some(last) = Log::last_in(log)?.filter(|last| last.sequence > self.point.sequence) {
            let mut commits = 0;
            Log::read(log, |entry| {
                commits += usize::from(entry.sequence > self.point.sequence && entry.kind == Kind::Commit);
                Ok(())
            })?;
            if commits > past.len() {
                return Err(Self::corrupt(
                    "its log names commits its file does not hold: it starts again from a snapshot",
                ));
            }
            let wal = past.drain(..commits).next_back().unwrap_or(self.wal);
            self.keep_durably_at(last, wal)?;
        }
        if !past.is_empty() {
            tracing::warn!(
                target: "tinystore",
                file = %file.display(),
                commits = past.len(),
                "a copy holds commits its position does not name, as a crash left it: it names them as they come again"
            );
        }
        Ok(past)
    }

    /// The point kept in `path`, with nothing opened to write: none for a
    /// file that is not there or holds none yet.
    pub(crate) fn read_point(path: &Path) -> Result<Option<Point>> {
        Ok(Self::read_kept(path)?.map(|(point, _)| point))
    }

    /// The point and the WAL's end kept in `path`.
    fn read_kept(path: &Path) -> Result<Option<(Point, WalEnd)>> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(io_error(path, error)),
        };
        if bytes.is_empty() {
            return Ok(None);
        }
        let named = Self::NAMED;
        let sum_holds = bytes.len() == Self::BYTES && bytes[named..] == Self::checksum(&bytes[..named]).to_le_bytes();
        if !sum_holds {
            return Err(Self::corrupt("its position does not read: the copy starts again from a snapshot"));
        }
        let word = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().expect("four bytes"));
        let point = Point {
            sequence: u64::from_le_bytes(bytes[..8].try_into().expect("eight bytes")),
            epoch: u64::from_le_bytes(bytes[8..16].try_into().expect("eight bytes")),
            check: bytes[16..32].try_into().expect("sixteen bytes"),
        };
        let wal = WalEnd { checkpoints: word(32), salts: [word(36), word(40)], frame: word(44) };
        Ok(Some((point, wal)))
    }

    /// Whether `entry` is the next to apply: `false` for one applied already,
    /// an error for one that does not follow.
    fn takes(&self, entry: &Entry<'_>) -> Result<bool> {
        let point = &self.point;
        if entry.sequence <= point.sequence {
            if entry.sequence == point.sequence && entry.check != point.check {
                return Err(Self::conflict(format!(
                    "entry {} is not the one it applied: another home wrote it",
                    entry.sequence
                )));
            }
            return Ok(false);
        }
        if entry.sequence > point.sequence + 1 {
            return Err(Error::invalid(format!(
                "entry {} follows {}: the entries between are missing",
                entry.sequence, point.sequence
            )));
        }
        if !entry.follows(point) {
            return Err(Self::conflict(format!(
                "entry {} does not follow the one it applied: another home wrote it",
                entry.sequence
            )));
        }
        Ok(true)
    }

    /// Keeps `point`, the file's WAL ending at `wal` after its commit.
    pub(crate) fn keep(&mut self, point: Point, wal: WalEnd) -> Result<()> {
        let mut bytes = Vec::with_capacity(Self::BYTES);
        bytes.extend_from_slice(&point.sequence.to_le_bytes());
        bytes.extend_from_slice(&point.epoch.to_le_bytes());
        bytes.extend_from_slice(&point.check);
        for word in [wal.checkpoints, wal.salts[0], wal.salts[1], wal.frame] {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        bytes.extend_from_slice(&Self::checksum(&bytes).to_le_bytes());
        self.kept
            .seek(SeekFrom::Start(0))
            .and_then(|_| self.kept.write_all(&bytes))
            .map_err(|e| io_error(&self.path, e))?;
        (self.point, self.wal) = (point, wal);
        Ok(())
    }

    /// Makes what was kept durable.
    pub(crate) fn sync(&mut self) -> Result<()> {
        self.kept.sync_data().map_err(|error| io_error(&self.path, error))
    }

    /// Keeps `point` durably, a point of no file's WAL: a promotion's, or the
    /// position a snapshot leaves, whose file has no WAL yet.
    pub(crate) fn keep_durably(&mut self, point: Point) -> Result<()> {
        self.keep_durably_at(point, WalEnd::default())
    }

    /// Keeps `point` durably, the file's WAL ending at `wal`.
    pub(crate) fn keep_durably_at(&mut self, point: Point, wal: WalEnd) -> Result<()> {
        self.keep(point, wal)?;
        self.sync()
    }

    fn checksum(bytes: &[u8]) -> u32 {
        let digest = Sha256::digest(bytes);
        u32::from_le_bytes(digest[..4].try_into().expect("four bytes"))
    }

    fn conflict(what: String) -> Error {
        Error::new(ErrorKind::Conflict, what)
    }

    fn corrupt(what: &str) -> Error {
        Error::new(ErrorKind::Corrupt, what)
    }
}

pub(crate) fn io_error(path: &Path, error: io::Error) -> Error {
    Error::io(path.display().to_string(), error)
}

/// Removes the file at `path`; one that is not there is removed already.
pub(crate) fn remove_if_there(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error(path, error)),
    }
}
