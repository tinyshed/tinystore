//! A directory: a folder of this machine under a name, which the store lists
//! and reads and never writes. A scan walks the folder and writes down in
//! blobs.db what appeared, changed or went since the last one; a read opens
//! the file on the disk, a segment at a time, following no link.
//!
//! ```no_run
//! # fn main() -> tinystore::Result<()> {
//! let store = tinystore::Store::open("data", Default::default())?;
//! let movies = store.directory("movies").path("D:/Movies").open()?;
//! let scan = movies.scan()?; // what the folder holds now, beside what the last scan saw
//! let page = movies.changes().page()?; // every file, `added`, from the first scan on
//! let film = movies.get("Dune (2021)/Dune.mkv")?; // read from the disk, as a stored file is
//! # Ok(())
//! # }
//! ```

mod changes;
mod ignore;
mod listed;
mod names;
mod opener;
mod outside;
mod walk;

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::OptionalExtension;

pub use changes::{Change, Changes, ChangesCall, FileChange};
pub use listed::DirectoryList;
pub(crate) use outside::Outside;
pub use walk::Scan;

use self::ignore::Ignore;
use super::engine::Blobs;
use super::info::{FileInfo, OCTETS};
use super::paths::Folder;
use super::read::StoredFile;
use crate::engine::Host;
use crate::sqlite::{Tx, own_work, sql_error};
use crate::sync::lock;
use crate::{Error, ErrorKind, Key, Result, Store};

const SELECT_DIRECTORY: &str = "select id from _tinystore_blobs_directories where name = ?1";
const INSERT_DIRECTORY: &str =
    "insert into _tinystore_blobs_directories (name) values (?1) on conflict (name) do nothing";
const SELECT_SET: &str = "select 1 from _tinystore_blobs_sets where name = ?1";

impl Store {
    /// The directory `name`: a folder of this machine, which the store lists
    /// and reads and never writes. Its name is a set of files' too, and the
    /// two never share one.
    pub fn directory(&self, name: &str) -> DirectoryBuilder<'_> {
        DirectoryBuilder {
            store: self,
            name: name.to_owned(),
            path: None,
            every: None,
            ignore: Vec::new(),
            growing: false,
        }
    }
}

/// What opens a directory: its folder on this machine, how often the store
/// scans it, the names it passes over, and whether its files grow as they
/// are read.
#[derive(Debug)]
#[must_use = "a directory opens at `open`"]
pub struct DirectoryBuilder<'s> {
    store: &'s Store,
    name: String,
    path: Option<PathBuf>,
    every: Option<Duration>,
    ignore: Vec<String>,
    growing: bool,
}

impl DirectoryBuilder<'_> {
    /// The folder on this machine. It is not kept in the store: a directory
    /// opened without one lists what its scans wrote and reads nothing.
    pub fn path(mut self, path: impl AsRef<Path>) -> Self {
        self.path = Some(path.as_ref().to_owned());
        self
    }

    /// Scans the folder on the store's background work, the first a period
    /// after the open; a period is minutes, not seconds.
    pub fn every(mut self, period: Duration) -> Self {
        self.every = Some(period);
        self
    }

    /// Names a scan passes over, a file's or a folder's, `*` for any run of
    /// characters within one name: a folder passed over is not entered.
    pub fn ignore<I, S>(mut self, patterns: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.ignore = patterns.into_iter().map(Into::into).collect();
        self
    }

    /// For a folder whose files are written while they are read: a read holds
    /// to no version, and goes up to the size the file has as it reads.
    pub fn growing(mut self, growing: bool) -> Self {
        self.growing = growing;
        self
    }

    pub fn open(self) -> Result<Directory> {
        let name = self.name.clone();
        self.opened().map_err(|error| error.within(format!("directory {name}")))
    }

    fn opened(self) -> Result<Directory> {
        crate::text::check_name("a directory's", &self.name)?;
        let ignore = Ignore::new(self.ignore)?;
        let root = self.path.as_deref().map(Root::of).transpose()?;
        let blobs = Blobs::of(self.store)?;
        let id = blobs.directory_id(&self.name)?;
        let logged = walk::Logged::default();
        let opened = Arc::new(Opened { name: self.name, id, root, ignore, growing: self.growing, logged });
        if let Some(period) = self.every {
            scan_every(self.store, &blobs, &opened, period)?;
        }
        Ok(Directory { blobs, opened, folder: Folder::default(), refused: None })
    }
}

/// Scans the directory every `period` on the store's background work, while
/// a handle of it is open; a copy of blobs.db scans nothing, its home does.
fn scan_every(store: &Store, blobs: &Arc<Blobs>, opened: &Arc<Opened>, period: Duration) -> Result<()> {
    let (blobs, opened): (Weak<Blobs>, Weak<Opened>) = (Arc::downgrade(blobs), Arc::downgrade(opened));
    let name = format!("blobs: directory {}: scan", opened.upgrade().map_or_else(String::new, |o| o.name.clone()));
    store
        .every(&name, period, move || {
            let (Some(blobs), Some(opened)) = (blobs.upgrade(), opened.upgrade()) else { return Ok(()) };
            if blobs.file.is_copy() {
                return Ok(());
            }
            Directory { blobs, opened, folder: Folder::default(), refused: None }.scan().map(drop)
        })
        .map(drop)
}

/// A directory's folder on this machine: its path, and the file system it is
/// on, which a scan does not leave.
#[derive(Debug)]
pub(crate) struct Root {
    path: PathBuf,
    #[cfg(unix)]
    device: u64,
}

impl Root {
    /// The folder at `path`, refused when it is not one or cannot be read.
    fn of(path: &Path) -> Result<Root> {
        let refused = |why: &str, error: std::io::Error| {
            Error::new(ErrorKind::Invalid, format!("{}: {why}: {error}", path.display()))
        };
        let metadata = std::fs::metadata(path).map_err(|error| refused("not a folder this process can read", error))?;
        if !metadata.is_dir() {
            return Err(Error::invalid(format!("{}: not a folder", path.display())));
        }
        drop(std::fs::read_dir(path).map_err(|error| refused("a folder this process cannot read", error))?);
        #[cfg(unix)]
        let device = std::os::unix::fs::MetadataExt::dev(&metadata);
        Ok(Root {
            path: path.to_owned(),
            #[cfg(unix)]
            device,
        })
    }
}

/// What every handle of one open of a directory shares.
#[derive(Debug)]
pub(crate) struct Opened {
    name: String,
    /// Its row in blobs.db.
    id: i64,
    /// None where the program gave no folder: it lists and reads nothing.
    root: Option<Root>,
    ignore: Ignore,
    growing: bool,
    /// The kinds of what scans passed over that were logged once.
    logged: walk::Logged,
}

/// A directory, or a folder of it: what its scans listed, and its files read
/// from the disk.
#[derive(Clone)]
pub struct Directory {
    blobs: Arc<Blobs>,
    opened: Arc<Opened>,
    folder: Folder,
    /// A folder that is not one, refused by the first call that uses it.
    refused: Option<Arc<Error>>,
}

impl fmt::Debug for Directory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Directory({}, {:?})", self.opened.name, self.folder.prefix())
    }
}

impl Directory {
    /// The folder in this one that `segment` names, whose paths are relative
    /// to it.
    pub fn folder(&self, segment: impl Key) -> Directory {
        let mut folder = self.clone();
        if folder.refused.is_none() {
            match self.folder.folder(&segment.text()) {
                Ok(inner) => folder.folder = inner,
                Err(error) => folder.refused = Some(Arc::new(self.fail(self.folder.prefix(), error))),
            }
        }
        folder
    }

    /// Walks the folder and writes down what appeared, changed or went since
    /// the last scan. One scan of a directory runs at a time: a second waits
    /// for the first and answers as it does.
    pub fn scan(&self) -> Result<Scan> {
        self.whole_directory("a scan")?;
        self.blobs.scans.once(self.opened.id, || walk::scan(&self.blobs, &self.opened))
    }

    /// The file at the path as a scan listed it and as the disk holds it now:
    /// none when no scan listed it, or when it is gone since.
    pub fn get(&self, path: &str) -> Result<Option<StoredFile>> {
        let whole = self.whole(path)?;
        self.open_file(&whole).map_err(|error| self.fail(&whole, error))
    }

    /// What the last scan wrote down of the file at the path; it reads no disk.
    pub fn head(&self, path: &str) -> Result<Option<FileInfo>> {
        let whole = self.whole(path)?;
        let row = listed::row(&self.blobs, self.opened.id, &whole).map_err(|error| self.fail(&whole, error))?;
        Ok(row.map(|row| row.info(&self.folder)))
    }

    /// Removes every file of the list, each told `removed`, and nothing on
    /// the disk: for a folder that was truly emptied, which a scan refuses to
    /// believe.
    pub fn forget(&self) -> Result<()> {
        self.whole_directory("a forget")?;
        self.refuse_on_a_copy("a forget")?;
        changes::forget(&self.blobs, self.opened.id).map_err(|error| self.fail("", error))
    }

    fn open_file(&self, whole: &str) -> Result<Option<StoredFile>> {
        let Some(row) = listed::row(&self.blobs, self.opened.id, whole)? else { return Ok(None) };
        let Some(root) = &self.opened.root else {
            return Err(Error::new(ErrorKind::Unavailable, "no folder of this machine was given it"));
        };
        // an awaited get opens a file on the store's pool, never where its future is polled
        if crate::polled::waits_refused() {
            return Err(crate::polled::refused(|| format!("{}: {whole:?}: open its file", self.describe())));
        }
        let copy = self.blobs.file.is_copy();
        if copy && self.opened.growing {
            return Err(Error::new(ErrorKind::Unavailable, "a growing directory is read on its home alone"));
        }
        let Some(file) = opener::open_beneath(&root.path, whole).map_err(|error| Error::io("open a file", error))?
        else {
            return Ok(None);
        };
        let outside = Outside::new(file, self.opened.growing).map_err(|error| Error::io("open a file", error))?;
        if copy && (outside.size(), outside.modified()) != (row.size, row.modified) {
            let why = "the file differs from its home's list: read it on the home";
            return Err(Error::new(ErrorKind::Unavailable, why));
        }
        let info = version_info(self.folder.relative(whole), outside.size(), outside.modified());
        self.blobs.probe.count("tinystore_blobs_read_total", &[], 1.0);
        self.blobs.probe.count("tinystore_blobs_read_bytes_total", &[], info.size as f64);
        Ok(Some(StoredFile::outside(info, outside, format!("{}: {whole:?}", self.describe()))))
    }

    fn whole(&self, path: &str) -> Result<String> {
        self.usable()?;
        self.folder.path(path).map_err(|error| self.fail(&format!("{}{path}", self.folder.prefix()), error))
    }

    fn usable(&self) -> Result<()> {
        self.refused.as_ref().map_or(Ok(()), |refused| Err(refused.duplicate()))
    }

    /// Refuses a call that is the whole directory's on a folder of it.
    fn whole_directory(&self, call: &str) -> Result<()> {
        self.usable()?;
        if !self.folder.prefix().is_empty() {
            let refused = Error::invalid(format!("{call} is the whole directory's, not a folder's"));
            return Err(self.fail(self.folder.prefix(), refused));
        }
        Ok(())
    }

    /// Refuses a call that writes the list on a copy of blobs.db.
    fn refuse_on_a_copy(&self, call: &str) -> Result<()> {
        if self.blobs.file.is_copy() {
            let why = format!("{call} writes the list, so it runs on the home of blobs.db");
            return Err(Error::new(ErrorKind::Unavailable, why).within(self.describe()));
        }
        Ok(())
    }

    /// An error said of the directory and the path it is about:
    /// `directory movies: "Dune (2021)/Dune.mkv": io`.
    pub(crate) fn fail(&self, path: &str, error: Error) -> Error {
        let error = if path.is_empty() { error } else { error.within(format!("{path:?}")) };
        error.within(self.describe())
    }

    fn describe(&self) -> String {
        format!("directory {}", self.opened.name)
    }
}

impl Blobs {
    /// The id of the directory `name`, made the first time it opens; a name a
    /// set of files has is refused.
    fn directory_id(&self, name: &str) -> Result<i64> {
        let owned = name.to_owned();
        // found before it is made, so that a copy, which writes nothing, opens one its home made
        own_work(|| {
            self.file.write(0, move |tx| {
                let failed = |error| sql_error("blobs: a directory", error);
                let found = tx.query_row(SELECT_DIRECTORY, [&owned], |row| row.get(0)).optional().map_err(failed)?;
                if let Some(id) = found {
                    return Ok(id);
                }
                if tx.query_row(SELECT_SET, [&owned], |_| Ok(())).optional().map_err(failed)?.is_some() {
                    return Err(Error::invalid(format!("{owned:?} names a set of files of this store")));
                }
                tx.execute(INSERT_DIRECTORY, [&owned]).map_err(failed)?;
                tx.query_row(SELECT_DIRECTORY, [&owned], |row| row.get(0)).map_err(failed)
            })
        })
    }
}

/// Refuses, for a connection that may only read, the directory `name` while
/// the store does not hold it: neither blobs.db nor the directory is made.
#[cfg(feature = "wire")]
pub(crate) fn directory_as_it_is(store: &Store, name: &str) -> Result<()> {
    let within = |error: Error| error.within(format!("directory {name}"));
    crate::text::check_name("a directory's", name).map_err(within)?;
    store.holds("blobs/blobs.db").map_err(within)?;
    let blobs = Blobs::of(store).map_err(within)?;
    let found = blobs.file.read(|connection| {
        connection
            .prepare_cached(SELECT_DIRECTORY)
            .and_then(|mut select| select.query_row([name], |_| Ok(())).optional())
            .map_err(|error| sql_error("blobs: a directory", error))
    });
    found.map_err(within)?.ok_or_else(|| within(crate::engine::not_held("directory")))
}

/// Refuses, in the write that makes a set of files, a name a directory has.
pub(crate) fn refuse_a_directory(tx: &Tx<'_>, name: &str) -> Result<()> {
    let found = tx
        .query_row(SELECT_DIRECTORY, [name], |_| Ok(()))
        .optional()
        .map_err(|error| sql_error("blobs: a directory of the name", error))?;
    match found {
        Some(()) => Err(Error::invalid(format!("{name:?} names a directory of this store"))),
        None => Ok(()),
    }
}

/// The scans under way, a directory at a time: a second scan of one waits
/// for the first and answers as it does.
#[derive(Debug, Default)]
pub(crate) struct Scans(Mutex<HashMap<i64, Arc<Running>>>);

#[derive(Debug, Default)]
struct Running {
    answer: Mutex<Option<std::result::Result<Scan, Error>>>,
    ended: Condvar,
}

impl Scans {
    fn once(&self, directory: i64, scan: impl FnOnce() -> Result<Scan>) -> Result<Scan> {
        let (running, leads) = {
            let mut scans = lock(&self.0);
            match scans.get(&directory) {
                Some(running) => (Arc::clone(running), false),
                None => {
                    let running = Arc::new(Running::default());
                    scans.insert(directory, Arc::clone(&running));
                    (running, true)
                }
            }
        };
        if leads {
            // the entry goes however the scan ends, a panic among the ways
            let _leaving = Leaving { scans: self, directory };
            let answer = scan();
            *lock(&running.answer) = Some(answer.as_ref().copied().map_err(Error::duplicate));
            running.ended.notify_all();
            return answer;
        }
        let mut answer = lock(&running.answer);
        loop {
            if let Some(answer) = answer.as_ref() {
                return answer.as_ref().copied().map_err(Error::duplicate);
            }
            answer = running.ended.wait(answer).unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }
}

/// Takes a scan that ended out of those under way.
struct Leaving<'s> {
    scans: &'s Scans,
    directory: i64,
}

impl Drop for Leaving<'_> {
    fn drop(&mut self) {
        let running = lock(&self.scans.0).remove(&self.directory);
        if let Some(running) = running {
            let mut answer = lock(&running.answer);
            answer.get_or_insert_with(|| Err(Error::internal("a scan that panicked")));
            drop(answer);
            running.ended.notify_all();
        }
    }
}

/// What a file of a directory carries: its size and modification time, an
/// ETag of the two, and the type and meta of a file nobody wrote through the
/// store.
fn version_info(path: &str, size: u64, modified: i64) -> FileInfo {
    FileInfo {
        path: path.to_owned(),
        size,
        etag: version_etag(size, modified),
        content_type: OCTETS.to_owned(),
        last_modified: time_of(modified),
        expires: None,
        meta: std::collections::BTreeMap::new(),
    }
}

/// An ETag that names a version, not the bytes: `"d-<size>-<time>"`, each in
/// hex, the time in nanoseconds since 1970 as its 64 bits.
///
/// ```text
/// 134,217,728 bytes, modified 1,700,000,000.5 s → "d-8000000-17979cfe53f76500"
/// ```
pub(crate) fn version_etag(size: u64, modified: i64) -> String {
    format!("\"d-{size:x}-{:x}\"", modified.cast_unsigned())
}

/// A modification time as the list keeps it: nanoseconds since 1970, before
/// it below zero, past 2262 its last.
pub(crate) fn nanos_of(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(after) => i64::try_from(after.as_nanos()).unwrap_or(i64::MAX),
        Err(before) => i64::try_from(before.duration().as_nanos()).map_or(i64::MIN, |nanos| -nanos),
    }
}

pub(crate) fn time_of(nanos: i64) -> SystemTime {
    let since = Duration::from_nanos(nanos.unsigned_abs());
    if nanos >= 0 { UNIX_EPOCH + since } else { UNIX_EPOCH - since }
}

#[cfg(test)]
pub(crate) mod fixture;

#[cfg(test)]
#[path = "directory_tests.rs"]
mod tests;
