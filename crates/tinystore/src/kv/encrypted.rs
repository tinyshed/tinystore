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
//! A bucket keeps the id of the key its values are sealed with, from its
//! first write until it is cleared whole. So a key lost is never replaced
//! unseen, and no value is written under another key among those it could not
//! open:
//!
//! | The bucket's key | The store's key | Opening the bucket    | A read    | A write   |
//! |------------------|-----------------|-----------------------|-----------|-----------|
//! | none yet         | none yet        | makes the store's key | as usual  | as usual  |
//! | `3fa9…`          | `3fa9…`         | as usual              | as usual  | as usual  |
//! | `3fa9…`          | lost            | `Io`: no key is made  |           |           |
//! | `3fa9…`          | `81c2…`         | opens, to be cleared  | `Invalid` | `Invalid` |

use std::sync::Arc;

use rusqlite::Connection;

use super::cells;
use super::scope::Scope;
use super::value::Raw;
use crate::encryption::EncryptionKey;
use crate::{Error, Result};

const NOTHING: u8 = 0;
const INTEGER: u8 = 1;
const BYTES: u8 = 2;

/// What seals and opens the rows of one encrypted bucket.
#[derive(Clone)]
pub(crate) struct Encrypted {
    key: Arc<EncryptionKey>,
    bucket: i64,
    /// The file's owner and the bucket's name, which every row's place starts
    /// with.
    place: Arc<[u8]>,
}

impl Encrypted {
    /// The seal of the bucket `scope` opened, with the store's key: the one
    /// the bucket's values were sealed with, or for a bucket that holds none
    /// the store's own, made when it has none.
    pub(crate) fn of(scope: &Scope) -> Result<Encrypted> {
        let opened = || {
            let kept = scope.kv.file().read(|connection| cells::bucket_key(connection, scope.id))?;
            scope.kv.encryption_key(kept.as_deref())
        };
        let key = opened().map_err(|error| error.within(scope.shown()))?;
        let place = [scope.kv.place().as_bytes(), &[0], scope.name().as_bytes(), &[0]].concat();
        Ok(Encrypted { key, bucket: scope.id, place: place.into() })
    }

    /// Lets a write through when the bucket's values are this key's, and
    /// gives a bucket that holds none this key. Called in the write's own
    /// transaction, so that two keys never seal one bucket.
    pub(crate) fn claim(&self, connection: &Connection) -> Result<()> {
        let mine = self.key.id();
        match cells::bucket_key(connection, self.bucket)? {
            Some(kept) if kept == mine => Ok(()),
            Some(kept) => Err(Error::invalid(format!(
                "its bucket's values are encrypted with the key {kept}, and the store's is {mine}: \
                 open the store with that key, or clear the bucket to start it again with this one"
            ))),
            None => cells::keep_bucket_key(connection, self.bucket, Some(&mine)),
        }
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

#[cfg(test)]
#[path = "encrypted_tests.rs"]
mod tests;
