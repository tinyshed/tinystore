//! Any bytes read as a head row, a segment's row and a block of records:
//! each gives its events or fails `corrupt`, and none panics.

#![no_main]

libfuzzer_sys::fuzz_target!(|bytes: &[u8]| tinystore::records::fuzzing::rows(bytes));
