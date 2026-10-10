#![allow(unsafe_code)]

/// Bytes SQLite holds for every connection of the process, by its own count.
pub(crate) fn used() -> i64 {
    // SAFETY: sqlite3_memory_used takes no arguments and reads a counter that
    // SQLite keeps under its own mutex; any thread may call it at any time,
    // before or after SQLite initializes.
    unsafe { rusqlite::ffi::sqlite3_memory_used() }
}

/// Whether the SQLite linked in keeps one page cache for every connection of
/// the process, as libsqlite3-sys builds it unless told otherwise.
pub(crate) fn shares_page_cache() -> bool {
    // SAFETY: sqlite3_compileoption_used reads a table compiled into SQLite,
    // at any time and from any thread, and its argument is a C string that
    // lives for the call.
    unsafe { rusqlite::ffi::sqlite3_compileoption_used(c"ENABLE_MEMORY_MANAGEMENT".as_ptr()) != 0 }
}
