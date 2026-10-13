//! The wire protocol of docs/wire.md: frames, the MessagePack profile, a
//! session that answers them, and each engine's messages. The server's
//! sockets and the FFI's pipe carry the same frames through the same session.

#[cfg(feature = "backup")]
mod backup;
#[cfg(feature = "blobs")]
mod blobs;
pub(crate) mod codec;
#[cfg(feature = "config")]
mod config;
pub(crate) mod frame;
#[cfg(feature = "jobs")]
mod jobs;
mod kv;
mod message;
#[cfg(feature = "metrics")]
mod metrics;
pub(crate) mod msgpack;
#[rustfmt::skip]
pub(crate) mod protocol;
#[cfg(feature = "records")]
mod records;
mod session;
#[cfg(feature = "sql")]
mod sql;
mod workers;

pub(crate) use session::Session;
pub use session::Wake;

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
    Worker,
}

/// Work on a session's workers, off the thread that asked.
#[cfg(any(feature = "blobs", feature = "records", feature = "metrics", feature = "config", feature = "backup"))]
pub(crate) type Spawn = std::sync::Arc<dyn Fn(Box<dyn FnOnce() + Send>) + Send + Sync>;

/// What a stream whose DATA goes either way needs of its session: a file's
/// bytes, a tail's batches, a writer's lines, a read's parts, the answers to
/// a client's instruments, a config's values.
#[cfg(any(feature = "blobs", feature = "records", feature = "metrics", feature = "config", feature = "backup"))]
#[derive(Clone)]
pub(crate) struct Link {
    /// A DATA on the stream.
    pub(crate) send: std::sync::Arc<dyn Fn(Vec<u8>) + Send + Sync>,
    /// The stream's last frame.
    pub(crate) finish: std::sync::Arc<dyn Fn(Result<Vec<u8>, protocol::Failure>) + Send + Sync>,
    /// Credit given back to the client for DATA it sent on the stream.
    pub(crate) grant: std::sync::Arc<dyn Fn(u32) + Send + Sync>,
    /// Work on the session's workers, where a file is read or written.
    pub(crate) spawn: Spawn,
    /// The DATA bytes the client takes before it grants more.
    pub(crate) credit: u64,
    pub(crate) max_body: usize,
}
