//! Replication: every store a node, each file written by one home and copied
//! by the nodes that ask for it, as [plan/scaling.md] describes.
//!
//! Built so far, phase 7's first slice: a node keeps a log of each file's
//! commits, as the pages each one wrote, in `log/<file>/` of its directory.
//!
//! [plan/scaling.md]: https://github.com/tinyshed/tinystore/blob/rust/plan/scaling.md

mod copy;
mod entry;
mod log;

use std::path::{Path, PathBuf};
use std::sync::Arc;

pub(crate) use copy::is_copy;
pub(crate) use log::{Bounds, Log};

use crate::Result;
use crate::sqlite::Observe;

/// What makes a store a node: it keeps a log of each file it writes, from
/// which other nodes copy the file. A draft: its links, `from` and `serve`,
/// come with phase 7's fourth slice.
#[derive(Clone, Debug, Default)]
pub struct Node {}

/// Where a node's logs are, a directory a file.
pub(crate) struct Logs {
    root: PathBuf,
    bounds: Bounds,
}

impl Logs {
    pub(crate) fn of_store(dir: &Path) -> Logs {
        Logs { root: dir.join(DIR), bounds: Bounds::default() }
    }

    /// The log of the file `name` inside the store's directory.
    pub(crate) fn open(&self, name: &str) -> Result<Arc<dyn Observe>> {
        Ok(Arc::new(Log::open(self.dir_of(name), self.bounds)?))
    }

    /// The directory of the log of the file `name`.
    pub(crate) fn dir_of(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }
}

/// The directory of a store's logs, beside its engines' files.
pub(crate) const DIR: &str = "log";

#[cfg(test)]
#[path = "log_tests.rs"]
mod tests;

#[cfg(all(
    test,
    feature = "jobs",
    feature = "sql",
    feature = "search",
    feature = "blobs",
    feature = "records",
    feature = "metrics"
))]
#[path = "node_tests.rs"]
mod node_tests;

#[cfg(all(
    test,
    feature = "jobs",
    feature = "sql",
    feature = "search",
    feature = "blobs",
    feature = "records",
    feature = "metrics"
))]
#[path = "copy_tests.rs"]
mod copy_tests;
