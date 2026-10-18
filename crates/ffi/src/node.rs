//! Node's way to the core in its own process, `tinystore.node`: the pipe as
//! an addon of Node-API, carrying the wire's frames whole as the C ABI does.
//!
//! Every call comes on Node's one thread, so that thread is the pipe's one
//! reader: `send` hands back what is ready at once, and the core's wake, a
//! threadsafe function, tells the loop to read the rest with `recv`. The
//! wake keeps the loop alive while the pipe is open, as a socket would, so
//! that a program awaiting a write does not exit before its commit.

use std::sync::{Arc, Mutex, PoisonError};

use napi::bindgen_prelude::Buffer;
use napi::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi::{Error, Result, Status};
use napi_derive::napi;
use tinystore::pipe::{Pipe, Wake};

/// The host's wake: called with nothing, its errors the host's to throw.
type NodeWake = ThreadsafeFunction<(), (), (), Status, false>;

/// A connection in memory to the store in one directory.
#[napi(js_name = "Pipe")]
pub(crate) struct NodePipe {
    /// None once closed.
    pipe: Option<Pipe>,
    /// Shared with the core's wake and taken at close, so that the loop is
    /// let go then, even while a core thread still holds the wake.
    wake: Arc<Mutex<Option<NodeWake>>>,
}

impl std::fmt::Debug for NodePipe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NodePipe").field("pipe", &self.pipe).finish_non_exhaustive()
    }
}

#[napi]
#[expect(unreachable_pub, reason = "napi exports only a method written pub, through its registration")]
impl NodePipe {
    /// Hands frames to the store and returns the frames ready at once, none
    /// when nothing is; what comes later is announced by the wake.
    #[napi]
    pub fn send(&self, frames: Buffer) -> Option<Buffer> {
        let ready = self.pipe.as_ref()?.send(&frames);
        (!ready.is_empty()).then(|| ready.into())
    }

    /// The bytes the store wrote since the last read, without waiting: none
    /// when nothing came.
    #[napi]
    pub fn recv(&self) -> Option<Buffer> {
        let ready = self.pipe.as_ref()?.recv(std::time::Duration::ZERO);
        (!ready.is_empty()).then(|| ready.into())
    }

    /// Ends the connection; the store closes with its last one.
    #[napi]
    pub fn close(&mut self) {
        self.wake.lock().unwrap_or_else(PoisonError::into_inner).take();
        self.pipe.take();
    }
}

/// Opens the store in `dir`, or joins it when this process holds it already.
/// `options` is the wire's `store.Options`, no bytes for a store as it opens
/// unasked. A store that does not open throws an Error with its message.
#[napi]
pub(crate) fn open(dir: String, options: Buffer, wake: NodeWake) -> Result<NodePipe> {
    let wake = Arc::new(Mutex::new(Some(wake)));
    let held = Arc::clone(&wake);
    let core_wake: Wake = Arc::new(move || {
        if let Some(wake) = held.lock().unwrap_or_else(PoisonError::into_inner).as_ref() {
            wake.call((), ThreadsafeFunctionCallMode::NonBlocking);
        }
    });
    match Pipe::open(&dir, &options, Some(core_wake)) {
        Ok(pipe) => Ok(NodePipe { pipe: Some(pipe), wake }),
        Err(error) => {
            wake.lock().unwrap_or_else(PoisonError::into_inner).take();
            Err(Error::new(Status::GenericFailure, error.to_string()))
        }
    }
}
