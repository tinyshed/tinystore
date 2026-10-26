//! A snapshot: a copy of every engine's files in a directory of the store's
//! own, on the disk the data is on, all of them one moment of the store, so
//! that a row of one file that names what another holds is copied with what
//! it names. The engines open name their files and keep working; the files
//! of one no handle opened are copied as they lie, at the same moment, and a
//! file blobs keeps is linked once blobs.db is copied.
//!
//! ```text
//! data/.snapshot-1a2b3c/kv.db            kv's, named by kv
//! data/.snapshot-1a2b3c/sql/app.db       a database no handle opened, from its file
//! data/.snapshot-1a2b3c/blobs/objects/…  a link to each file blobs.db's copy names
//! ```

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::engine::{Copied, Copying, Then};
use crate::sqlite::Source;
use crate::{Error, Result, Store, unix_nanos};

/// What a snapshot's directory starts with, which no engine's file does.
pub(super) const PREFIX: &str = ".snapshot-";

/// The engines' files a store's directory may hold besides those of `sql/`
/// and `blobs/`, by their names in it.
pub(super) const FILES: [(&str, &str); 5] = [
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

/// Copies every engine's files at one moment: those open name theirs, and
/// the files of those that are not are found as they lie.
pub(super) fn take(store: &Store) -> Result<Snapshot> {
    let (engines, claimed) = store.engines_open()?;
    let mut snapshot = Snapshot { dir: made_dir(store.dir())?, files: Vec::new() };
    let copying = engines.iter().map(|engine| engine.snapshot()).collect::<Result<Vec<Copying<'_>>>>()?;
    let mut files = Vec::new();
    let mut at_the_moment = Vec::new();
    for engine in copying {
        files.extend(engine.files);
        at_the_moment.extend(engine.at_the_moment);
    }
    let named: HashSet<String> = files.iter().map(|(copied, _)| copied.name.clone()).collect();
    let unopened = Unopened { store: store.dir(), into: &snapshot.dir, claimed: &claimed, named: &named };
    files.extend(unopened.databases()?);
    let mut to_copy = Vec::with_capacity(files.len());
    for (copied, source) in files {
        to_copy.push((source, unopened.target(&copied.name)?));
        snapshot.files.push(copied);
    }
    let then = crate::sqlite::copy_at_one_moment(store.gate(), &to_copy, || {
        at_the_moment.into_iter().map(|at_the_moment| at_the_moment()).collect::<Vec<Then<'_>>>()
    })?;
    for then in then {
        snapshot.files.extend(then(&snapshot.dir)?);
    }
    if !claimed.contains("blobs") && snapshot.files.iter().any(|copied| copied.name == BLOBS) {
        snapshot.files.extend(unopened.objects()?);
    }
    Ok(snapshot)
}

fn made_dir(store: &Path) -> Result<PathBuf> {
    static MADE: AtomicU64 = AtomicU64::new(0);
    let stamp = unix_nanos(std::time::SystemTime::now());
    let dir = store.join(format!("{PREFIX}{stamp:x}-{:x}", MADE.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&dir).map_err(|error| Error::io(format!("snapshot {}", dir.display()), error))?;
    Ok(dir)
}

/// blobs.db, by its path in the store's directory.
const BLOBS: &str = "blobs/blobs.db";

/// The engines' files that no engine open holds, copied as they lie: an
/// SQLite file at the moment beside the others, and once blobs.db is copied,
/// each object of `blobs/objects` linked; uploads a process left are not
/// walked.
struct Unopened<'a> {
    store: &'a Path,
    into: &'a Path,
    /// The names engines open hold, as they claimed them.
    claimed: &'a HashSet<String>,
    /// What the engines open name already.
    named: &'a HashSet<String>,
}

impl Unopened<'_> {
    /// The SQLite files that lie there unheld and unnamed.
    fn databases(&self) -> Result<Vec<(Copied, Source)>> {
        let mut names: Vec<(String, &'static str)> =
            FILES.iter().map(|&(name, engine)| (name.to_owned(), engine)).collect();
        for name in self.names_in("sql")? {
            if name.ends_with(".db") {
                names.push((format!("sql/{name}"), "sql"));
            }
        }
        if !self.claimed.contains("blobs") {
            names.push((BLOBS.to_owned(), "blobs"));
        }
        let unheld = |(name, _): &(String, &'static str)| {
            !self.claimed.contains(name) && !self.named.contains(name) && self.store.join(name).is_file()
        };
        let source = |(name, engine): (String, &'static str)| {
            let path = self.store.join(&name);
            (Copied { name, engine }, Source::unheld(path))
        };
        Ok(names.into_iter().filter(unheld).map(source).collect())
    }

    /// Links every file of `objects/`, which nothing removes while blobs is
    /// not open.
    fn objects(&self) -> Result<Vec<Copied>> {
        let mut copied = Vec::new();
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
