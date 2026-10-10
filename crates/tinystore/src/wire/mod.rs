//! The wire protocol of docs/wire.md: frames, the MessagePack profile, a
//! session that answers them, and each engine's messages. The server's
//! sockets and the FFI's pipe carry the same frames through the same session.

#[cfg(feature = "blobs")]
mod blobs;
pub(crate) mod codec;
pub(crate) mod frame;
#[cfg(feature = "jobs")]
mod jobs;
mod kv;
mod message;
pub(crate) mod msgpack;
#[rustfmt::skip]
pub(crate) mod protocol;
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
/// watch whose reports are, anything else on a worker.
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
    Get,
    #[cfg(feature = "blobs")]
    Upload,
    Worker,
}
