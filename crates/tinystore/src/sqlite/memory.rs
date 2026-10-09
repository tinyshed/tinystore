#![allow(unsafe_code)]

/// Bytes SQLite holds for every connection of the process, by its own count.
pub(crate) fn used() -> i64 {
    // SAFETY: sqlite3_memory_used takes no arguments and reads a counter that
    // SQLite keeps under its own mutex; any thread may call it at any time,
    // before or after SQLite initializes.
    unsafe { rusqlite::ffi::sqlite3_memory_used() }
}
