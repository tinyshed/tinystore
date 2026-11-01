//! A client's CANCEL, and the tokens of the reads under way that it stops.

use std::sync::Arc;

use super::{ClientRun, Shared};
use crate::CancellationToken;
use crate::sync::lock;
use crate::wire::protocol::Failure;
use crate::wire::protocol::method::{self, Access};

impl Shared {
    /// Lets go of a client's run of once, answering `cancelled`. A read under
    /// way stops, its statement interrupted, and answers `cancelled`; a call
    /// that writes runs to its end, short as a call is. A worker cancelled
    /// ends as its END would end it, the jobs it held failing their attempt,
    /// and a watch cancelled ends.
    pub(super) fn cancel(self: &Arc<Self>, stream: u32) {
        let read = lock(&self.reads).get(&stream).cloned();
        if let Some(read) = read {
            read.cancel();
        }
        #[cfg(feature = "sql")]
        if self.sql.cancel(stream) {
            return;
        }
        #[cfg(feature = "blobs")]
        if self.blobs.cancel(stream) {
            return;
        }
        #[cfg(feature = "records")]
        if self.records.cancel(stream) {
            return;
        }
        #[cfg(feature = "metrics")]
        if self.metrics.cancel(stream) {
            return;
        }
        #[cfg(feature = "config")]
        if self.config.cancel(stream) {
            return;
        }
        #[cfg(feature = "backup")]
        if self.backups.cancel(stream) {
            return;
        }
        #[cfg(feature = "channels")]
        if self.channels.cancel(stream) {
            return;
        }
        #[cfg(feature = "locks")]
        if self.locks.cancel(stream) {
            return self.answer(stream, Err(Failure::cancelled("the client cancelled its hold")));
        }
        #[cfg(feature = "jobs")]
        if self.jobs.work(stream).is_some() {
            return self.end_work(stream, Err(Failure::cancelled("the client cancelled its worker")));
        }
        #[cfg(feature = "jobs")]
        if let Some(watch) = self.jobs.take_watch(stream) {
            return watch.close(Err(Failure::cancelled("the client cancelled its watch")));
        }
        let mut runs = self.lock_runs();
        let cancelled = Failure::cancelled("the client cancelled the run");
        match runs.remove(&stream) {
            Some(ClientRun::Asking(_)) => self.answer(stream, Err(cancelled)),
            Some(ClientRun::Handed(handed)) => {
                self.end_stream(stream, Err(cancelled));
                drop(runs);
                drop(handed);
            }
            None => {}
        }
    }

    /// The token of a read a worker runs for `stream`, which a CANCEL of the
    /// stream cancels; none for a call that writes, which runs to its end.
    pub(super) fn reading(&self, stream: u32, method: u16) -> Option<CancellationToken> {
        if method::access(method) != Some(Access::Read) {
            return None;
        }
        let token = CancellationToken::new();
        lock(&self.reads).insert(stream, token.clone());
        Some(token)
    }

    /// Runs `read` under its stream's token, then lets the token go, unless a
    /// REQUEST that named the stream's number again has put its own there.
    pub(super) fn read_under<T>(&self, stream: u32, token: Option<CancellationToken>, read: impl FnOnce() -> T) -> T {
        let Some(token) = token else { return read() };
        let answered = token.run(read);
        let mut reads = lock(&self.reads);
        if reads.get(&stream).is_some_and(|kept| kept.same(&token)) {
            reads.remove(&stream);
        }
        answered
    }
}
