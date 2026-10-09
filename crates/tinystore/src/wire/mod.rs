//! The wire protocol of docs/wire.md: frames, the MessagePack profile, a
//! session that answers them, and each engine's messages. The server's
//! sockets and the FFI's pipe carry the same frames through the same session.

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
mod workers;

pub(crate) use session::Session;
pub use session::Wake;

/// How a session runs a method: a point read at once on the caller's thread,
/// a write queued for its group commit with no thread waiting on it, a run of
/// once handed over, a worker whose jobs and answers are the stream's items,
/// anything else on a worker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Route {
    Inline,
    Submit,
    Handover,
    #[cfg(feature = "jobs")]
    Exchange,
    Worker,
}
