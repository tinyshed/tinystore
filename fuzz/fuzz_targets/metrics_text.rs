//! Any bytes read as a text in Prometheus' format or OpenMetrics': its series
//! read again the same from the text they spell, or the text refused naming
//! its line, and never a panic.

#![no_main]

libfuzzer_sys::fuzz_target!(|bytes: &[u8]| tinystore::metrics::fuzzing::exposition(bytes));
