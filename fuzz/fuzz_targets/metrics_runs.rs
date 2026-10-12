//! Any bytes read as a run's body under the head their first bytes give:
//! it gives as many samples as its head says or fails `corrupt`, and never
//! panics.

#![no_main]

libfuzzer_sys::fuzz_target!(|bytes: &[u8]| tinystore::metrics::fuzzing::runs(bytes));
