#![allow(unsafe_code)]
//! The server's named pipe on tokio, on Windows: an instance the library
//! made, of the user's alone and refusing remote clients, as an embedded
//! store's endpoint makes its own, handed to tokio. Tokio takes a handle made
//! elsewhere only through `unsafe`, so this module is one of the command
//! line's two homes of it, beside `own_handles`.

use std::io;
use std::os::windows::io::IntoRawHandle as _;

use tokio::net::windows::named_pipe::NamedPipeServer;

/// An instance of the pipe `name`, the first of its name when `first` says,
/// registered with the runtime this is called on.
pub(crate) fn instance(name: &str, first: bool) -> io::Result<NamedPipeServer> {
    let handle = tinystore::local::pipe_instance(name, first)?;
    // SAFETY: the library made the handle for overlapped calls, as tokio
    // asks, and gives it up here: nothing else owns or closes it.
    unsafe { NamedPipeServer::from_raw_handle(handle.into_raw_handle()) }
}
