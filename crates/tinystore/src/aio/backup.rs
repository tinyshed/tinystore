//! The async API's calls of the Backup engine, beside the blocking ones of
//! [`crate::backup`]: a backup of the whole store as one zip, made while the
//! store keeps working, its write awaited.
//!
//! ```no_run
//! # async fn example() -> tinystore::Result<()> {
//! use tinystore::Options;
//! use tinystore::aio::Store;
//!
//! let store = Store::open("data", Options::default())?;
//! let backup = store.backup().include("secret.key").write("backups/nightly.zip").await?;
//! assert!(!backup.key_left_out, "a store that seals values needs its key beside the backup");
//! store.close().await?;
//! tinystore::backup::restore("backups/nightly.zip", "restored")?; // blocking: no store is open there yet
//! # Ok(())
//! # }
//! ```
//!
//! A backup runs blocking on a thread of the store's pool, minutes on a
//! large store, each engine copying its file while it keeps working, so the
//! thread that polls its future waits for none of it. Once polled it runs to
//! its end, as a write does, whether or not anyone still awaits it: its zip
//! is written whole, or, when it fails, no zip and no snapshot are left. A
//! future made and dropped unpolled writes nothing.
//!
//! [`crate::backup::restore`] stays a blocking call: it runs before a store
//! opens in its directory, where there is no store and no pool yet.

use std::fmt;
use std::future::Future;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use super::run;
use crate::Result;
use crate::pool::Pool;

pub use crate::backup::Backup;

impl super::Store {
    /// A backup of every engine of the store, made while it keeps working,
    /// as [`crate::Store::backup`] builds one.
    pub fn backup(&self) -> BackupBuilder {
        BackupBuilder { blocking: self.blocking().backup(), pool: Arc::clone(self.pool()) }
    }
}

/// A backup about to be made, as [`crate::backup::BackupBuilder`] builds
/// one: the files of the program's it takes beside the engines', and
/// whether it takes the store's encryption key.
///
/// `write` and `write_to` run blocking on a thread of the store's pool, and
/// once polled each runs to its end, whether or not anyone still awaits it.
#[must_use = "a backup is made by write or write_to"]
pub struct BackupBuilder {
    blocking: crate::backup::BackupBuilder,
    pool: Arc<Pool>,
}

impl fmt::Debug for BackupBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.blocking, f)
    }
}

impl BackupBuilder {
    /// Includes a file of the program's, by its path inside the store's
    /// directory, such as `secret.key`, which a restore puts back where it
    /// was.
    pub fn include(self, name: &str) -> Self {
        BackupBuilder { blocking: self.blocking.include(name), pool: self.pool }
    }

    /// Includes the store's encryption key, as it is, which a backup leaves
    /// out otherwise: the zip is not encrypted, so a backup with the key
    /// opens every value it holds.
    pub fn include_encryption_key(self) -> Self {
        BackupBuilder { blocking: self.blocking.include_encryption_key(), pool: self.pool }
    }

    /// Writes the backup to `path` whole, as
    /// [`crate::backup::BackupBuilder::write`] does: beside it first, then
    /// renamed once synced, so that a backup that fails leaves no zip.
    pub fn write(self, path: impl AsRef<Path>) -> impl Future<Output = Result<Backup>> + Send + 'static {
        let (builder, path) = (self.blocking, path.as_ref().to_path_buf());
        let what = format!("backup {}", path.display());
        run::blocking(&self.pool, None, what, move || builder.write(path))
    }

    /// Writes the backup to `out` as it is made, as
    /// [`crate::backup::BackupBuilder::write_to`] does, for a program that
    /// sends it elsewhere: `out` is written on the thread of the store's
    /// pool that makes the backup, and what it holds of a backup that fails
    /// is no backup.
    pub fn write_to(self, out: impl Write + Send + 'static) -> impl Future<Output = Result<Backup>> + Send + 'static {
        let builder = self.blocking;
        run::blocking(&self.pool, None, "backup".to_owned(), move || builder.write_to(out))
    }
}

#[cfg(test)]
#[path = "backup_tests.rs"]
mod tests;
