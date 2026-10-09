//! The wire protocol of docs/wire.md: frames, the MessagePack profile, a
//! session that answers them, and each engine's messages. The server's
//! sockets and the FFI's pipe carry the same frames through the same session.

pub(crate) mod frame;
mod kv;
mod message;
pub(crate) mod msgpack;
mod session;
mod workers;

pub(crate) use session::Session;
pub use session::Wake;
