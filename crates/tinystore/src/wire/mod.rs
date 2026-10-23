//! The wire protocol of docs/wire.md: frames, the MessagePack profile, a
//! session that answers them, and each engine's messages. The server's
//! sockets and the FFI's pipe carry the same frames through the same session.

#[cfg(feature = "backup")]
mod backup;
#[cfg(feature = "blobs")]
mod blobs;
#[cfg(feature = "channels")]
mod channels;
pub(crate) mod codec;
#[cfg(feature = "config")]
mod config;
pub(crate) mod frame;
#[cfg(feature = "jobs")]
mod jobs;
mod kv;
#[cfg(feature = "locks")]
mod locks;
mod message;
#[cfg(feature = "metrics")]
mod metrics;
pub(crate) mod msgpack;
mod output;
#[rustfmt::skip]
pub(crate) mod protocol;
#[cfg(feature = "records")]
mod records;
#[cfg(feature = "search")]
mod search;
mod session;
#[cfg(feature = "sql")]
mod sql;
pub(crate) mod workers;

pub(crate) use session::Session;
pub use session::Wake;

/// The largest part a DATA carries, less `own` bytes of its frame: half of
/// what the client takes at once, so that one grant lets the next part go
/// while it reads the last.
#[cfg(any(feature = "sql", feature = "blobs", feature = "metrics"))]
pub(crate) fn part_bound(max_body: usize, credit: u64, own: usize) -> usize {
    (max_body.min(usize::try_from(credit).unwrap_or(usize::MAX)) / 2).saturating_sub(own).max(1)
}

/// A call's answer: its body, or the failure the client is told.
pub(crate) type Answered = Result<Vec<u8>, protocol::Failure>;

/// What a connection opened of one kind, by the handle each was given.
#[cfg(any(feature = "blobs", feature = "config", feature = "records", feature = "search", feature = "sql"))]
#[derive(Debug)]
pub(crate) struct ByHandle<T> {
    last: std::sync::atomic::AtomicU64,
    open: std::sync::Mutex<std::collections::HashMap<u64, T>>,
}

#[cfg(any(feature = "blobs", feature = "config", feature = "records", feature = "search", feature = "sql"))]
impl<T> Default for ByHandle<T> {
    fn default() -> Self {
        ByHandle { last: Default::default(), open: Default::default() }
    }
}

#[cfg(any(feature = "blobs", feature = "config", feature = "records", feature = "search", feature = "sql"))]
impl<T: Clone> ByHandle<T> {
    /// Keeps `value` under a handle no other the connection opened has.
    pub(crate) fn keep(&self, value: T) -> u64 {
        let handle = self.last.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        crate::sync::lock(&self.open).insert(handle, value);
        handle
    }

    /// What `handle` names; `missing` says why there is none.
    pub(crate) fn get(&self, handle: u64, missing: impl FnOnce() -> String) -> Result<T, protocol::Failure> {
        crate::sync::lock(&self.open).get(&handle).cloned().ok_or_else(|| protocol::Failure::invalid(missing()))
    }

    #[cfg(feature = "config")]
    pub(crate) fn clear(&self) {
        crate::sync::lock(&self.open).clear();
    }
}

/// The file of a database a connection opened, by its handle: where a bucket
/// or a queue opened with that handle is kept.
pub(crate) type Lent<'a> = &'a dyn Fn(u64) -> Result<std::sync::Arc<crate::engine::SharedFile>, protocol::Failure>;

/// How a session runs a method: a point read at once on the caller's thread,
/// a write queued for its group commit with no thread waiting on it, a run of
/// once handed over, a worker whose jobs and answers are the stream's items, a
/// watch whose reports are, a tail whose batches are, a writer whose lines
/// are, anything else on a worker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Route {
    Inline,
    Submit,
    Handover,
    #[cfg(feature = "jobs")]
    Exchange,
    #[cfg(feature = "jobs")]
    Watch,
    #[cfg(feature = "sql")]
    Download,
    #[cfg(feature = "sql")]
    Transaction,
    #[cfg(feature = "blobs")]
    Read,
    #[cfg(feature = "blobs")]
    Upload,
    #[cfg(feature = "records")]
    Tail,
    #[cfg(feature = "records")]
    Lines,
    /// A read, the newest samples or an aggregate, answered in parts.
    #[cfg(feature = "metrics")]
    Parts,
    /// A client's instruments, whose batches and answers are the stream's
    /// items.
    #[cfg(feature = "metrics")]
    Instruments,
    /// A config's values, now and at each change, until the client cancels.
    #[cfg(feature = "config")]
    Follow,
    /// A backup's zip in pieces, then what it holds.
    #[cfg(feature = "backup")]
    Backup,
    /// A hold, answered once its key is given, by whichever thread lets it go.
    #[cfg(feature = "locks")]
    Hold,
    /// A subscription's batches, within the client's credit, until it cancels.
    #[cfg(feature = "channels")]
    Subscribe,
    Worker,
}

/// Work on a session's workers, off the thread that asked.
#[cfg(any(
    feature = "jobs",
    feature = "blobs",
    feature = "records",
    feature = "metrics",
    feature = "config",
    feature = "backup",
    feature = "channels"
))]
pub(crate) type Spawn = std::sync::Arc<dyn Fn(Box<dyn FnOnce() + Send>) + Send + Sync>;

/// What a stream whose DATA goes either way needs of its session: a file's
/// bytes, a tail's batches, a writer's lines, a read's parts, the answers to
/// a client's instruments, a config's values.
#[cfg(any(
    feature = "blobs",
    feature = "records",
    feature = "metrics",
    feature = "config",
    feature = "backup",
    feature = "channels"
))]
#[derive(Clone)]
pub(crate) struct Link {
    /// A DATA on the stream.
    pub(crate) send: std::sync::Arc<dyn Fn(Vec<u8>) + Send + Sync>,
    /// The stream's last frame.
    pub(crate) finish: std::sync::Arc<dyn Fn(Result<Vec<u8>, protocol::Failure>) + Send + Sync>,
    /// Credit given back to the client for DATA it sent on the stream.
    #[cfg_attr(
        not(any(feature = "blobs", feature = "records", feature = "metrics")),
        expect(dead_code, reason = "only blobs', records' and metrics' streams take DATA from a client")
    )]
    pub(crate) grant: std::sync::Arc<dyn Fn(u32) + Send + Sync>,
    /// Work on the session's workers, where a file is read or written.
    pub(crate) spawn: Spawn,
    /// The DATA bytes the client takes before it grants more.
    pub(crate) credit: u64,
    pub(crate) max_body: usize,
    /// The bytes the connection owes its client, queued and not yet taken.
    #[cfg(feature = "channels")]
    pub(crate) owed: std::sync::Arc<dyn Fn() -> usize + Send + Sync>,
    /// Runs a task once what the connection owes fell below its low mark.
    #[cfg(feature = "channels")]
    pub(crate) when_drained: Spawn,
}
