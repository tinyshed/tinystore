//! A snapshot: a copy of every engine's files in a directory of the store's
//! own, on the disk the data is on, each copy one moment of its engine. Two
//! engines are two moments, which is safe as no write spans two files. An
//! engine open copies its own while it keeps working; the files of one no
//! handle opened are copied as they lie, an SQLite file through SQLite's own
//! snapshot, so that one opening meanwhile is copied whole all the same.
//!
//! ```text
//! data/.snapshot-1a2b3c/kv.db            kv's copy, by kv
//! data/.snapshot-1a2b3c/sql/app.db       a database no handle opened, from its file
//! data/.snapshot-1a2b3c/blobs/objects/…  a link to each file blobs.db's copy names
//! ```

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::engine::Copied;
use crate::{Error, Result, Store, unix_nanos};

/// What a snapshot's directory starts with, which no engine's file does.
pub(super) const PREFIX: &str = ".snapshot-";

/// The engines' files a store's directory may hold besides those of `sql/`
/// and `blobs/`, by their names in it.
const FILES: [(&str, &str); 5] = [
    ("kv.db", "kv"),
    ("jobs.db", "jobs"),
    ("records.db", "records"),
    ("metrics.db", "metrics"),
    ("search.db", "search"),
];

/// A copy of every engine's files, removed with its directory once dropped.
pub(super) struct Snapshot {
    pub(super) dir: PathBuf,
    pub(super) files: Vec<Copied>,
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.dir) {
            let dir = self.dir.display();
            tracing::warn!(target: "tinystore", %error, %dir, "a backup's snapshot is left in the store's directory");
        }
    }
}

/// Copies every engine's files: those open copy their own, and the files of
/// those that are not are copied as they lie.
pub(super) fn take(store: &Store) -> Result<Snapshot> {
    let (engines, claimed) = store.engines_open()?;
    let mut snapshot = Snapshot { dir: made_dir(store.dir())?, files: Vec::new() };
    for engine in &engines {
        snapshot.files.extend(engine.snapshot(&snapshot.dir)?);
    }
    let copied: HashSet<String> = snapshot.files.iter().map(|file| file.name.clone()).collect();
    let unopened = Unopened { store: store.dir(), into: &snapshot.dir, claimed: &claimed, copied: &copied };
    snapshot.files.extend(unopened.copy()?);
    Ok(snapshot)
}

fn made_dir(store: &Path) -> Result<PathBuf> {
    static MADE: AtomicU64 = AtomicU64::new(0);
    let stamp = unix_nanos(std::time::SystemTime::now());
    let dir = store.join(format!("{PREFIX}{stamp:x}-{:x}", MADE.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&dir).map_err(|error| Error::io(format!("snapshot {}", dir.display()), error))?;
    Ok(dir)
}

/// The engines' files that no engine open holds, copied as they lie: an
/// SQLite file through a connection of its own, and each object of
/// `blobs/objects` linked; uploads a process left are not walked.
struct Unopened<'a> {
    store: &'a Path,
    into: &'a Path,
    /// The names engines open hold, as they claimed them.
    claimed: &'a HashSet<String>,
    /// What the engines open copied already.
    copied: &'a HashSet<String>,
}

impl Unopened<'_> {
    fn copy(&self) -> Result<Vec<Copied>> {
        let mut copied = Vec::new();
        for (name, engine) in FILES {
            if let Some(file) = self.database(name, engine)? {
                copied.push(file);
            }
        }
        for name in self.names_in("sql")? {
            if name.ends_with(".db")
                && let Some(file) = self.database(&format!("sql/{name}"), "sql")?
            {
                copied.push(file);
            }
        }
        if !self.claimed.contains("blobs") && self.store.join("blobs").join("blobs.db").exists() {
            copied.extend(self.blobs()?);
        }
        Ok(copied)
    }

    /// Copies the SQLite file `name` when it lies there unheld and uncopied.
    fn database(&self, name: &str, engine: &'static str) -> Result<Option<Copied>> {
        let source = self.store.join(name);
        if self.claimed.contains(name) || self.copied.contains(name) || !source.is_file() {
            return Ok(None);
        }
        let target = self.target(name)?;
        crate::sqlite::copy(&source, &target).map_err(|error| error.within(format!("snapshot {name}")))?;
        Ok(Some(Copied { name: name.to_owned(), engine }))
    }

    /// Copies `blobs.db` and links every file of `objects/`, which nothing
    /// removes while blobs is not open.
    fn blobs(&self) -> Result<Vec<Copied>> {
        let mut copied: Vec<Copied> = self.database("blobs/blobs.db", "blobs")?.into_iter().collect();
        let mut folders = vec!["blobs/objects".to_owned()];
        while let Some(folder) = folders.pop() {
            for name in self.names_in(&folder)? {
                let path = format!("{folder}/{name}");
                if self.store.join(&path).is_dir() {
                    folders.push(path);
                    continue;
                }
                match link(&self.store.join(&path), &self.target(&path)?) {
                    Ok(()) => copied.push(Copied { name: path, engine: "blobs" }),
                    // removed by a blobs that opened meanwhile, which named it no more
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(Error::io(format!("snapshot {path}"), error)),
                }
            }
        }
        Ok(copied)
    }

    /// The names in a folder of the store's, none when it is not there.
    fn names_in(&self, folder: &str) -> Result<Vec<String>> {
        let failed = |error: io::Error| Error::io(format!("snapshot {folder}"), error);
        let entries = match fs::read_dir(self.store.join(folder)) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(failed(error)),
        };
        let mut names = Vec::new();
        for entry in entries {
            let name = entry.map_err(failed)?.file_name();
            names.push(
                name.into_string()
                    .map_err(|name| Error::invalid(format!("{folder}/{name:?}: a name that is no UTF-8")))?,
            );
        }
        names.sort();
        Ok(names)
    }

    /// Where `name` goes in the snapshot, its folder made.
    fn target(&self, name: &str) -> Result<PathBuf> {
        let target = self.into.join(name);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|error| Error::io(format!("snapshot {name}"), error))?;
        }
        Ok(target)
    }
}

/// A hard link, which copies no byte, or a copy where the system links none.
fn link(source: &Path, target: &Path) -> io::Result<()> {
    fs::hard_link(source, target).or_else(|_| fs::copy(source, target).map(drop))
}
