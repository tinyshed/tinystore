//! Any bytes read as the message of the schema their first two pick: each is
//! refused or read whole, and what was read writes within its size and reads
//! again as the same bytes, and none panics.

#![no_main]

libfuzzer_sys::fuzz_target!(|bytes: &[u8]| tinystore::wire_fuzzing::messages(bytes));
