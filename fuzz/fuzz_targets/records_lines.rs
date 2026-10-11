//! Any text written through a writer of lines, cut anywhere: every line one
//! event holds comes back as it was written, and a longer one is dropped and
//! counted.

#![no_main]

use std::sync::OnceLock;

use tinystore::{Options, Store};

/// One store for the whole run, each input a stream of its own.
static STORE: OnceLock<Store> = OnceLock::new();

libfuzzer_sys::fuzz_target!(|bytes: &[u8]| {
    let store = STORE.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("tinystore-fuzz-lines-{}", std::process::id()));
        Store::open(dir, Options { background: false, ..Options::default() }).expect("a store in the temporary directory")
    });
    tinystore::records::fuzzing::lines(store, bytes);
});
