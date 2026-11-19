//! A store in a directory of its own under the system's temporary one,
//! which its close removes: the lines every test of a program writes once.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::{Error, Options, Result, Store};

/// How long a removal tries again while a file stays held: Windows lets go
/// of a closed file a moment late, and so may a scanner reading a new one.
const LET_GO: Duration = Duration::from_secs(1);

/// Opens a store in a new, empty directory under the system's temporary
/// one, named `tinystore-` and a part of its own, with `options`: a test's
/// clock among them, `Options { clock: Some(clock.clone()), .. }`. The store
/// closes and its directory goes when the [`TempStore`] drops, or at its
/// [`TempStore::close`] for a test that wants the error (contract STORE-12).
pub fn open_temp(options: Options) -> Result<TempStore> {
    let dir = made_dir()?;
    match Store::open(&dir, options) {
        Ok(store) => Ok(TempStore { store, dir, closed: false }),
        Err(error) => {
            let _ = removed(&dir);
            Err(error)
        }
    }
}

/// A store [`open_temp`] opened. It derefs to [`Store`].
#[derive(Debug)]
pub struct TempStore {
    store: Store,
    dir: PathBuf,
    closed: bool,
}

impl TempStore {
    /// The store's directory, for a test that looks at its files.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Closes the store, then removes its directory. A file still held a
    /// second after the close is a handle the test leaked, and fails `Io`
    /// rather than leave the directory behind unsaid.
    pub fn close(mut self) -> Result<()> {
        self.closed = true;
        self.store.close()?;
        removed(&self.dir)
    }
}

impl std::ops::Deref for TempStore {
    type Target = Store;

    fn deref(&self) -> &Store {
        &self.store
    }
}

impl Drop for TempStore {
    /// Closes and removes as `close` does, its failure left unsaid: a drop
    /// that panics while a failing test unwinds aborts the process.
    fn drop(&mut self) {
        if !self.closed && self.store.close().is_ok() {
            let _ = removed(&self.dir);
        }
    }
}

/// A new directory of the process's own: its id, the time and a count make a
/// name no other open takes, and `create_dir` refuses one that is there.
fn made_dir() -> Result<PathBuf> {
    static MADE: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    loop {
        let count = MADE.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("tinystore-{}-{nanos:x}-{count}", std::process::id()));
        match std::fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(Error::io(format!("open_temp: {}", dir.display()), error)),
        }
    }
}

fn removed(dir: &Path) -> Result<()> {
    let deadline = Instant::now() + LET_GO;
    loop {
        match std::fs::remove_dir_all(dir) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) if Instant::now() > deadline => {
                let what = format!("open_temp: {}: a file stays held after the store closed", dir.display());
                return Err(Error::io(what, error));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // contract: STORE-12
    #[test]
    fn a_temporary_store_lives_in_a_directory_of_its_own_which_its_close_or_drop_removes() {
        let first = open_temp(Options { background: false, ..Options::default() }).unwrap();
        let second = open_temp(Options { background: false, ..Options::default() }).unwrap();
        assert_ne!(first.dir(), second.dir(), "a directory each");
        assert!(first.dir().file_name().unwrap().to_string_lossy().starts_with("tinystore-"));
        assert!(first.dir().join("LOCK").exists(), "the store opened in it");
        let (closed, dropped) = (first.dir().to_path_buf(), second.dir().to_path_buf());
        first.close().unwrap();
        assert!(!closed.exists(), "close removes it");
        drop(second);
        assert!(!dropped.exists(), "and so does a drop");
    }
}
