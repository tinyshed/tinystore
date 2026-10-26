#![allow(unsafe_code)]
//! Keeps this command's own stdin, stdout and stderr from a process it starts
//! to outlive it, on Windows. Windows gives a child every handle its parent
//! may hand down, whatever stdio the child is given: a sidecar would hold the
//! pipe a shell reads this command's output from, and `| tee` or `$(…)` would
//! wait for the sidecar's end, 30 s after its last client, rather than this
//! command's. Unix closes them in the child, which takes other stdio; std's
//! own way to choose on Windows, `inherit_handles`, is not stable yet.

use std::io;
use std::os::windows::io::AsRawHandle;

use windows_sys::Win32::Foundation::{HANDLE_FLAG_INHERIT, SetHandleInformation};

/// Lets no process started from now on have this command's stdio.
pub(crate) fn keep() {
    for handle in [io::stdin().as_raw_handle(), io::stdout().as_raw_handle(), io::stderr().as_raw_handle()] {
        // SAFETY: the handle is this process's own standard handle, open for
        // as long as the process runs, or none, which the call refuses; the
        // call clears its inherit flag and touches nothing else, and a child
        // given one of them as its stdio gets a duplicate std makes for it.
        unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) };
    }
}
