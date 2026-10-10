//! Where a content's bytes lie on disk, named by its id in hex: in `uploads/`
//! until its commit, then in a directory of `objects/` for each 4,096 ids.
//!
//! ```text
//! 0x1a3f07 → uploads/1a3f07 → objects/1a3/1a3f07
//! ```

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use super::engine::lock;
use super::ids::Ids;
use crate::{Error, Result};

pub(crate) const UPLOADS: &str = "uploads";
pub(crate) const OBJECTS: &str = "objects";

/// Bits of an id that name its file within its directory: 4,096 a directory.
const IDS_PER_DIR: u32 = 12;

/// Tries of a rename a scanner holds on Windows.
const RENAME_TRIES: u32 = 5;

pub(crate) struct Disk {
    dir: PathBuf,
    /// Whether a file and its directory are synced before a commit names
    /// them: the store's durability is `Full`.
    durable: bool,
    made: Mutex<HashSet<i64>>,
    syncs: Mutex<HashMap<PathBuf, Arc<DirSync>>>,
}

impl Disk {
    pub(crate) fn new(dir: PathBuf, durable: bool) -> Disk {
        Disk { dir, durable, made: Mutex::new(HashSet::new()), syncs: Mutex::new(HashMap::new()) }
    }

    pub(crate) fn durable(&self) -> bool {
        self.durable
    }

    /// The directory of `objects/` an id's file lies in.
    pub(crate) fn fan(id: i64) -> i64 {
        id >> IDS_PER_DIR
    }

    /// The first and the last id of a directory of `objects/`.
    pub(crate) fn span(fan: i64) -> (i64, i64) {
        (fan << IDS_PER_DIR, ((fan + 1) << IDS_PER_DIR) - 1)
    }

    pub(crate) fn upload_path(&self, id: i64) -> PathBuf {
        self.dir.join(UPLOADS).join(format!("{id:x}"))
    }

    pub(crate) fn object_path(&self, id: i64) -> PathBuf {
        self.fan_dir(Disk::fan(id)).join(format!("{id:x}"))
    }

    fn fan_dir(&self, fan: i64) -> PathBuf {
        self.dir.join(OBJECTS).join(format!("{fan:x}"))
    }

    /// Creates an upload's file, which no other id ever names.
    pub(crate) fn create_upload(&self, id: i64) -> Result<std::fs::File> {
        let path = self.upload_path(id);
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|error| Error::io(format!("blobs: create {}", path.display()), error))
    }

    /// Moves an upload's file into `objects/`, where it is never written
    /// again nor named otherwise, and makes its name durable before a commit
    /// names it: its directory made and synced the first time, the rename
    /// retried while a scanner holds the file on Windows, and the directory
    /// synced with the uploads that finish beside it.
    pub(crate) fn place(&self, id: i64) -> Result<()> {
        let fan = Disk::fan(id);
        self.make_fan(fan)?;
        let (from, to) = (self.upload_path(id), self.object_path(id));
        for attempt in 0.. {
            match std::fs::rename(&from, &to) {
                Ok(()) => break,
                Err(error) if held_elsewhere(&error) && attempt + 1 < RENAME_TRIES => {
                    std::thread::sleep(Duration::from_millis(1 << attempt));
                }
                Err(error) => return Err(Error::io(format!("blobs: publish {}", to.display()), error)),
            }
        }
        self.sync_dir(self.fan_dir(fan))
    }

    /// Creates a directory of `objects/` the first time a file goes there,
    /// and syncs `objects/`, so that the directory's name is durable before
    /// any file in it is.
    fn make_fan(&self, fan: i64) -> Result<()> {
        if lock(&self.made).contains(&fan) {
            return Ok(());
        }
        let dir = self.fan_dir(fan);
        match std::fs::create_dir(&dir) {
            Ok(()) => self.sync_dir(self.dir.join(OBJECTS))?,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(Error::io(format!("blobs: create {}", dir.display()), error)),
        }
        lock(&self.made).insert(fan);
        Ok(())
    }

    /// Syncs a directory once for the files that arrive in it together: each
    /// caller waits for a sync that began after it called.
    fn sync_dir(&self, dir: PathBuf) -> Result<()> {
        if !self.durable {
            return Ok(());
        }
        let shared = Arc::clone(lock(&self.syncs).entry(dir.clone()).or_default());
        shared.run(|| sync_directory(&dir)).map_err(|error| Error::io(format!("blobs: sync {}", dir.display()), error))
    }

    /// Removes a content's file, which no file names any more; one already
    /// gone counts as removed.
    pub(crate) fn remove_object(&self, id: i64) -> Result<()> {
        let path = self.object_path(id);
        match std::fs::remove_file(&path) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => {
                Err(Error::io(format!("blobs: remove {}", path.display()), error))
            }
            _ => Ok(()),
        }
    }

    /// Removes an upload's file wherever it was left.
    pub(crate) fn remove_upload(&self, id: i64) -> Result<()> {
        let path = self.upload_path(id);
        match std::fs::remove_file(&path) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => {
                Err(Error::io(format!("blobs: remove {}", path.display()), error))
            }
            _ => Ok(()),
        }
    }

    /// Removes the files of `uploads/` that no upload of this process holds:
    /// at open every one of them, later what an upload failed to remove. Says
    /// how many went.
    pub(crate) fn sweep_uploads(&self, ids: &Ids) -> Result<usize> {
        let mut removed = 0;
        for (name, id) in self.names_in(&self.dir.join(UPLOADS))? {
            if id.is_some_and(|id| ids.holds(id)) {
                continue;
            }
            let path = self.dir.join(UPLOADS).join(&name);
            std::fs::remove_file(&path)
                .map_err(|error| Error::io(format!("blobs: remove {}", path.display()), error))?;
            removed += 1;
        }
        Ok(removed)
    }

    /// The ids of the files a directory of `objects/` holds.
    pub(crate) fn ids_in(&self, fan: i64) -> Result<Vec<i64>> {
        Ok(self.names_in(&self.fan_dir(fan))?.into_iter().filter_map(|(_, id)| id).collect())
    }

    /// The names a directory holds, each with the id it spells when the
    /// engine gave it; none when the directory does not exist.
    fn names_in(&self, dir: &Path) -> Result<Vec<(String, Option<i64>)>> {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(Error::io(format!("blobs: list {}", dir.display()), error)),
        };
        let mut names = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| Error::io(format!("blobs: list {}", dir.display()), error))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let id = i64::from_str_radix(&name, 16).ok().filter(|id| *id > 0 && format!("{id:x}") == name);
            names.push((name, id));
        }
        Ok(names)
    }
}

/// A directory's sync shared by the callers that arrive while one runs: a
/// caller waits for a sync that began after it called, and starts one when
/// none runs.
#[derive(Default)]
struct DirSync {
    state: Mutex<SyncState>,
    done: Condvar,
}

#[derive(Default)]
struct SyncState {
    running: bool,
    begun: u64,
    synced: u64,
    failure: Option<(io::ErrorKind, String)>,
}

impl DirSync {
    fn run(&self, flush: impl Fn() -> io::Result<()>) -> io::Result<()> {
        let mut state = lock(&self.state);
        let wanted = state.begun + 1;
        while state.synced < wanted {
            if state.running {
                state = self.done.wait(state).unwrap_or_else(std::sync::PoisonError::into_inner);
                continue;
            }
            state.running = true;
            state.begun += 1;
            let mine = state.begun;
            drop(state);
            let outcome = flush();
            state = lock(&self.state);
            state.running = false;
            state.synced = mine;
            state.failure = outcome.err().map(|error| (error.kind(), error.to_string()));
            self.done.notify_all();
        }
        match &state.failure {
            Some((kind, what)) => Err(io::Error::new(*kind, what.clone())),
            None => Ok(()),
        }
    }
}

/// Makes a directory's names durable: its own sync.
#[cfg(not(windows))]
fn sync_directory(dir: &Path) -> io::Result<()> {
    std::fs::File::open(dir)?.sync_all()
}

/// Makes a directory's names durable. Windows opens a directory only for
/// backup and flushes it as a file, in about a millisecond.
#[cfg(windows)]
fn sync_directory(dir: &Path) -> io::Result<()> {
    use std::os::windows::fs::OpenOptionsExt;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    std::fs::OpenOptions::new()
        .access_mode(GENERIC_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(dir)?
        .sync_all()
}

/// Whether a rename failed because another program, a scanner or a backup
/// tool, opened the file without sharing it.
#[cfg(windows)]
fn held_elsewhere(error: &io::Error) -> bool {
    const ERROR_SHARING_VIOLATION: i32 = 32;
    error.kind() == io::ErrorKind::PermissionDenied || error.raw_os_error() == Some(ERROR_SHARING_VIOLATION)
}

#[cfg(not(windows))]
fn held_elsewhere(_: &io::Error) -> bool {
    false
}

/// Whether an open failed on a file whose removal has begun: until the handle
/// that removes it closes, Windows refuses to open it rather than saying it
/// is gone.
#[cfg(windows)]
pub(crate) fn being_removed(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::PermissionDenied
}

#[cfg(not(windows))]
pub(crate) fn being_removed(_: &io::Error) -> bool {
    false
}

/// Reads at `offset` without moving the file's own position, from several
/// threads at once.
#[cfg(unix)]
pub(crate) fn read_at(file: &std::fs::File, bytes: &mut [u8], offset: u64) -> io::Result<usize> {
    std::os::unix::fs::FileExt::read_at(file, bytes, offset)
}

#[cfg(windows)]
pub(crate) fn read_at(file: &std::fs::File, bytes: &mut [u8], offset: u64) -> io::Result<usize> {
    std::os::windows::fs::FileExt::seek_read(file, bytes, offset)
}
