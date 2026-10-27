//! An encrypted bucket's rows: sealed with the store's encryption key on their
//! way into the file, and opened on their way out.
//!
//! A row is sealed for its place, the file's owner, the bucket's name and the
//! row's path, its owners and its key in the bucket: copied to another place,
//! it does not open. A key in the bucket is not sealed, only its value.
//!
//! ```text
//! what is sealed:   0             nothing, as a set's key keeps
//!                   1 | 8 bytes   an integer, big end first
//!                   2 | bytes     bytes
//! ```
//!
//! Each value says which key sealed it, and opens with that key of the
//! store's ring; a write seals with the ring's sealing key. A bucket keeps the
//! ids of the keys its values were sealed with, from its first write until it
//! is cleared whole or sealed again whole. So a key lost is never replaced
//! unseen while any bucket keeps a value it sealed, in kv.db or in a
//! database's file, opened by the program or not:
//!
//! | The bucket's keys | The store's keys               | Opening the bucket    | A value of 3fa9… | A write    |
//! |-------------------|--------------------------------|-----------------------|------------------|------------|
//! | none yet          | none yet                       | makes the store's key |                  | as usual   |
//! | none yet          | lost, another bucket's yet     | `Io`: no key is made  |                  |            |
//! | `3fa9…`           | `3fa9…`                        | as usual              | opens            | as usual   |
//! | `3fa9…`           | lost                           | `Io`: no key is made  |                  |            |
//! | `3fa9…`           | `81c2…`, put there by hand     | as usual              | `Invalid`        | by `81c2…` |
//! | `3fa9…`           | `81c2…`, and `3fa9…` the older | as usual              | opens            | by `81c2…` |
//!
//! `reseal` seals again with the sealing key every value an older key
//! sealed, a batch at a time, so that the older key can go.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::Connection;

use super::cells;
use super::scope::Scope;
use super::value::Raw;
use crate::encryption::Keyring;
use crate::{Error, Result};

const NOTHING: u8 = 0;
const INTEGER: u8 = 1;
const BYTES: u8 = 2;

/// What seals and opens the rows of one encrypted bucket.
#[derive(Clone)]
pub(crate) struct Encrypted {
    key: Arc<Keyring>,
    bucket: i64,
    /// The file's owner and the bucket's name, which every row's place starts
    /// with.
    place: Arc<[u8]>,
}

impl Encrypted {
    /// The seal of the bucket `scope` opened, with the store's keys, which
    /// the store makes only for a bucket that holds no value sealed yet.
    pub(crate) fn of(scope: &Scope) -> Result<Encrypted> {
        let opened = || {
            let kept = scope.kv.file().read(|connection| cells::bucket_key(connection, scope.id))?;
            scope.kv.encryption_key(kept.as_deref())
        };
        let key = opened().map_err(|error| error.within(scope.shown()))?;
        let place = [scope.kv.place().as_bytes(), &[0], scope.name().as_bytes(), &[0]].concat();
        Ok(Encrypted { key, bucket: scope.id, place: place.into() })
    }

    /// Notes among the bucket's keys the one that seals this write, in the
    /// write's own transaction, so that the bucket never holds a value
    /// sealed with a key it does not name.
    pub(crate) fn claim(&self, connection: &Connection) -> Result<()> {
        let mine = self.key.id();
        let kept = cells::bucket_key(connection, self.bucket)?.unwrap_or_default();
        if kept.split(' ').any(|id| id == mine) {
            return Ok(());
        }
        let mut ids: Vec<&str> = kept.split(' ').filter(|id| !id.is_empty()).chain([mine.as_str()]).collect();
        ids.sort_unstable();
        cells::keep_bucket_key(connection, self.bucket, Some(&ids.join(" ")))
    }

    /// Whether the sealing key sealed a value the file keeps, which leaves
    /// it nothing to seal again.
    pub(crate) fn sealed_it(&self, kept: &[u8]) -> bool {
        self.key.sealed_it(kept)
    }

    /// The id of the key that seals.
    pub(crate) fn sealing(&self) -> String {
        self.key.id()
    }

    /// The id of the key that seals the bucket's rows here.
    #[cfg(feature = "replication")]
    pub(crate) fn key_id(&self) -> String {
        self.key.id()
    }

    /// The row as the file keeps it.
    pub(crate) fn seal(&self, path: &[u8], raw: &Raw) -> Result<Raw> {
        let value = match raw {
            Raw::None => vec![NOTHING],
            Raw::Int(int) => [&[INTEGER][..], &int.to_be_bytes()].concat(),
            Raw::Bytes(bytes) => [&[BYTES][..], bytes].concat(),
        };
        self.key.seal(&self.at(path), &value).map(Raw::Bytes)
    }

    /// The row a caller wrote, from what the file keeps of it.
    pub(crate) fn open(&self, path: &[u8], kept: &Raw) -> Result<Raw> {
        let Raw::Bytes(sealed) = kept else {
            return Err(Error::corrupt("its value is not one the store encrypted"));
        };
        let value = self.key.open(&self.at(path), sealed)?;
        match value.split_first() {
            Some((&NOTHING, [])) => Ok(Raw::None),
            Some((&INTEGER, int)) => match <[u8; 8]>::try_from(int) {
                Ok(int) => Ok(Raw::Int(i64::from_be_bytes(int))),
                Err(_) => Err(Error::corrupt("its encrypted value is an integer of another size")),
            },
            Some((&BYTES, bytes)) => Ok(Raw::Bytes(bytes.to_vec())),
            _ => Err(Error::corrupt("its encrypted value is of no kind a row is")),
        }
    }

    /// The place of the row at `path`, which its seal is bound to.
    fn at(&self, path: &[u8]) -> Vec<u8> {
        [&self.place[..], path].concat()
    }
}

/// The keys a bucket keeps the ids of, `3fa9… 81c2…`, as an error names them.
pub(crate) fn named(kept: &str) -> String {
    match kept.split(' ').collect::<Vec<_>>().as_slice() {
        [one] => format!("the key {one}"),
        many => format!("the keys {}", many.join(", ")),
    }
}

/// The id of a key that values of a bucket in the store's directory `dir`
/// are sealed with, if one is: in kv.db or in a database's file, whether a
/// handle opened it or not, since a program opens its buckets in any order.
/// A backup asks its snapshot the same.
pub(crate) fn sealed_in(dir: &Path) -> Result<Option<String>> {
    for file in files_of_buckets(dir)? {
        if let Some(id) = crate::sqlite::read_file(&file, cells::a_bucket_key)? {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

/// The ids of every key that values of a bucket in the store's directory
/// `dir` are sealed with, as [`sealed_in`] finds one: what a backup holding
/// the store's key still leaves out.
#[cfg(feature = "backup")]
pub(crate) fn keys_sealed_in(dir: &Path) -> Result<std::collections::BTreeSet<String>> {
    let mut keys = std::collections::BTreeSet::new();
    for file in files_of_buckets(dir)? {
        keys.extend(crate::sqlite::read_file(&file, cells::bucket_keys)?);
    }
    Ok(keys)
}

/// kv.db and each database's file that `dir` holds: every file a bucket
/// may live in.
fn files_of_buckets(dir: &Path) -> Result<Vec<PathBuf>> {
    let databases = dir.join(crate::engine::DATABASES);
    let failed = |error: io::Error| Error::io(format!("the databases of {}", dir.display()), error);
    let mut files = vec![dir.join(super::engine::FILE)];
    match fs::read_dir(&databases) {
        Ok(entries) => {
            for entry in entries {
                let path = entry.map_err(failed)?.path();
                if path.extension().is_some_and(|extension| extension == "db") {
                    files.push(path);
                }
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(failed(error)),
    }
    files.retain(|file| file.is_file());
    Ok(files)
}

#[cfg(test)]
#[path = "encrypted_tests.rs"]
mod tests;
