//! Any bytes read as a series' head, a group's clock and its directory,
//! each sealed as its row is: each gives what it holds or fails `corrupt`,
//! and none panics.

#![no_main]

libfuzzer_sys::fuzz_target!(|bytes: &[u8]| tinystore::metrics::fuzzing::rows(bytes));
