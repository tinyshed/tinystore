//! A copy of a file another node writes: its home's entries applied in
//! order, each a whole commit, the position applied kept beside them, and
//! each entry kept in the copy's own log for the nodes that copy it.
//!
//! An entry carries whole pages, so applying again an entry the copy holds
//! leaves the file as it was: a position kept late, as a crash may leave it,
//! costs only entries applied twice. A position kept early would claim pages
//! the file lacks, so it is written only after its commit.

use std::fs::{self, OpenOptions};
use std::io::{self, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use sha2::{Digest, Sha256};

use super::entry::{Entry, Kind, Point};
use super::log::Log;
use crate::sqlite::{Applier, Durability};
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
}

/// The entry applied last, and the file that keeps it.
pub(crate) struct Position {
    point: Point,
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
        let position = Position::read(dir.join(Position::FILE)).map_err(|error| error.within(what()))?;
        let log = Log::open(dir, super::log::Bounds::default(), store.node_notify()).map_err(|e| e.within(what()))?;
        let path = store.dir().join(name);
        Ok(Copy {
            name: name.to_owned(),
            applier: Applier::new(&path, Durability::Full),
            path,
            store: store.downgrade(),
            position: Mutex::new(position),
            log,
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
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
        match entry.kind {
            Kind::Commit => self.applier.apply(entry.page_size, entry.size, &entry.pages)?,
            Kind::Begin if entry.sequence == 1 && entry.size <= 1 => {}
            Kind::Begin | Kind::Gap => {
                return Err(Error::new(
                    ErrorKind::Conflict,
                    format!(
                        "copy {}: its home's log starts again at entry {}: the copy starts again from a snapshot",
                        self.name, entry.sequence
                    ),
                ));
            }
        }
        position.keep(entry.point()).map_err(|error| error.within(format!("copy {}", self.name)))?;
        if let Some(bytes) = bytes {
            self.log.keep(entry, bytes).map_err(|error| error.within(format!("copy {}", self.name)))?;
        }
        drop(position);
        if let Some(store) = self.store.upgrade() {
            store.applied(&self.name);
        }
        Ok(())
    }

    fn lock(&self) -> MutexGuard<'_, Position> {
        self.position.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Drop for Copy {
    fn drop(&mut self) {
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
    /// and check, and the first four bytes of their SHA-256.
    pub(crate) const FILE: &str = "applied";
    const BYTES: usize = 8 + 8 + 16 + 4;

    /// Reads the position kept in `path`, or the copy's start when there is
    /// none yet.
    pub(crate) fn read(path: PathBuf) -> Result<Position> {
        let kept = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&path);
        let kept = kept.map_err(|error| io_error(&path, error))?;
        let bytes = fs::read(&path).map_err(|error| io_error(&path, error))?;
        let mut position = Position { point: Point::default(), kept, path };
        if bytes.is_empty() {
            return Ok(position);
        }
        let sum_holds = bytes.len() == Self::BYTES && bytes[32..] == Self::checksum(&bytes[..32]).to_le_bytes();
        if !sum_holds {
            return Err(Self::corrupt("its position does not read: the copy starts again from a snapshot"));
        }
        position.point.sequence = u64::from_le_bytes(bytes[..8].try_into().expect("eight bytes"));
        position.point.epoch = u64::from_le_bytes(bytes[8..16].try_into().expect("eight bytes"));
        position.point.check.copy_from_slice(&bytes[16..32]);
        Ok(position)
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

    pub(crate) fn keep(&mut self, point: Point) -> Result<()> {
        let mut bytes = Vec::with_capacity(Self::BYTES);
        bytes.extend_from_slice(&point.sequence.to_le_bytes());
        bytes.extend_from_slice(&point.epoch.to_le_bytes());
        bytes.extend_from_slice(&point.check);
        bytes.extend_from_slice(&Self::checksum(&bytes).to_le_bytes());
        self.kept
            .seek(SeekFrom::Start(0))
            .and_then(|_| self.kept.write_all(&bytes))
            .map_err(|e| io_error(&self.path, e))?;
        self.point = point;
        Ok(())
    }

    /// Keeps `point` durably: the position a snapshot leaves, which no
    /// entry the copy could apply again stands behind.
    pub(crate) fn keep_durably(&mut self, point: Point) -> Result<()> {
        self.keep(point)?;
        self.kept.sync_data().map_err(|error| io_error(&self.path, error))
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
