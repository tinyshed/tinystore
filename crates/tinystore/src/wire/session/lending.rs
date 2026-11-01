//! What one engine lends another inside a database's file: the file to a
//! bucket or a queue opened in it, a queue's add to its batch, and the calls
//! a transaction carries.

use std::sync::Arc;

use super::Shared;
#[cfg(feature = "sql")]
use crate::Transaction;
use crate::engine::SharedFile;
#[cfg(all(feature = "sql", feature = "blobs"))]
use crate::wire::blobs;
#[cfg(all(feature = "sql", feature = "channels"))]
use crate::wire::channels;
#[cfg(all(feature = "sql", feature = "jobs"))]
use crate::wire::jobs;
#[cfg(feature = "sql")]
use crate::wire::kv;
use crate::wire::protocol::Failure;
#[cfg(all(feature = "sql", feature = "jobs"))]
use crate::wire::protocol::method;

impl Shared {
    /// The file of a database this connection opened, for a bucket or a
    /// queue opened in it.
    pub(super) fn lent(&self, handle: u64) -> std::result::Result<Arc<SharedFile>, Failure> {
        #[cfg(feature = "sql")]
        return self.sql.shared(handle);
        #[cfg(not(feature = "sql"))]
        Err(Failure::invalid(format!("database {handle}: this server keeps no databases")))
    }

    /// A call of another engine that a database's batch carries: a queue's
    /// add, for a queue kept in the database's file.
    #[cfg(feature = "sql")]
    pub(super) fn lend(&self, called: u16, body: &[u8]) -> std::result::Result<crate::sql::Write, Failure> {
        match called {
            #[cfg(feature = "jobs")]
            method::JOBS_ADD => jobs::lend(&self.jobs, body),
            _ => Err(Failure::invalid(format!(
                "method {called:#06x} does not run in a batch, which takes a queue's add"
            ))),
        }
    }

    /// Answers a call of kv or jobs made inside a database's transaction.
    #[cfg(feature = "sql")]
    pub(super) fn call_in(
        &self,
        tx: &Transaction<'_>,
        method: u16,
        body: &[u8],
        max_body: usize,
    ) -> std::result::Result<Vec<u8>, Failure> {
        match method >> 8 {
            0x01 => kv::call_in(&self.kv, tx, method, body, max_body),
            #[cfg(feature = "jobs")]
            0x02 => jobs::call_in(&self.jobs, tx, method, body, max_body),
            #[cfg(feature = "blobs")]
            0x04 => blobs::call_in(&self.blobs, tx, method, body),
            #[cfg(feature = "channels")]
            0x0a => channels::call_in(&self.store, tx, method, body),
            _ => Err(Failure::invalid(format!("method {method:#06x} does not run inside a transaction"))),
        }
    }
}
