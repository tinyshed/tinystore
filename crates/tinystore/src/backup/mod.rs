//! A backup of a whole store while its program runs, as one zip: every
//! engine's files, each copied as one moment of its engine, the files of the
//! program's it is told to take, and a manifest of their sizes and SHA-256;
//! and its restore into an empty directory, before a store opens there,
//! checking every byte. The book is `plan/api/backup.md`.
//!
//! ```text
//! backup.zip
//! ├── kv.db, jobs.db, records.db, metrics.db,    each engine's file
//! │   search.db
//! ├── sql/app.db                                 each database, its buckets and queues with it
//! ├── blobs/blobs.db, blobs/objects/1a3/1a3f07   the files and their bytes, linked rather than copied
//! ├── secret.key, encryption.key                 the program's own files and the key, when asked for
//! └── manifest.json                              its format, its time, each file's size and SHA-256
//! ```
//!
//! ```no_run
//! # fn main() -> tinystore::Result<()> {
//! let store = tinystore::Store::open("data", Default::default())?;
//! let backup = store.backup().include("secret.key").write("backup-2026-10-13.zip")?;
//! assert!(!backup.key_left_out, "a store that seals values needs its key beside the backup");
//! # drop(store);
//! tinystore::backup::restore("backup-2026-10-13.zip", "restored")?;
//! # Ok(())
//! # }
//! ```

mod manifest;
mod restore;
mod snapshot;
mod zip;

use std::fs;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};

pub use restore::restore;

use crate::{Error, Result, Store, unix_millis};
use manifest::{Listed, MANIFEST, Manifest};

/// What a backup holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Backup {
    /// The files, the engines' and those it was told to take.
    pub files: usize,
    /// Their bytes, the zip's own fields aside.
    pub bytes: u64,
    /// The store seals values with an encryption key the backup does not
    /// hold: they open with that key alone, kept apart.
    pub key_left_out: bool,
}

/// A backup about to be made: the files of the program's it takes beside
/// the engines', and whether it takes the store's encryption key.
#[must_use = "a backup is made by write or write_to"]
pub struct BackupBuilder {
    store: Store,
    include: Vec<String>,
    include_encryption_key: bool,
}

impl std::fmt::Debug for BackupBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackupBuilder")
            .field("include", &self.include)
            .field("include_encryption_key", &self.include_encryption_key)
            .finish()
    }
}

impl Store {
    /// A backup of every engine of the store, made while it keeps working.
    pub fn backup(&self) -> BackupBuilder {
        BackupBuilder { store: self.clone(), include: Vec::new(), include_encryption_key: false }
    }
}

impl BackupBuilder {
    /// Includes a file of the program's, by its path inside the store's
    /// directory, such as `secret.key`, which a restore puts back where it
    /// was. A backup holds no file but the engines' unless one is named.
    pub fn include(mut self, name: &str) -> Self {
        self.include.push(name.to_owned());
        self
    }

    /// Includes the store's encryption key, as it is, which a backup leaves
    /// out otherwise: the zip is not encrypted, so a backup with the key
    /// opens every value it holds, and is kept as safely as the key.
    pub fn include_encryption_key(mut self) -> Self {
        self.include_encryption_key = true;
        self
    }

    /// Writes the backup to `path` whole: beside it first, then renamed once
    /// synced, so that a backup that fails leaves no zip.
    pub fn write(self, path: impl AsRef<Path>) -> Result<Backup> {
        let path = path.as_ref();
        let shown = || format!("backup {}", path.display());
        let partial = beside(path);
        let file = fs::File::create_new(&partial).map_err(|error| Error::io(shown(), error))?;
        let written = self.write_to(io::BufWriter::new(&file)).and_then(|backup| {
            file.sync_all().map_err(|error| Error::io(shown(), error))?;
            fs::rename(&partial, path).map_err(|error| Error::io(shown(), error))?;
            Ok(backup)
        });
        if written.is_err() {
            let _ = fs::remove_file(&partial);
        }
        written
    }

    /// Writes the backup to `out` as it is made, for a program that sends it
    /// elsewhere: what `out` holds of a backup that fails is no backup.
    pub fn write_to(self, out: impl Write) -> Result<Backup> {
        let taken = self.taken().map_err(|error| error.within("backup"))?;
        let snapshot = snapshot::take(&self.store).map_err(|error| error.within("backup"))?;
        let mut zip = zip::Writer::new(out);
        let mut manifest = Manifest { created: unix_millis(self.store.clock().now()), ..Manifest::default() };
        for copied in &snapshot.files {
            let listed = add(&mut zip, &snapshot.dir.join(&copied.name), &copied.name, copied.engine)?;
            manifest.files.push(listed);
        }
        for (name, from, engine) in taken.files {
            manifest.files.push(add(&mut zip, &from, &name, engine)?);
        }
        manifest.key_left_out = taken.key_left_out;
        let text = manifest.to_json();
        zip.add(MANIFEST, text.len() as u64, text.as_bytes(), |_| {})
            .map_err(|error| Error::io("backup: its manifest", error))?;
        zip.finish().map_err(|error| Error::io("backup", error))?;
        Ok(manifest.summary())
    }

    /// The files of the program's the backup takes, each checked, with the
    /// key when asked for: their names in the backup and where they are.
    fn taken(&self) -> Result<Taken> {
        let mut files = Vec::new();
        for name in &self.include {
            check_file(name)?;
            if files.iter().any(|(taken, _, _)| taken == name) {
                return Err(Error::invalid(format!("{name}: named twice")));
            }
            files.push((name.clone(), self.store.dir().join(name), "program"));
        }
        let key = encryption_key_file(&self.store).filter(|file| file.exists());
        let key_left_out = key.is_some() && !self.include_encryption_key;
        if let Some(file) = key.filter(|_| self.include_encryption_key) {
            files.push((KEY.to_owned(), file, "key"));
        }
        Ok(Taken { files, key_left_out })
    }
}

/// The files of the program's a backup takes, and whether it leaves out a
/// key that exists.
struct Taken {
    files: Vec<(String, PathBuf, &'static str)>,
    key_left_out: bool,
}

/// The name a backup gives the store's encryption key, and a restore puts
/// it back under, where the store reads it by default.
const KEY: &str = "encryption.key";

/// The file the store's key is in: the one its options name, or
/// `encryption.key` of its directory.
fn encryption_key_file(store: &Store) -> Option<PathBuf> {
    #[cfg(feature = "kv")]
    return Some(store.encryption_key_file().map_or_else(|| store.dir().join(KEY), Path::to_path_buf));
    #[cfg(not(feature = "kv"))]
    {
        let _ = store;
        None
    }
}

/// Writes one file into the zip, summing its bytes as they go by.
fn add(zip: &mut zip::Writer<impl Write>, path: &Path, name: &str, engine: &'static str) -> Result<Listed> {
    let failed = |error: io::Error| Error::io(format!("backup {name}"), error);
    let file = fs::File::open(path).map_err(failed)?;
    let metadata = file.metadata().map_err(failed)?;
    if !metadata.is_file() {
        return Err(Error::invalid(format!("backup {name}: not a file")));
    }
    let mut sha = Sha256::new();
    zip.add(name, metadata.len(), io::BufReader::new(file), |piece| sha.update(piece)).map_err(failed)?;
    Ok(Listed { name: name.to_owned(), engine, size: metadata.len(), sha256: crate::text::hex(&sha.finalize()) })
}

/// Refuses a name of the program's that is not a path inside the store's
/// directory, spelled as one, or that is a file of the store's own.
fn check_file(name: &str) -> Result<()> {
    let refused = |why: &str| Err(Error::invalid(format!("{name:?}: {why}")));
    if !is_inside(name) {
        return refused("not a path inside the store's directory, its parts joined by /");
    }
    if name == KEY {
        return refused("the store's encryption key, which include_encryption_key() takes");
    }
    let first = name.split('/').next().unwrap_or_default();
    let own = |own: &&str| first == *own || first.strip_prefix(own).is_some_and(|rest| rest.starts_with('-'));
    let engines = snapshot::FILES.iter().map(|(file, _)| file);
    if OWN.iter().chain(engines).any(own) || first.starts_with(snapshot::PREFIX) {
        return refused("a file of the store's own, which a backup holds or leaves by itself");
    }
    Ok(())
}

/// The names of the store's directory a program's file is none of, beside
/// the engines' files: `LOCK`, the manifest's, the databases', blobs' and the
/// server's; each with its journals after a `-`.
const OWN: [&str; 5] = ["LOCK", MANIFEST, "sql", "blobs", "server"];

/// Whether `name` is a relative path whose parts are each a name: no `..`,
/// no root, no drive, no `\`.
pub(crate) fn is_inside(name: &str) -> bool {
    let parts_named = name.split('/').all(|part| !part.is_empty() && part != "." && part != "..");
    let plain = Path::new(name).components().all(|component| matches!(component, Component::Normal(_)));
    parts_named && plain && !name.contains('\\') && !name.contains(':')
}

/// Where a backup is written before it is whole: beside its name.
fn beside(path: &Path) -> PathBuf {
    let mut name = path.file_name().map(std::ffi::OsStr::to_os_string).unwrap_or_default();
    name.push(format!(".{}.part", std::process::id()));
    path.with_file_name(name)
}

#[cfg(all(
    test,
    feature = "kv",
    feature = "jobs",
    feature = "sql",
    feature = "blobs",
    feature = "records",
    feature = "metrics",
    feature = "search"
))]
#[path = "backup_tests.rs"]
mod tests;
