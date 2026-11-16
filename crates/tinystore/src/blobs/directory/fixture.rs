//! What directories' tests share: a store without background work on a
//! clock the test moves, a folder of the test's beside it, and a picture of
//! that folder byte for byte, which every call must leave as it was (BLOB-10).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::{Directory, DirectoryBuilder};
use crate::{Options, Store, TestClock};

pub(crate) struct Fixture {
    pub(crate) store: Store,
    pub(crate) clock: Arc<TestClock>,
    // Last: fields drop in order, and Windows removes no file the store still holds.
    pub(crate) dir: tempfile::TempDir,
    pub(crate) folder: tempfile::TempDir,
}

pub(crate) fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let folder = tempfile::tempdir().unwrap();
    let clock = Arc::new(TestClock::new(UNIX_EPOCH + Duration::from_secs(1_800_000_000)));
    let options = Options { clock: Some(clock.clone()), background: false, ..Options::default() };
    let store = Store::open(dir.path(), options).unwrap();
    Fixture { store, clock, dir, folder }
}

/// A time a test gives a file, `seconds` past a fixed moment.
pub(crate) fn at(seconds: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_700_000_000 + seconds)
}

/// What a folder holds, each path's kind, bytes and modification time.
pub(crate) type Picture = BTreeMap<PathBuf, (bool, Vec<u8>, Option<SystemTime>)>;

impl Fixture {
    pub(crate) fn path(&self) -> &Path {
        self.folder.path()
    }

    /// The directory `movies` on the test's folder, opened with `options`.
    pub(crate) fn open(&self, options: impl FnOnce(DirectoryBuilder<'_>) -> DirectoryBuilder<'_>) -> Directory {
        options(self.store.directory("movies").path(self.path())).open().unwrap()
    }

    pub(crate) fn movies(&self) -> Directory {
        self.open(|options| options)
    }

    /// Writes a file at a path of `/` segments, its folders made, its time set.
    pub(crate) fn write(&self, path: &str, bytes: &[u8], modified: SystemTime) {
        let file = self.path().join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, bytes).unwrap();
        let opened = std::fs::OpenOptions::new().write(true).open(&file).unwrap();
        opened.set_modified(modified).unwrap();
    }

    pub(crate) fn remove(&self, path: &str) {
        let at = self.path().join(path);
        if at.is_dir() { std::fs::remove_dir_all(at) } else { std::fs::remove_file(at) }.unwrap();
    }

    /// The folder as it is now, byte for byte.
    pub(crate) fn picture(&self) -> Picture {
        let mut picture = Picture::new();
        look(self.path(), self.path(), &mut picture);
        picture
    }
}

/// A link at `link` to the folder `target`, or false when this process may
/// not make one: Windows asks a developer's mode or a right for it.
pub(crate) fn link_folder(target: &Path, link: &Path) -> bool {
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(target, link);
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_dir(target, link);
    made.is_ok()
}

/// A link at `link` to the file `target`, or false as [`link_folder`] says.
pub(crate) fn link_file(target: &Path, link: &Path) -> bool {
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(target, link);
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_file(target, link);
    made.is_ok()
}

fn look(root: &Path, folder: &Path, picture: &mut Picture) {
    for entry in std::fs::read_dir(folder).unwrap() {
        let path = entry.unwrap().path();
        let metadata = std::fs::symlink_metadata(&path).unwrap();
        let relative = path.strip_prefix(root).unwrap().to_owned();
        if metadata.is_dir() {
            picture.insert(relative, (true, Vec::new(), metadata.modified().ok()));
            look(root, &path, picture);
        } else {
            let bytes = if metadata.is_file() { std::fs::read(&path).unwrap() } else { Vec::new() };
            picture.insert(relative, (false, bytes, metadata.modified().ok()));
        }
    }
}
