//! TinyStore's C ABI: four functions that carry the wire's frames, whole, to a
//! store in this process. They know nothing of engines, so they never change:
//! a new call or a new engine is new messages, agreed in HELLO.
//!
//! Every function takes and returns pointers and integers alone, which bun:ffi,
//! ctypes and cgo all pass without a struct. No memory of the core's crosses:
//! the host gives the bytes its frames are in and the bytes the core's are
//! written into, and the core keeps neither past the call. A panic never
//! crosses: it ends the call with nothing, or `open` with its message.

#![allow(unsafe_code)]

use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
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
/// `options` is the wire's `store.Options` in `options_len` bytes of
/// MessagePack, none for a store as it opens unasked.
///
/// On success it writes the connection to `connection` and returns 0. On
/// failure it writes a message, UTF-8, into `error`, as much of it as
/// `error_room` bytes hold, and returns how many it wrote. `wake`, when not
/// null, is called with `context` from any thread once frames become ready
/// after the host read the last ones, and not again until it reads.
///
/// # Safety
///
/// `dir` points to `dir_len` readable bytes and `options` to `options_len`;
/// `connection` points to a writable pointer; `error` points to `error_room`
/// writable bytes; `wake`,
/// when given, may be called from any thread with `context` until
/// `tinystore_close` returns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tinystore_open(
    dir: *const u8,
    dir_len: usize,
    options: *const u8,
    options_len: usize,
    wake: Option<unsafe extern "C" fn(*mut c_void)>,
    context: *mut c_void,
    connection: *mut *mut Connection,
    error: *mut u8,
    error_room: usize,
) -> usize {
    // SAFETY: the caller promises dir points to dir_len readable bytes.
    let dir = unsafe { std::slice::from_raw_parts(dir, dir_len) };
    // SAFETY: the caller promises options points to options_len readable bytes.
    let options = if options_len == 0 { &[] } else { unsafe { std::slice::from_raw_parts(options, options_len) } };
    let opened = catch_unwind(AssertUnwindSafe(|| {
        let dir = std::str::from_utf8(dir).map_err(|_| "a directory name that is not UTF-8".to_owned())?;
        let wake = wake.map(|wake| {
            let host = HostWake { wake, context };
            Arc::new(move || host.call()) as Wake
        });
        Pipe::open(dir, options, wake).map_err(|failure| failure.to_string())
    }));
    let outcome = opened.unwrap_or_else(|_| Err("tinystore_open panicked".to_owned()));
    match outcome {
        Ok(pipe) => {
            // SAFETY: the caller promises connection points to a writable pointer.
            unsafe { *connection = Box::into_raw(Box::new(Connection { pipe })) };
            0
        }
        Err(message) => {
            // SAFETY: the caller promises error points to error_room writable bytes.
            let room = unsafe { host_bytes(error, error_room) };
            let written = message.len().min(room.len());
            room[..written].copy_from_slice(&message.as_bytes()[..written]);
            written
        }
    }
}

/// Hands `len` bytes of frames to the connection, writes the frames ready at
/// once into `into`, as many of their bytes as `room` holds, and returns how
/// many it wrote.
///
/// What it writes is a stream: a frame may end in the next read. When it
/// returns `room`, more may wait, which no wake announces: the host reads on
/// with `tinystore_recv`. With no room it writes nothing, and every frame is
/// left to `tinystore_recv`: a host that sends from many threads does so, and
/// reads in one place.
///
/// # Safety
///
/// `connection` came from `tinystore_open` and is not closed; `frames` points
/// to `len` readable bytes; `into` points to `room` writable bytes, which no
/// other call writes or reads meanwhile.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tinystore_send(
    connection: *mut Connection,
    frames: *const u8,
    len: usize,
    into: *mut u8,
    room: usize,
) -> usize {
    // SAFETY: the caller promises a live connection and len readable bytes.
    let (connection, frames) = unsafe { (&*connection, std::slice::from_raw_parts(frames, len)) };
    // SAFETY: the caller promises into points to room writable bytes of its own.
    let into = unsafe { host_bytes(into, room) };
    catch_unwind(AssertUnwindSafe(|| connection.pipe.send_into(frames, into))).unwrap_or_default()
}

/// Writes the frames ready since into `into`, as many of their bytes as
/// `room` holds, waiting up to `wait_ms` for the first, and returns how many
/// it wrote; 0 does not wait. When it returns `room`, more may wait.
///
/// # Safety
///
/// `connection` came from `tinystore_open` and is not closed; `into` points
/// to `room` writable bytes, which no other call writes or reads meanwhile.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tinystore_recv(
    connection: *mut Connection,
    wait_ms: u32,
    into: *mut u8,
    room: usize,
) -> usize {
    // SAFETY: the caller promises a live connection.
    let connection = unsafe { &*connection };
    // SAFETY: the caller promises into points to room writable bytes of its own.
    let into = unsafe { host_bytes(into, room) };
    let wait = Duration::from_millis(u64::from(wait_ms));
    catch_unwind(AssertUnwindSafe(|| connection.pipe.recv_into(wait, into))).unwrap_or_default()
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

/// The host's bytes to write into; none where it gave no room, whatever the
/// pointer.
///
/// # Safety
///
/// With `room` above 0, `bytes` points to that many writable bytes that
/// nothing else reads or writes while the slice lives.
unsafe fn host_bytes<'host>(bytes: *mut u8, room: usize) -> &'host mut [u8] {
    if room == 0 || bytes.is_null() {
        return &mut [];
    }
    // SAFETY: the caller promises room writable bytes of its own at bytes.
    unsafe { std::slice::from_raw_parts_mut(bytes, room) }
}

#[cfg(test)]
mod tests {
    use std::ptr;

    use super::*;

    /// HELLO { 1 protocol: 2, 2 client: "c/0" }, a frame of kind 1 on no stream.
    const HELLO: [u8; 20] = [8, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0x82, 0x01, 0x02, 0x02, 0xa3, b'c', b'/', b'0'];
    const WELCOME: u8 = 2;

    fn opened(dir: &std::path::Path) -> *mut Connection {
        let name = dir.to_str().unwrap().as_bytes();
        let mut connection = ptr::null_mut();
        let mut error = [0u8; 256];
        // SAFETY: every pointer is to a live value of this function, of the length said.
        let failed = unsafe {
            tinystore_open(
                name.as_ptr(),
                name.len(),
                ptr::null(),
                0,
                None,
                ptr::null_mut(),
                &mut connection,
                error.as_mut_ptr(),
                256,
            )
        };
        assert_eq!(failed, 0, "{}", String::from_utf8_lossy(&error[..failed]));
        connection
    }

    /// The length a frame's header says its body has.
    fn body_of(frame: &[u8]) -> usize {
        u32::from_le_bytes(frame[..4].try_into().unwrap()) as usize
    }

    #[test]
    fn a_send_writes_what_is_ready_into_the_hosts_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let connection = opened(dir.path());
        let mut into = [0u8; 4096];
        // SAFETY: the connection is open, and every pointer is to live bytes of the length said.
        let written = unsafe { tinystore_send(connection, HELLO.as_ptr(), HELLO.len(), into.as_mut_ptr(), into.len()) };
        assert_eq!(into[4], WELCOME, "a HELLO is answered at once");
        assert_eq!(written, 12 + body_of(&into), "one frame, whole");
        // SAFETY: the connection came from tinystore_open and is closed once.
        unsafe { tinystore_close(connection) };
    }

    #[test]
    fn what_does_not_fit_comes_with_the_next_read_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let connection = opened(dir.path());
        let mut whole = Vec::new();
        let mut piece = [0u8; 5];
        // SAFETY: the connection is open, and every pointer is to live bytes of the length said.
        let mut written = unsafe { tinystore_send(connection, HELLO.as_ptr(), HELLO.len(), piece.as_mut_ptr(), 5) };
        while written == piece.len() {
            whole.extend_from_slice(&piece);
            // SAFETY: as above.
            written = unsafe { tinystore_recv(connection, 0, piece.as_mut_ptr(), piece.len()) };
        }
        whole.extend_from_slice(&piece[..written]);
        assert_eq!(whole[4], WELCOME);
        assert_eq!(whole.len(), 12 + body_of(&whole), "the frame came whole, five bytes at a time");
        // SAFETY: the connection came from tinystore_open and is closed once.
        unsafe { tinystore_close(connection) };
    }

    #[test]
    fn a_send_given_no_room_leaves_every_frame_to_the_read() {
        let dir = tempfile::tempdir().unwrap();
        let connection = opened(dir.path());
        // SAFETY: the connection is open; no bytes are given to write into.
        let written = unsafe { tinystore_send(connection, HELLO.as_ptr(), HELLO.len(), ptr::null_mut(), 0) };
        assert_eq!(written, 0);
        let mut into = [0u8; 4096];
        // SAFETY: the connection is open, and into is live for its length.
        let read = unsafe { tinystore_recv(connection, 10_000, into.as_mut_ptr(), into.len()) };
        assert_eq!((into[4], read), (WELCOME, 12 + body_of(&into)));
        // SAFETY: the connection came from tinystore_open and is closed once.
        unsafe { tinystore_close(connection) };
    }

    #[test]
    fn a_store_that_does_not_open_says_why_in_the_hosts_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a-file");
        std::fs::write(&file, b"not a directory").unwrap();
        let name = file.to_str().unwrap().as_bytes();
        let mut connection = ptr::null_mut();
        let mut error = [0u8; 256];
        // SAFETY: every pointer is to a live value of this function, of the length said.
        let failed = unsafe {
            tinystore_open(
                name.as_ptr(),
                name.len(),
                ptr::null(),
                0,
                None,
                ptr::null_mut(),
                &mut connection,
                error.as_mut_ptr(),
                256,
            )
        };
        assert!(failed > 0 && connection.is_null(), "a file is no store's directory");
        assert!(std::str::from_utf8(&error[..failed]).unwrap().contains("a-file"), "the message names what failed");

        // SAFETY: as above, with room for eight bytes of the message.
        let cut = unsafe {
            tinystore_open(
                name.as_ptr(),
                name.len(),
                ptr::null(),
                0,
                None,
                ptr::null_mut(),
                &mut connection,
                error.as_mut_ptr(),
                8,
            )
        };
        assert_eq!(cut, 8, "a message longer than its room is cut to it");
    }
}
