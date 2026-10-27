//! A set of files kept in a database's file, inside the database's
//! transaction: a file written commits with the rows the transaction writes,
//! or neither does.
//!
//! The bytes past what stays inline are written to their object before the
//! row that names them, under a content id the transaction takes in its own
//! rows. A rollback gives the id back and removes the object while the
//! writer is still held, so that no later write takes the id while the
//! object lies there; one a crash left lies past what the file reserved, and
//! the next open removes it.

use std::io::Write as _;
use std::sync::Arc;

use sha2::{Digest, Sha256};

use super::change::{Condition, Content, Place};
use super::engine::Blobs;
use super::files::{FileCall, Files, Options};
use super::info::FileInfo;
use super::upload::INLINE;
use crate::transaction::sealed::Sealed;
use crate::{Error, Result, Transaction, TxHandle};

impl Sealed for Files {}

impl TxHandle for Files {
    type InTx<'h> = TxFiles<'h>;

    fn in_tx<'h>(&'h self, tx: &'h Transaction<'h>) -> TxFiles<'h> {
        TxFiles { files: self, tx }
    }
}

/// A set of files' calls inside a transaction of the database it was opened
/// from: what they write commits with the transaction, and what they read
/// sees what it wrote.
pub struct TxFiles<'a> {
    files: &'a Files,
    tx: &'a Transaction<'a>,
}

impl std::fmt::Debug for TxFiles<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TxFiles({:?})", self.files)
    }
}

impl<'a> TxFiles<'a> {
    /// Writes the file and replaces whatever the path held, once the
    /// transaction commits.
    pub fn put(&self, path: &str, bytes: impl AsRef<[u8]>) -> Result<FileInfo> {
        self.key(path).put(bytes)
    }

    /// Writes the file only when the path holds none; says whether it did.
    pub fn create(&self, path: &str, bytes: impl AsRef<[u8]>) -> Result<bool> {
        self.key(path).create(bytes)
    }

    /// Removes the file at the path; nothing to remove is not an error.
    pub fn delete(&self, path: &str) -> Result<()> {
        self.key(path).delete()
    }

    /// What the file at the path carries, as the transaction sees it.
    pub fn head(&self, path: &str) -> Result<Option<FileInfo>> {
        let whole = self.files.whole(path)?;
        let read = |sql: &crate::sqlite::Tx<'_>| self.files.head_on(sql, &whole);
        on_file(self.tx, self.files, read).map_err(|error| self.files.fail(&whole, error))
    }

    /// A call on one path inside the transaction, its options before its
    /// verb: `tx.with(&files).key("a.md").content_type("text/markdown").put(&text)`.
    pub fn key(&self, path: &str) -> FileCall<'a> {
        FileCall::inside(self.files, self.tx, path)
    }
}

/// Runs `work` on the transaction's writer, in a savepoint of its own, once
/// the files are kept in the transaction's file.
pub(super) fn on_file<T>(
    tx: &Transaction<'_>,
    files: &Files,
    work: impl FnOnce(&crate::sqlite::Tx<'_>) -> Result<T>,
) -> Result<T> {
    tx.takes(&files.blobs.file, files.blobs.place())?;
    tx.call(work)
}

/// Writes `bytes` at the whole path inside the transaction when `condition`
/// holds; says what it wrote, nothing when a create found a file. The
/// contents a replace leaves unnamed are maintenance's to remove.
pub(super) fn write_in(
    tx: &Transaction<'_>,
    files: &Files,
    (whole, options): (&str, &Options),
    bytes: &[u8],
    condition: Condition,
) -> Result<Option<FileInfo>> {
    check_size(files, options, bytes.len() as u64)?;
    if bytes.len() > INLINE {
        files.blobs.room(bytes.len() as u64)?;
    }
    let (sha256, size) = (Sha256::digest(bytes).to_vec(), bytes.len() as i64);
    on_file(tx, files, |sql| {
        let id = files.blobs.id_in(sql)?;
        let place = if bytes.len() <= INLINE {
            Place::Inline(bytes.to_vec())
        } else {
            let placed = Placed::write(&files.blobs, id, bytes)?;
            sql.after_commit(Box::new(move || placed.keep()));
            Place::File
        };
        let content = Content { id, size, sha256, place };
        let (_, publication) = files.publication(whole, options, content, condition);
        let (info, _freed) = publication(sql)?;
        Ok(info)
    })
}

/// Refuses bytes past the files' bound, or of another length than the size
/// the call declared.
fn check_size(files: &Files, options: &Options, size: u64) -> Result<()> {
    if let Some(most) = files.max_file_size.filter(|most| size > *most) {
        return Err(Error::limit(format!("a file of {size} bytes, past the {most} the files take at most")));
    }
    match options.size {
        Some(declared) if declared != size => {
            Err(Error::invalid(format!("a body of {size} bytes, where its size says {declared}")))
        }
        _ => Ok(()),
    }
}

/// A file's bytes placed in `objects/` for a transaction that has not
/// committed yet: removed when it is dropped unkept, as a rollback, or the
/// savepoint of a call that failed, drops the hook that keeps it.
struct Placed {
    blobs: Arc<Blobs>,
    id: i64,
    kept: bool,
}

impl Placed {
    /// Writes `bytes` beside `objects/` and renames them into it, each synced
    /// as the store's durability says, so that the row names bytes whose
    /// name is durable.
    fn write(blobs: &Arc<Blobs>, id: i64, bytes: &[u8]) -> Result<Placed> {
        // made first, so that bytes written half way are removed too
        let placed = Placed { blobs: Arc::clone(blobs), id, kept: false };
        let mut file = blobs.disk.create_upload(id)?;
        file.write_all(bytes).map_err(|error| Error::io("blobs: write a file's bytes", error))?;
        if blobs.disk.durable() {
            file.sync_data().map_err(|error| Error::io("blobs: sync a file's bytes", error))?;
        }
        drop(file);
        blobs.disk.place(id)?;
        blobs.disk.sync_placed(id)?;
        Ok(placed)
    }

    fn keep(mut self) {
        self.kept = true;
    }
}

impl Drop for Placed {
    fn drop(&mut self) {
        if self.kept {
            return;
        }
        let _ = self.blobs.disk.remove_upload(self.id);
        if let Err(error) = self.blobs.disk.remove_object(self.id) {
            // maintenance asks the file, which names it not, and removes it then
            tracing::debug!(target: "tinystore", %error, "blobs: a rolled back file's bytes, removed later");
            self.blobs.ids.doubt(self.id);
        }
    }
}

#[cfg(test)]
#[path = "tx_tests.rs"]
mod tests;
