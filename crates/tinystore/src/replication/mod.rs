//! Replication: every store a node, each file written by one home and copied
//! by the nodes that ask for it, as [plan/scaling.md] describes.
//!
//! A node keeps a log of each file it writes, as the pages each commit
//! wrote, in `log/<file>/` of its directory. Another node takes a file from
//! it over a link: a snapshot of its pages first, then each entry as the log
//! grows, applied to a copy its engines read; and it serves what it copies to
//! the nodes that take from it in turn, the same bytes its home wrote.
//!
//! [plan/scaling.md]: https://github.com/tinyshed/tinystore/blob/rust/plan/scaling.md

mod copy;
mod entry;
mod follow;
mod forward;
mod log;
mod message;
mod node;
mod pack;
mod promote;
mod queue;
mod serve;
mod snapshot;
mod status;
mod transport;

use std::fmt;

pub(crate) use copy::is_copy;
#[cfg(feature = "blobs")]
pub(crate) use forward::BLOBS;
#[cfg(feature = "jobs")]
pub(crate) use forward::JOBS;
#[cfg(feature = "kv")]
pub(crate) use forward::KV;
#[cfg(feature = "search")]
pub(crate) use forward::SEARCH;
#[cfg(feature = "sql")]
pub(crate) use forward::SQL;
pub(crate) use forward::{Lenders, database_file, database_of, enqueue, make_in, pass_on, queued, tag_of};
pub(crate) use log::Notify;
pub(crate) use message::{In, Out};
pub(crate) use node::State;
pub use promote::{Demoted, Promoted, demote, promote};
pub use status::{FileStatus, status};

/// What makes a store a node: it keeps a log of each file it writes, from
/// which other nodes copy the file, and copies the files of the node it
/// takes from. A draft: the names are the owner's to settle.
#[derive(Clone, Default)]
pub struct Node {
    /// Where this node takes the files it copies, `host:port`, tried in order
    /// until one answers; none for a node that only writes its own.
    pub from: Vec<String>,
    /// Where other nodes take files from this one: an address to listen on,
    /// `host:port`, or none.
    pub serve: Option<String>,
    /// The files this node copies from `from`, by their names in its
    /// directory, as `kv.db` or `sql/app.db`; every other file is its own.
    /// `None` copies every file the node it takes from serves, but its own
    /// records and metrics.
    pub files: Option<Vec<String>>,
    /// What this node says to the node it takes from, and asks of the nodes
    /// that take from it; none admits any.
    pub token: Option<String>,
    /// A write of a file this node writes answers once a node copying it
    /// holds its commit, while one follows it: losing this node then loses no
    /// write it answered.
    pub synchronous: bool,
    /// TLS on its links, with the `replication-tls` feature; plain TCP when
    /// none.
    pub tls: Option<NodeTls>,
}

/// TLS on a node's links: the certificate its server answers with, and the
/// roots it trusts of the node it takes from. A draft, as its names are.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NodeTls {
    /// Its certificate chain, PEM, which the nodes that take from it check.
    pub cert: Option<std::path::PathBuf>,
    /// The chain's private key, PEM.
    pub key: Option<std::path::PathBuf>,
    /// The certificates it trusts of the node it takes from, PEM: a CA's, or
    /// that node's own.
    pub roots: Option<std::path::PathBuf>,
}

impl fmt::Debug for Node {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Node")
            .field("from", &self.from)
            .field("serve", &self.serve)
            .field("files", &self.files)
            .field("token", &self.token.as_ref().map(|_| "hidden"))
            .field("synchronous", &self.synchronous)
            .field("tls", &self.tls)
            .finish()
    }
}

/// The files a node keeps as its own even when it copies every other: what
/// it logged and measured of itself.
pub(crate) const NODE_LOCAL: [&str; 2] = ["records.db", "metrics.db"];

/// The directory of a store's logs, beside its engines' files.
pub(crate) const DIR: &str = "log";

#[cfg(test)]
pub(crate) use log::{Bounds, Log};

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

#[cfg(all(
    test,
    feature = "jobs",
    feature = "sql",
    feature = "search",
    feature = "blobs",
    feature = "records",
    feature = "metrics"
))]
#[path = "link_tests.rs"]
mod link_tests;
