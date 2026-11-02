#![allow(unsafe_code)]
//! Whether the console shows the escape sequences colours are written with.
//! A Windows console from before Windows Terminal prints them as text until
//! the program asks it to read them, which only the system can be asked.

/// Whether the console of standard output and standard error reads escape
/// sequences, after asking each that is a console to: false only where a
/// console refuses, as one older than Windows 10 does. A stream that is no
/// console, a file, a pipe or Git Bash's pseudo-terminal, has nothing to ask.
#[cfg(windows)]
pub(super) fn reads_escapes() -> bool {
    use std::ffi::c_void;
    use std::os::windows::io::AsRawHandle;

    /// SetConsoleMode's ENABLE_VIRTUAL_TERMINAL_PROCESSING.
    const ESCAPES: u32 = 0x0004;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetConsoleMode(console: *mut c_void, mode: *mut u32) -> i32;
        fn SetConsoleMode(console: *mut c_void, mode: u32) -> i32;
    }
    let handles = [std::io::stdout().as_raw_handle(), std::io::stderr().as_raw_handle()];
    handles.into_iter().all(|handle| {
        let mut mode = 0_u32;
        // SAFETY: the handle is the process's standard stream's, which std
        // keeps open, or null or invalid where it has none, which the call
        // refuses; the mode is a u32 the call writes.
        if unsafe { GetConsoleMode(handle, &raw mut mode) } == 0 {
            return true;
        }
        // SAFETY: the same handle, a console now, and a mode the call reads.
        mode & ESCAPES != 0 || unsafe { SetConsoleMode(handle, mode | ESCAPES) } != 0
    })
}

/// Elsewhere a terminal reads them.
#[cfg(not(windows))]
pub(super) fn reads_escapes() -> bool {
    true
}
