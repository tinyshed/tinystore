//! TinyStore's C ABI: five functions that carry the wire's frames, whole, to a
//! store in this process. They know nothing of engines, so they never change:
//! a new call or a new engine is new messages, agreed in HELLO.
//!
//! Every function takes and returns pointers and integers alone, which bun:ffi,
//! ctypes and cgo all pass without a struct. A panic never crosses: it ends
//! the call with nothing, or `open` with its message.

#![allow(unsafe_code)]

use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;
use std::sync::Arc;
use std::time::Duration;

use tinystore::pipe::{Pipe, Wake};

/// A connection: a pipe to the store in one directory.
#[derive(Debug)]
pub struct Connection {
    pipe: Pipe,
}

/// The host's wake function and the pointer it passes back, which the host
/// keeps valid until it closes the connection.
struct HostWake {
    wake: unsafe extern "C" fn(*mut c_void),
    context: *mut c_void,
}

// SAFETY: the host promises, by passing them to tinystore_open, that its wake
// function may run on any thread with its context until tinystore_close
// returns; the pointer is only ever handed back to that function.
unsafe impl Send for HostWake {}
// SAFETY: as above; the function is called, never the context dereferenced.
unsafe impl Sync for HostWake {}

impl HostWake {
    fn call(&self) {
        // SAFETY: the host promised its wake may run on any thread with its
        // context until tinystore_close returns, which outlives every call.
        unsafe { (self.wake)(self.context) }
    }
}

/// Opens the store in the directory `dir` names, as UTF-8 of `dir_len` bytes,
/// or joins it when this process holds it through another connection.
///
/// On success it writes the connection to `connection` and returns 0. On
/// failure it writes a message, UTF-8, to `error` and returns its length; the
/// host gives it back with `tinystore_free`. `wake`, when not null, is called
/// with `context` from any thread once frames become ready after the host took
/// the last ones, and not again until it takes them.
///
/// # Safety
///
/// `dir` points to `dir_len` readable bytes; `connection` and `error` point to
/// writable pointers; `wake`, when given, may be called from any thread with
/// `context` until `tinystore_close` returns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tinystore_open(
    dir: *const u8,
    dir_len: usize,
    wake: Option<unsafe extern "C" fn(*mut c_void)>,
    context: *mut c_void,
    connection: *mut *mut Connection,
    error: *mut *mut u8,
) -> usize {
    // SAFETY: the caller promises dir points to dir_len readable bytes.
    let dir = unsafe { std::slice::from_raw_parts(dir, dir_len) };
    let opened = catch_unwind(AssertUnwindSafe(|| {
        let dir = std::str::from_utf8(dir).map_err(|_| "a directory name that is not UTF-8".to_owned())?;
        let wake = wake.map(|wake| {
            let host = HostWake { wake, context };
            Arc::new(move || host.call()) as Wake
        });
        Pipe::open(dir, wake).map_err(|failure| failure.to_string())
    }));
    let outcome = opened.unwrap_or_else(|_| Err("tinystore_open panicked".to_owned()));
    match outcome {
        Ok(pipe) => {
            // SAFETY: the caller promises connection points to a writable pointer.
            unsafe { *connection = Box::into_raw(Box::new(Connection { pipe })) };
            0
        }
        // SAFETY: the caller promises error points to a writable pointer.
        Err(message) => unsafe { hand_over(message.into_bytes(), error) },
    }
}

/// Hands `len` bytes of frames to the connection, and returns the length of
/// the frames ready at once, written to `out`; 0 writes nothing.
///
/// # Safety
///
/// `connection` came from `tinystore_open` and is not closed; `frames` points
/// to `len` readable bytes; `out` points to a writable pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tinystore_send(
    connection: *mut Connection,
    frames: *const u8,
    len: usize,
    out: *mut *mut u8,
) -> usize {
    // SAFETY: the caller promises a live connection and len readable bytes.
    let (connection, frames) = unsafe { (&*connection, std::slice::from_raw_parts(frames, len)) };
    let ready = catch_unwind(AssertUnwindSafe(|| connection.pipe.send(frames))).unwrap_or_default();
    // SAFETY: the caller promises out points to a writable pointer.
    unsafe { hand_over(ready, out) }
}

/// Returns the length of the frames ready since, written to `out`, waiting up
/// to `wait_ms` for the first; 0 does not wait, and 0 bytes writes nothing.
///
/// # Safety
///
/// `connection` came from `tinystore_open` and is not closed; `out` points to
/// a writable pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tinystore_recv(connection: *mut Connection, wait_ms: u32, out: *mut *mut u8) -> usize {
    // SAFETY: the caller promises a live connection.
    let connection = unsafe { &*connection };
    let wait = Duration::from_millis(u64::from(wait_ms));
    let ready = catch_unwind(AssertUnwindSafe(|| connection.pipe.recv(wait))).unwrap_or_default();
    // SAFETY: the caller promises out points to a writable pointer.
    unsafe { hand_over(ready, out) }
}

/// Ends a connection; the store closes with its last one. Calls under way
/// finish first, and the wake function is not called after this returns.
///
/// # Safety
///
/// `connection` came from `tinystore_open` and is closed once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tinystore_close(connection: *mut Connection) {
    if connection.is_null() {
        return;
    }
    // SAFETY: the caller promises the connection came from tinystore_open and
    // is closed once, so this box is the only owner.
    let connection = unsafe { Box::from_raw(connection) };
    let _ = catch_unwind(AssertUnwindSafe(move || drop(connection)));
}

/// Gives back bytes that `tinystore_open`, `tinystore_send` or
/// `tinystore_recv` handed over.
///
/// # Safety
///
/// `bytes` and `len` are exactly what one of those calls handed over, given
/// back once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tinystore_free(bytes: *mut u8, len: usize) {
    if bytes.is_null() || len == 0 {
        return;
    }
    // SAFETY: the caller promises these came from hand_over, which leaked a
    // boxed slice of exactly len bytes.
    drop(unsafe { Box::from_raw(ptr::slice_from_raw_parts_mut(bytes, len)) });
}

/// Writes bytes for the host to read and give back, and returns their length;
/// nothing is written for no bytes.
///
/// # Safety
///
/// `out` points to a writable pointer.
unsafe fn hand_over(bytes: Vec<u8>, out: *mut *mut u8) -> usize {
    if bytes.is_empty() {
        return 0;
    }
    let len = bytes.len();
    let leaked = Box::into_raw(bytes.into_boxed_slice()) as *mut u8;
    // SAFETY: the caller promises out points to a writable pointer.
    unsafe { *out = leaked };
    len
}
