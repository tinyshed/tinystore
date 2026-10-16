//! A copy of a file another node writes: its home's entries applied in
//! order, each a whole commit, and the position applied kept beside them.
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

use super::entry::{Entry, Kind};
use crate::sqlite::{Applier, Durability};
use crate::store::WeakStore;
use crate::{Error, ErrorKind, Result, Store};

/// The file in a copy's log directory that marks its file a copy.
const MARK: &str = "copy";

#[cfg_attr(not(test), expect(dead_code, reason = "a node's links drive its copies, phase 7's fourth slice"))]
pub(crate) struct Copy {
    name: String,
    store: WeakStore,
    applier: Applier,
    position: Mutex<Position>,
}

/// The entry applied last, and the file that keeps it.
#[cfg_attr(not(test), expect(dead_code, reason = "a node's links drive its copies, phase 7's fourth slice"))]
struct Position {
    sequence: u64,
    epoch: u64,
    check: [u8; 16],
    kept: fs::File,
    path: PathBuf,
}

#[cfg_attr(not(test), expect(dead_code, reason = "a node's links drive its copies, phase 7's fourth slice"))]
impl Copy {
    /// Makes the store's file `name` a copy of its home's, or opens the copy
    /// it is already. A file the store wrote itself is its own and becomes a
    /// copy only from a snapshot of its home.
    pub(crate) fn start(store: &Store, name: &str) -> Result<Copy> {
        let what = || format!("copy {name}");
        let dir = store.log_of(name).map_err(|error| error.within(what()))?;
        if !dir.join(MARK).exists() {
            Self::mark(store, name, &dir).map_err(|error| error.within(what()))?;
        }
        let position = Position::read(dir.join(Position::FILE)).map_err(|error| error.within(what()))?;
        Ok(Copy {
            name: name.to_owned(),
            store: store.downgrade(),
            applier: Applier::new(store.dir().join(name), Durability::Full),
            position: Mutex::new(position),
        })
    }

    /// The sequence of the entry applied last: the next it takes is the one after.
    pub(crate) fn applied(&self) -> u64 {
        self.lock().sequence
    }

    /// Applies the home's next entry, or nothing for one applied already. An
    /// entry that does not follow the last applied, by its sequence and by the
    /// check it names, is refused: entries are missing, or another home wrote it.
    pub(crate) fn apply(&self, entry: &Entry<'_>) -> Result<()> {
        let mut position = self.lock();
        if !position.takes(entry).map_err(|error| error.within(format!("copy {}", self.name)))? {
            return Ok(());
        }
        match entry.kind {
            Kind::Commit => self.applier.apply(entry.page_size, entry.size, &entry.pages)?,
            Kind::Begin if entry.sequence == 1 && entry.size <= 1 => {}
            Kind::Begin | Kind::Gap => {
                return Err(Error::invalid(format!(
                    "copy {}: its home's log starts again at entry {}: the copy starts again from a snapshot",
                    self.name, entry.sequence
                )));
            }
        }
        position.keep(entry).map_err(|error| error.within(format!("copy {}", self.name)))?;
        drop(position);
        if let Some(store) = self.store.upgrade() {
            store.applied(&self.name);
        }
        Ok(())
    }

    fn lock(&self) -> MutexGuard<'_, Position> {
        self.position.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Marks `name` a copy in its log's directory, before anything of its
    /// home's is applied.
    fn mark(store: &Store, name: &str, dir: &Path) -> Result<()> {
        if store.dir().join(name).exists() {
            return Err(Error::invalid(
                "the store holds its own file of that name: a copy takes its place only from a snapshot of its home",
            ));
        }
        fs::create_dir_all(dir).map_err(|error| io_error(dir, error))?;
        fs::write(dir.join(MARK), b"").map_err(|error| io_error(dir, error))?;
        crate::durable::sync_directory(dir).map_err(|error| io_error(dir, error))
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

#[cfg_attr(not(test), expect(dead_code, reason = "a node's links drive its copies, phase 7's fourth slice"))]
impl Position {
    /// Where the copy keeps the entry it applied last: its sequence, epoch
    /// and check, and the first four bytes of their SHA-256.
    const FILE: &str = "applied";
    const BYTES: usize = 8 + 8 + 16 + 4;

    /// Reads the position kept in `path`, or the copy's start when there is
    /// none yet.
    fn read(path: PathBuf) -> Result<Position> {
        let kept = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&path);
        let kept = kept.map_err(|error| io_error(&path, error))?;
        let bytes = fs::read(&path).map_err(|error| io_error(&path, error))?;
        let mut position = Position { sequence: 0, epoch: 0, check: [0; 16], kept, path };
        if bytes.is_empty() {
            return Ok(position);
        }
        let sum_holds = bytes.len() == Self::BYTES && bytes[32..] == Self::checksum(&bytes[..32]).to_le_bytes();
        if !sum_holds {
            return Err(Self::corrupt("its position does not read: the copy starts again from a snapshot"));
        }
        position.sequence = u64::from_le_bytes(bytes[..8].try_into().expect("eight bytes"));
        position.epoch = u64::from_le_bytes(bytes[8..16].try_into().expect("eight bytes"));
        position.check.copy_from_slice(&bytes[16..32]);
        Ok(position)
    }

    /// Whether `entry` is the next to apply: `false` for one applied already,
    /// an error for one that does not follow.
    fn takes(&self, entry: &Entry<'_>) -> Result<bool> {
        if entry.sequence <= self.sequence {
            if entry.sequence == self.sequence && entry.check != self.check {
                return Err(Self::conflict(format!(
                    "entry {} is not the one it applied: another home wrote it",
                    entry.sequence
                )));
            }
            return Ok(false);
        }
        if entry.sequence > self.sequence + 1 {
            return Err(Error::invalid(format!(
                "entry {} follows {}: the entries between are missing",
                entry.sequence, self.sequence
            )));
        }
        if entry.previous != self.check || entry.epoch < self.epoch {
            return Err(Self::conflict(format!(
                "entry {} does not follow the one it applied: another home wrote it",
                entry.sequence
            )));
        }
        Ok(true)
    }

    fn keep(&mut self, entry: &Entry<'_>) -> Result<()> {
        let mut bytes = Vec::with_capacity(Self::BYTES);
        bytes.extend_from_slice(&entry.sequence.to_le_bytes());
        bytes.extend_from_slice(&entry.epoch.to_le_bytes());
        bytes.extend_from_slice(&entry.check);
        bytes.extend_from_slice(&Self::checksum(&bytes).to_le_bytes());
        self.kept
            .seek(SeekFrom::Start(0))
            .and_then(|_| self.kept.write_all(&bytes))
            .map_err(|e| io_error(&self.path, e))?;
        (self.sequence, self.epoch, self.check) = (entry.sequence, entry.epoch, entry.check);
        Ok(())
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

#[cfg_attr(not(test), expect(dead_code, reason = "a node's links drive its copies, phase 7's fourth slice"))]
fn io_error(path: &Path, error: io::Error) -> Error {
    Error::io(path.display().to_string(), error)
}
